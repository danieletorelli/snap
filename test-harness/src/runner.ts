import { rmSync } from "node:fs";
import type {
  Environment,
  HttpAssertion,
  HttpRoute,
  ProcessAssertion,
  StateAssertion,
  Step,
  StepResult,
  TestCase,
  TestResult,
} from "./types.js";
import {
  copyTree,
  createSandbox,
  fifoFixture,
  mkdirFixture,
  removeFixture,
  sandboxDirectory,
  symlinkFixture,
  writeFixture,
} from "./filesystem.js";
import { interpolate, interpolateJson } from "./interpolate.js";
import {
  applyEnvironment,
  cleanupProcess,
  deterministicEnvironment,
  runProcess,
  startProcess,
  stopProcess,
  type ManagedProcess,
} from "./process.js";
import {
  performHttpRequest,
  startControlledServer,
  stopControlledServer,
  type ControlledServer,
} from "./http-server.js";
import { checkHttpAssertions, checkProcessAssertions, checkStateAssertions } from "./assertions.js";

export interface RunConfig {
  candidate: string;
  keepFailed?: boolean;
}

export async function runCase(test: TestCase, config: RunConfig): Promise<TestResult> {
  const started = Date.now();
  const sandbox = createSandbox();
  const variables = new Map<string, string>([["sandbox", sandbox], ["candidate", config.candidate]]);
  const processes = new Map<string, ManagedProcess>();
  const servers = new Map<string, ControlledServer>();
  const steps: StepResult[] = [];
  const deadline = started + (test.timeout ?? 30) * 1000;
  let error: string | undefined;

  try {
    const baseEnv = deterministicEnvironment(sandbox, interpolateEnvironment(test.env, variables));
    for (const [index, step] of test.steps.entries()) {
      if (Date.now() >= deadline) throw new Error(`case timed out after ${test.timeout ?? 30}s`);
      try {
        const result = await executeStep(step, index, sandbox, variables, processes, servers, baseEnv, config.candidate, deadline);
        steps.push(result);
        if (!result.passed) break;
      } catch (cause) {
        error = (cause as Error).message;
        steps.push({ index, label: label(step), passed: false, failures: [error] });
        break;
      }
    }
  } catch (cause) {
    error = (cause as Error).message;
  } finally {
    for (const process of processes.values()) cleanupProcess(process);
    await Promise.allSettled([...processes.values()].map((process) => process.completion));
    await Promise.allSettled([...servers.values()].map(stopControlledServer));
  }

  const passed = error === undefined && steps.length === test.steps.length && steps.every((step) => step.passed);
  const preserve = !passed && config.keepFailed === true;
  if (!preserve) rmSync(sandbox, { recursive: true, force: true });
  return {
    source: test.source,
    name: test.name,
    passed,
    durationMs: Date.now() - started,
    steps,
    ...(error === undefined ? {} : { error }),
    ...(preserve ? { sandbox } : {}),
  };
}

