import { readFileSync, readdirSync } from "node:fs";
import { basename, join } from "node:path";
import { parse } from "yaml";
import { validateVariableName } from "./interpolate.js";
import type {
  CaptureValue,
  Environment,
  ExpectedTreeEntry,
  HttpAssertion,
  HttpRequestStep,
  HttpRoute,
  ProcessAssertion,
  RunStep,
  StartHttpStep,
  StartStep,
  StateAssertion,
  Step,
  StopStep,
  TestCase,
} from "./types.js";

type MapValue = Record<string, unknown>;

export function discoverTests(testsDir: string, filter?: string): TestCase[] {
  const tests = readdirSync(testsDir)
    .filter((name) => name.endsWith(".yaml") || name.endsWith(".yml"))
    .sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)))
    .map((name) => loadTest(join(testsDir, name)));
  if (!filter) return tests;
  return tests.filter((test) => basename(test.source).includes(filter) || test.name.includes(filter));
}

export function loadTest(path: string): TestCase {
  let parsed: unknown;
  try { parsed = parse(readFileSync(path, "utf8"), { uniqueKeys: true }); }
  catch (error) { throw new Error(`${path}: invalid YAML: ${(error as Error).message}`); }
  const root = mapping(parsed, path);
  exact(root, ["format", "name", "description", "timeout", "env", "steps"], path);
  if (root.format !== 1) throw new Error(`${path}.format: expected 1`);
  const steps = array(root.steps, `${path}.steps`);
  if (steps.length === 0) throw new Error(`${path}.steps: must not be empty`);
  return {
    source: path,
    format: 1,
    name: string(root.name, `${path}.name`),
    ...(root.description === undefined ? {} : { description: string(root.description, `${path}.description`) }),
    ...(root.timeout === undefined ? {} : { timeout: positive(root.timeout, `${path}.timeout`) }),
    ...(root.env === undefined ? {} : { env: environment(root.env, `${path}.env`) }),
    steps: steps.map((step, index) => parseStep(step, `${path}.steps[${index}]`)),
  };
}

function parseStep(value: unknown, location: string): Step {
  const outer = mapping(value, location);
  if (Object.keys(outer).length !== 1) throw new Error(`${location}: expected exactly one operation key`);
  const [tag, payload] = Object.entries(outer)[0];
  switch (tag) {
    case "mkdir": return { type: "mkdir", path: pathPayload(payload, location) };
    case "remove": return { type: "remove", path: pathPayload(payload, location) };
    case "fifo": return { type: "fifo", path: pathPayload(payload, location) };
    case "write_file": {
      const map = mapping(payload, `${location}.write_file`);
      exact(map, ["path", "text", "base64"], `${location}.write_file`);
      exclusive(map, "text", "base64", `${location}.write_file`);
      const base = { type: "write_file" as const, path: string(map.path, `${location}.write_file.path`) };
      return map.text !== undefined
        ? { ...base, text: string(map.text, `${location}.write_file.text`) }
        : { ...base, base64: base64Template(map.base64, `${location}.write_file.base64`) };
    }
    case "copy_tree": {
      const map = mapping(payload, `${location}.copy_tree`);
      exact(map, ["from", "to"], `${location}.copy_tree`);
      return { type: "copy_tree", from: string(map.from, `${location}.copy_tree.from`), to: string(map.to, `${location}.copy_tree.to`) };
    }
    case "symlink": {
      const map = mapping(payload, `${location}.symlink`);
      exact(map, ["path", "target"], `${location}.symlink`);
      return { type: "symlink", path: string(map.path, `${location}.symlink.path`), target: string(map.target, `${location}.symlink.target`) };
    }
    case "run": return { type: "run", value: runStep(payload, `${location}.run`) };
    case "start": return { type: "start", value: startStep(payload, `${location}.start`) };
    case "stop": return { type: "stop", value: stopStep(payload, `${location}.stop`) };
    case "start_http": return { type: "start_http", value: startHttpStep(payload, `${location}.start_http`) };
    case "stop_http": {
      const map = mapping(payload, `${location}.stop_http`);
      exact(map, ["id"], `${location}.stop_http`);
      return { type: "stop_http", id: identifier(map.id, `${location}.stop_http.id`) };
    }
    case "http_request": return { type: "http_request", value: httpRequestStep(payload, `${location}.http_request`) };
    case "assert": {
      const values = array(payload, `${location}.assert`);
      if (values.length === 0) throw new Error(`${location}.assert: must not be empty`);
      return { type: "assert", assertions: values.map((item, i) => stateAssertion(item, `${location}.assert[${i}]`)) };
    }
    default: throw new Error(`${location}: unknown operation: ${tag}`);
  }
}

function runStep(value: unknown, location: string): RunStep {
  const map = mapping(value, location);
  exact(map, ["cwd", "args", "stdin", "timeout", "env", "capture", "expect"], location);
  const expect = processAssertions(map.expect, `${location}.expect`, true);
  return {
    ...(map.cwd === undefined ? {} : { cwd: string(map.cwd, `${location}.cwd`) }),
    ...(map.args === undefined ? {} : { args: strings(map.args, `${location}.args`) }),
    ...(map.stdin === undefined ? {} : { stdin: string(map.stdin, `${location}.stdin`) }),
    ...(map.timeout === undefined ? {} : { timeout: positive(map.timeout, `${location}.timeout`) }),
    ...(map.env === undefined ? {} : { env: environment(map.env, `${location}.env`) }),
    ...(map.capture === undefined ? {} : { capture: captureMap(map.capture, `${location}.capture`) }),
    expect,
  };
}

function startStep(value: unknown, location: string): StartStep {
  const map = mapping(value, location);
  exact(map, ["id", "cwd", "args", "stdin", "timeout", "env", "ready"], location);
  const ready = mapping(map.ready, `${location}.ready`);
  exact(ready, ["stream", "pattern", "capture"], `${location}.ready`);
  const stream = enumValue(ready.stream, ["stdout", "stderr"] as const, `${location}.ready.stream`);
  const pattern = regex(ready.pattern, `${location}.ready.pattern`);
  let capture: StartStep["ready"]["capture"];
  if (ready.capture !== undefined) {
    const captureMap = mapping(ready.capture, `${location}.ready.capture`);
    exact(captureMap, ["as", "group"], `${location}.ready.capture`);
    const as = identifier(captureMap.as, `${location}.ready.capture.as`);
    validateVariableName(as, `${location}.ready.capture.as`);
    capture = { as, ...(captureMap.group === undefined ? {} : { group: nonnegative(captureMap.group, `${location}.ready.capture.group`) }) };
  }
  return {
    id: identifier(map.id, `${location}.id`),
    ...(map.cwd === undefined ? {} : { cwd: string(map.cwd, `${location}.cwd`) }),
    ...(map.args === undefined ? {} : { args: strings(map.args, `${location}.args`) }),
    ...(map.stdin === undefined ? {} : { stdin: string(map.stdin, `${location}.stdin`) }),
    ...(map.timeout === undefined ? {} : { timeout: positive(map.timeout, `${location}.timeout`) }),
    ...(map.env === undefined ? {} : { env: environment(map.env, `${location}.env`) }),
    ready: { stream, pattern, ...(capture === undefined ? {} : { capture }) },
  };
}

function stopStep(value: unknown, location: string): StopStep {
  const map = mapping(value, location);
  exact(map, ["id", "signal", "timeout", "expect"], location);
  return {
    id: identifier(map.id, `${location}.id`),
    ...(map.signal === undefined ? {} : { signal: enumValue(map.signal, ["SIGTERM", "SIGINT"] as const, `${location}.signal`) }),
    ...(map.timeout === undefined ? {} : { timeout: positive(map.timeout, `${location}.timeout`) }),
    expect: processAssertions(map.expect, `${location}.expect`, true),
  };
}