async function executeStep(
  step: Step,
  index: number,
  sandbox: string,
  variables: Map<string, string>,
  processes: Map<string, ManagedProcess>,
  servers: Map<string, ControlledServer>,
  baseEnv: NodeJS.ProcessEnv,
  candidate: string,
  deadline: number,
): Promise<StepResult> {
  const result: StepResult = { index, label: label(step), passed: true, failures: [] };
  switch (step.type) {
    case "mkdir": mkdirFixture(sandbox, interpolate(step.path, variables)); break;
    case "write_file": {
      const path = interpolate(step.path, variables);
      const bytes = step.text !== undefined
        ? Buffer.from(interpolate(step.text, variables))
        : decodeBase64(interpolate(step.base64!, variables));
      writeFixture(sandbox, path, bytes);
      break;
    }
    case "copy_tree":
      copyTree(sandbox, interpolate(step.from, variables), interpolate(step.to, variables));
      break;
    case "remove": removeFixture(sandbox, interpolate(step.path, variables)); break;
    case "symlink":
      symlinkFixture(sandbox, interpolate(step.path, variables), interpolate(step.target, variables));
      break;
    case "fifo": fifoFixture(sandbox, interpolate(step.path, variables)); break;
    case "run": {
      const value = step.value;
      const process = await runProcess({
        candidate,
        args: (value.args ?? []).map((arg) => interpolate(arg, variables)),
        cwd: sandboxDirectory(sandbox, interpolate(value.cwd ?? ".", variables)),
        env: applyEnvironment(baseEnv, interpolateEnvironment(value.env, variables)),
        stdin: interpolate(value.stdin ?? "", variables),
        timeoutMs: stepTimeout(value.timeout, deadline),
      });
      const assertions = interpolateProcessAssertions(value.expect, variables);
      result.failures.push(...checkProcessAssertions(assertions, process));
      result.process = process;
      if (result.failures.length === 0) {
        if (value.capture?.stdout) capture(value.capture.stdout.as, value.capture.stdout.trim ? process.stdout.trim() : process.stdout, variables);
        if (value.capture?.stderr) capture(value.capture.stderr.as, value.capture.stderr.trim ? process.stderr.trim() : process.stderr, variables);
      }
      break;
    }
    case "start": {
      if (processes.has(step.value.id)) throw new Error(`duplicate background process id: ${step.value.id}`);
      const ready = { ...step.value.ready, pattern: interpolate(step.value.ready.pattern, variables) };
      const started = await startProcess({
        candidate,
        args: (step.value.args ?? []).map((arg) => interpolate(arg, variables)),
        cwd: sandboxDirectory(sandbox, interpolate(step.value.cwd ?? ".", variables)),
        env: applyEnvironment(baseEnv, interpolateEnvironment(step.value.env, variables)),
        stdin: interpolate(step.value.stdin ?? "", variables),
      }, ready, stepTimeout(step.value.timeout, deadline));
      processes.set(step.value.id, started.managed);
      if (ready.capture) {
        const group = ready.capture.group ?? 0;
        const captured = started.match[group];
        if (captured === undefined) throw new Error(`ready capture group ${group} did not participate`);
        capture(ready.capture.as, captured, variables);
      }
      break;
    }
    case "stop": {
      const process = processes.get(step.value.id);
      if (!process) throw new Error(`unknown background process: ${step.value.id}`);
      const stopped = await stopProcess(process, step.value.signal ?? "SIGTERM", stepTimeout(step.value.timeout, deadline));
      processes.delete(step.value.id);
      result.failures.push(...checkProcessAssertions(interpolateProcessAssertions(step.value.expect, variables), stopped));
      result.process = stopped;
      break;
    }
    case "start_http": {
      if (servers.has(step.value.id)) throw new Error(`duplicate controlled HTTP server id: ${step.value.id}`);
      const routes = step.value.routes.map((route) => interpolateRoute(route, variables));
      const server = await startControlledServer(routes);
      servers.set(step.value.id, server);
      capture(step.value.capture_url, server.url, variables);
      break;
    }
    case "stop_http": {
      const server = servers.get(step.id);
      if (!server) throw new Error(`unknown controlled HTTP server: ${step.id}`);
      await stopControlledServer(server);
      servers.delete(step.id);
      break;
    }
    case "http_request": {
      const value = step.value;
      const response = await performHttpRequest(
        value.method,
        interpolate(value.url, variables),
        interpolateStringMap(value.headers ?? {}, variables),
        stepTimeout(value.timeout, deadline),
      );
      result.failures.push(...checkHttpAssertions(interpolateHttpAssertions(value.expect, variables), response));
      break;
    }
    case "assert": result.failures.push(...checkStateAssertions(step.assertions, sandbox, variables, servers)); break;
  }
  result.passed = result.failures.length === 0;
  return result;
}

function interpolateEnvironment(env: Environment | undefined, variables: ReadonlyMap<string, string>): Environment | undefined {
  if (env === undefined) return undefined;
  return Object.fromEntries(Object.entries(env).map(([key, value]) => [key, value === null ? null : interpolate(value, variables)]));
}

function interpolateStringMap(values: Record<string, string>, variables: ReadonlyMap<string, string>): Record<string, string> {
  return Object.fromEntries(Object.entries(values).map(([key, value]) => [key, interpolate(value, variables)]));
}

function interpolateProcessAssertions(values: ProcessAssertion[], variables: ReadonlyMap<string, string>): ProcessAssertion[] {
  return values.map((assertion) => {
    switch (assertion.type) {
      case "exit_code": return assertion;
      case "stdout_matches":
      case "stderr_matches": return { ...assertion, pattern: interpolate(assertion.pattern, variables) };
      default: return { ...assertion, value: interpolate(assertion.value, variables) };
    }
  });
}

function interpolateHttpAssertions(values: HttpAssertion[], variables: ReadonlyMap<string, string>): HttpAssertion[] {
  return values.map((assertion) => {
    switch (assertion.type) {
      case "status": return assertion;
      case "header_equals": return { ...assertion, value: interpolate(assertion.value, variables) };
      case "body_json_equals": return { ...assertion, value: interpolateJson(assertion.value, variables) };
      default: return { ...assertion, value: interpolate(assertion.value, variables) };
    }
  });
}

function interpolateRoute(route: HttpRoute, variables: ReadonlyMap<string, string>): HttpRoute {
  return {
    ...route,
    target: interpolate(route.target, variables),
    ...(route.headers === undefined ? {} : { headers: interpolateStringMap(route.headers, variables) }),
    ...(route.text === undefined ? {} : { text: interpolate(route.text, variables) }),
    ...(route.base64 === undefined ? {} : { base64: interpolate(route.base64, variables) }),
  };
}

function capture(name: string, value: string, variables: Map<string, string>): void {
  if (variables.has(name)) throw new Error(`variable already defined: ${name}`);
  variables.set(name, value);
}

function decodeBase64(value: string): Buffer {
  if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)) {
    throw new Error("interpolated value is not canonical padded base64");
  }
  return Buffer.from(value, "base64");
}

function stepTimeout(seconds: number | undefined, deadline: number): number {
  const remaining = deadline - Date.now();
  if (remaining <= 0) throw new Error("case timeout exceeded");
  return Math.min(seconds === undefined ? remaining : seconds * 1000, remaining);
}

function label(step: Step): string {
  if (step.type === "start" || step.type === "stop") return `${step.type} ${step.value.id}`;
  if (step.type === "start_http") return `start_http ${step.value.id}`;
  if (step.type === "stop_http") return `stop_http ${step.id}`;
  return step.type;
}