function startHttpStep(value: unknown, location: string): StartHttpStep {
  const map = mapping(value, location);
  exact(map, ["id", "capture_url", "routes"], location);
  const captureUrl = identifier(map.capture_url, `${location}.capture_url`);
  validateVariableName(captureUrl, `${location}.capture_url`);
  return {
    id: identifier(map.id, `${location}.id`),
    capture_url: captureUrl,
    routes: array(map.routes, `${location}.routes`).map((route, i) => httpRoute(route, `${location}.routes[${i}]`)),
  };
}

function httpRoute(value: unknown, location: string): HttpRoute {
  const map = mapping(value, location);
  exact(map, ["method", "target", "status", "headers", "text", "base64"], location);
  if (map.text !== undefined && map.base64 !== undefined) throw new Error(`${location}: text and base64 are mutually exclusive`);
  return {
    method: method(map.method, `${location}.method`),
    target: string(map.target, `${location}.target`),
    status: integerRange(map.status, 100, 599, `${location}.status`),
    ...(map.headers === undefined ? {} : { headers: stringMap(map.headers, `${location}.headers`) }),
    ...(map.text === undefined ? {} : { text: string(map.text, `${location}.text`) }),
    ...(map.base64 === undefined ? {} : { base64: base64Template(map.base64, `${location}.base64`) }),
  };
}

function httpRequestStep(value: unknown, location: string): HttpRequestStep {
  const map = mapping(value, location);
  exact(map, ["method", "url", "headers", "timeout", "expect"], location);
  const expects = array(map.expect, `${location}.expect`);
  if (expects.length === 0) throw new Error(`${location}.expect: must not be empty`);
  return {
    method: method(map.method, `${location}.method`),
    url: string(map.url, `${location}.url`),
    ...(map.headers === undefined ? {} : { headers: stringMap(map.headers, `${location}.headers`) }),
    ...(map.timeout === undefined ? {} : { timeout: positive(map.timeout, `${location}.timeout`) }),
    expect: expects.map((item, i) => httpAssertion(item, `${location}.expect[${i}]`)),
  };
}

function processAssertions(value: unknown, location: string, requireExit: boolean): ProcessAssertion[] {
  const values = array(value, location);
  if (values.length === 0) throw new Error(`${location}: must not be empty`);
  const assertions = values.map((item, index) => processAssertion(item, `${location}[${index}]`));
  const exits = assertions.filter((assertion) => assertion.type === "exit_code").length;
  if (requireExit && exits !== 1) throw new Error(`${location}: requires exactly one exit_code assertion`);
  return assertions;
}

function processAssertion(value: unknown, location: string): ProcessAssertion {
  const map = mapping(value, location);
  const type = string(map.type, `${location}.type`);
  if (type === "exit_code") {
    exact(map, ["type", "value"], location);
    return { type, value: integerRange(map.value, 0, 255, `${location}.value`) };
  }
  if (["stdout_equals", "stdout_contains", "stderr_equals", "stderr_contains"].includes(type)) {
    exact(map, ["type", "value"], location);
    const value = string(map.value, `${location}.value`);
    switch (type) {
      case "stdout_equals": return { type, value };
      case "stdout_contains": return { type, value };
      case "stderr_equals": return { type, value };
      case "stderr_contains": return { type, value };
    }
  }
  if (type === "stdout_matches" || type === "stderr_matches") {
    exact(map, ["type", "pattern"], location);
    return { type, pattern: regex(map.pattern, `${location}.pattern`) };
  }
  throw new Error(`${location}.type: unknown process assertion: ${type}`);
}

function httpAssertion(value: unknown, location: string): HttpAssertion {
  const map = mapping(value, location);
  const type = string(map.type, `${location}.type`);
  if (type === "status") {
    exact(map, ["type", "value"], location);
    return { type, value: integerRange(map.value, 100, 599, `${location}.value`) };
  }
  if (type === "header_equals") {
    exact(map, ["type", "name", "value"], location);
    return { type, name: string(map.name, `${location}.name`), value: string(map.value, `${location}.value`) };
  }
  if (type === "body_text_equals" || type === "body_base64_equals") {
    exact(map, ["type", "value"], location);
    return { type, value: type === "body_base64_equals" ? base64Template(map.value, `${location}.value`) : string(map.value, `${location}.value`) };
  }
  if (type === "body_json_equals") {
    exact(map, ["type", "value"], location);
    return { type, value: map.value };
  }
  throw new Error(`${location}.type: unknown HTTP assertion: ${type}`);
}

function stateAssertion(value: unknown, location: string): StateAssertion {
  const map = mapping(value, location);
  const type = string(map.type, `${location}.type`);
  switch (type) {
    case "tree_equals": {
      exact(map, ["type", "path", "entries"], location);
      const entries = array(map.entries, `${location}.entries`).map((entry, i) => treeEntry(entry, `${location}.entries[${i}]`));
      const sorted = [...entries].sort((a, b) => Buffer.compare(Buffer.from(a.path), Buffer.from(b.path)));
      if (!isSameOrder(entries, sorted)) throw new Error(`${location}.entries: must be in unsigned UTF-8 path order`);
      return { type, path: string(map.path, `${location}.path`), entries };
    }
    case "file_text_equals":
    case "file_base64_equals": {
      exact(map, ["type", "path", "value"], location);
      return { type, path: string(map.path, `${location}.path`), value: type === "file_base64_equals" ? base64Template(map.value, `${location}.value`) : string(map.value, `${location}.value`) };
    }
    case "json_equals":
      exact(map, ["type", "path", "value"], location);
      return { type, path: string(map.path, `${location}.path`), value: map.value };
    case "path_exists":
      exact(map, ["type", "path", "kind"], location);
      return { type, path: string(map.path, `${location}.path`), ...(map.kind === undefined ? {} : { kind: enumValue(map.kind, ["file", "directory", "symlink", "fifo", "other"] as const, `${location}.kind`) }) };
    case "path_not_exists":
      exact(map, ["type", "path"], location);
      return { type, path: string(map.path, `${location}.path`) };
    case "trees_equal":
      exact(map, ["type", "left", "right", "ignore"], location);
      return { type, left: string(map.left, `${location}.left`), right: string(map.right, `${location}.right`), ...(map.ignore === undefined ? {} : { ignore: strings(map.ignore, `${location}.ignore`) }) };
    case "http_requests_equal": {
      exact(map, ["type", "server", "value"], location);
      const requests = array(map.value, `${location}.value`).map((item, i) => {
        const request = mapping(item, `${location}.value[${i}]`);
        exact(request, ["method", "target"], `${location}.value[${i}]`);
        return { method: method(request.method, `${location}.value[${i}].method`), target: string(request.target, `${location}.value[${i}].target`) };
      });
      return { type, server: identifier(map.server, `${location}.server`), value: requests };
    }
    default: throw new Error(`${location}.type: unknown state assertion: ${type}`);
  }
}

function treeEntry(value: unknown, location: string): ExpectedTreeEntry {
  const map = mapping(value, location);
  exact(map, ["path", "kind", "target"], location);
  const kind = enumValue(map.kind, ["file", "directory", "symlink", "fifo", "other"] as const, `${location}.kind`);
  if (kind === "symlink" && map.target === undefined) throw new Error(`${location}.target: required for symlink`);
  if (kind !== "symlink" && map.target !== undefined) throw new Error(`${location}.target: only valid for symlink`);
  return { path: string(map.path, `${location}.path`), kind, ...(map.target === undefined ? {} : { target: string(map.target, `${location}.target`) }) };
}

function captureMap(value: unknown, location: string): RunStep["capture"] {
  const map = mapping(value, location);
  exact(map, ["stdout", "stderr"], location);
  if (map.stdout === undefined && map.stderr === undefined) throw new Error(`${location}: must capture stdout or stderr`);
  return {
    ...(map.stdout === undefined ? {} : { stdout: captureValue(map.stdout, `${location}.stdout`) }),
    ...(map.stderr === undefined ? {} : { stderr: captureValue(map.stderr, `${location}.stderr`) }),
  };
}

function captureValue(value: unknown, location: string): CaptureValue {
  const map = mapping(value, location);
  exact(map, ["as", "trim"], location);
  const as = identifier(map.as, `${location}.as`);
  validateVariableName(as, `${location}.as`);
  return { as, ...(map.trim === undefined ? {} : { trim: boolean(map.trim, `${location}.trim`) }) };
}

function pathPayload(value: unknown, location: string): string {
  const map = mapping(value, location);
  exact(map, ["path"], location);
  return string(map.path, `${location}.path`);
}

function environment(value: unknown, location: string): Environment {
  const map = mapping(value, location);
  return Object.fromEntries(Object.entries(map).map(([key, item]) => {
    if (item !== null && typeof item !== "string") throw new Error(`${location}.${key}: expected string or null`);
    return [key, item as string | null];
  }));
}

function stringMap(value: unknown, location: string): Record<string, string> {
  const map = mapping(value, location);
  return Object.fromEntries(Object.entries(map).map(([key, item]) => [key, string(item, `${location}.${key}`)]));
}

function exclusive(map: MapValue, a: string, b: string, location: string): void {
  if ((map[a] === undefined) === (map[b] === undefined)) throw new Error(`${location}: exactly one of ${a} and ${b} is required`);
}

function exact(map: MapValue, allowed: string[], location: string): void {
  const unknown = Object.keys(map).filter((key) => !allowed.includes(key));
  if (unknown.length > 0) throw new Error(`${location}: unknown field(s): ${unknown.join(", ")}`);
}

function mapping(value: unknown, location: string): MapValue {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error(`${location}: expected mapping`);
  return value as MapValue;
}

function array(value: unknown, location: string): unknown[] {
  if (!Array.isArray(value)) throw new Error(`${location}: expected array`);
  return value;
}

function string(value: unknown, location: string): string {
  if (typeof value !== "string") throw new Error(`${location}: expected string`);
  return value;
}

function strings(value: unknown, location: string): string[] {
  return array(value, location).map((item, i) => string(item, `${location}[${i}]`));
}

function boolean(value: unknown, location: string): boolean {
  if (typeof value !== "boolean") throw new Error(`${location}: expected boolean`);
  return value;
}

function positive(value: unknown, location: string): number { return integerRange(value, 1, 86_400, location); }
function nonnegative(value: unknown, location: string): number { return integerRange(value, 0, 100, location); }

function integerRange(value: unknown, min: number, max: number, location: string): number {
  if (!Number.isInteger(value) || (value as number) < min || (value as number) > max) {
    throw new Error(`${location}: expected integer ${min}..${max}`);
  }
  return value as number;
}

function identifier(value: unknown, location: string): string {
  const result = string(value, location);
  if (!/^[A-Za-z_][A-Za-z0-9_-]*$/.test(result)) throw new Error(`${location}: invalid identifier: ${result}`);
  return result;
}

function method(value: unknown, location: string): string {
  const result = string(value, location);
  if (!/^[A-Z]+$/.test(result)) throw new Error(`${location}: expected uppercase HTTP method`);
  return result;
}

function regex(value: unknown, location: string): string {
  const result = string(value, location);
  try { new RegExp(result, "m"); } catch (error) { throw new Error(`${location}: invalid regex: ${(error as Error).message}`); }
  return result;
}

function base64(value: unknown, location: string): string {
  const result = string(value, location);
  if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(result)) {
    throw new Error(`${location}: expected canonical padded base64`);
  }
  return result;
}

function base64Template(value: unknown, location: string): string {
  const result = string(value, location);
  return result.includes("{{") || result.includes("}}") ? result : base64(result, location);
}

function enumValue<const T extends readonly string[]>(value: unknown, allowed: T, location: string): T[number] {
  const result = string(value, location);
  if (!allowed.includes(result)) throw new Error(`${location}: expected ${allowed.join(" | ")}`);
  return result as T[number];
}

function isSameOrder<T>(left: T[], right: T[]): boolean {
  return left.every((item, index) => item === right[index]);
}
