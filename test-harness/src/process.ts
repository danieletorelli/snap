import { spawn, type ChildProcess } from "node:child_process";
import type { Environment, ProcessResult, StartStep } from "./types.js";

const STREAM_LIMIT = 16 * 1024 * 1024;

export interface ProcessOptions {
  candidate: string;
  args: string[];
  cwd: string;
  env: NodeJS.ProcessEnv;
  stdin: string;
  timeoutMs: number;
}

export interface ManagedProcess {
  child: ChildProcess;
  completion: Promise<ProcessResult>;
  output: OutputCollector;
}

export function deterministicEnvironment(root: string, changes?: Environment): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {
    PATH: process.env.PATH ?? "/usr/bin:/bin",
    HOME: `${root}/home`,
    TMPDIR: `${root}/tmp`,
    NO_COLOR: "1",
    LANG: "C",
    LC_ALL: "C",
    NO_PROXY: "127.0.0.1,localhost",
  };
  return applyEnvironment(env, changes);
}

export function applyEnvironment(base: NodeJS.ProcessEnv, changes?: Environment): NodeJS.ProcessEnv {
  const env = { ...base };
  for (const [key, value] of Object.entries(changes ?? {})) {
    if (value === null) delete env[key];
    else env[key] = value;
  }
  return env;
}

export async function runProcess(options: ProcessOptions): Promise<ProcessResult> {
  const managed = launch(options);
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    killGroup(managed.child, "SIGKILL");
  }, options.timeoutMs);
  try {
    const result = await managed.completion;
    if (timedOut) throw new Error(`process did not exit within ${options.timeoutMs}ms`);
    return result;
  } finally {
    clearTimeout(timer);
  }
}

export async function startProcess(
  options: Omit<ProcessOptions, "timeoutMs">,
  ready: StartStep["ready"],
  timeoutMs: number,
): Promise<{ managed: ManagedProcess; match: RegExpMatchArray }> {
  const managed = launch({ ...options, timeoutMs });
  const regex = new RegExp(ready.pattern, "m");
  try {
    return await new Promise((resolve, reject) => {
      let settled = false;
      const finish = (error?: Error, match?: RegExpMatchArray) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        managed.output.changed.delete(check);
        if (error) {
          cleanupProcess(managed);
          reject(error);
        }
        else resolve({ managed, match: match! });
      };
      const check = () => {
        try {
          const text = ready.stream === "stdout" ? managed.output.stdoutText() : managed.output.stderrText();
          const match = text.match(regex);
          if (match) finish(undefined, match);
        } catch (error) {
          finish(error as Error);
        }
      };
      const timer = setTimeout(() => {
        killGroup(managed.child, "SIGKILL");
        finish(new Error(`background process did not become ready within ${timeoutMs}ms`));
      }, timeoutMs);
      managed.output.changed.add(check);
      managed.completion.then(
        (result) => finish(new Error(`background process exited before ready (exit ${result.exitCode})`)),
        (error) => finish(error as Error),
      );
      check();
    });
  } catch (error) {
    cleanupProcess(managed);
    await managed.completion.catch(() => undefined);
    throw error;
  }
}

export async function stopProcess(
  managed: ManagedProcess,
  signal: NodeJS.Signals,
  timeoutMs: number,
): Promise<ProcessResult> {
  killGroup(managed.child, signal);
  let timer: NodeJS.Timeout | undefined;
  try {
    return await Promise.race([
      managed.completion,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => {
          killGroup(managed.child, "SIGKILL");
          reject(new Error(`background process did not stop within ${timeoutMs}ms`));
        }, timeoutMs);
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

export function cleanupProcess(managed: ManagedProcess): void {
  if (managed.child.exitCode === null && managed.child.signalCode === null) {
    killGroup(managed.child, "SIGKILL");
  }
}

function launch(options: ProcessOptions): ManagedProcess {
  const child = spawn(options.candidate, options.args, {
    cwd: options.cwd,
    env: options.env,
    detached: true,
    stdio: ["pipe", "pipe", "pipe"],
  });
  const output = new OutputCollector(child);
  const completion = new Promise<ProcessResult>((resolve, reject) => {
    child.once("error", reject);
    child.stdin!.on("error", (error: NodeJS.ErrnoException) => {
      if (error.code !== "EPIPE") reject(error);
    });
    child.once("close", (exitCode, signal) => {
      try {
        output.finish();
        resolve({
          stdout: output.stdoutText(),
          stderr: output.stderrText(),
          exitCode,
          signal,
        });
      } catch (error) {
        reject(error);
      }
    });
  });
  completion.catch(() => killGroup(child, "SIGKILL"));
  child.stdin!.end(options.stdin);
  return { child, completion, output };
}

export class OutputCollector {
  readonly changed = new Set<() => void>();
  private readonly stdout: Buffer[] = [];
  private readonly stderr: Buffer[] = [];
  private stdoutSize = 0;
  private stderrSize = 0;
  private overflow?: Error;

  constructor(child: ChildProcess) {
    child.stdout!.on("data", (chunk: Buffer) => this.add("stdout", chunk, child));
    child.stderr!.on("data", (chunk: Buffer) => this.add("stderr", chunk, child));
  }

  stdoutText(): string { return decode(Buffer.concat(this.stdout)); }
  stderrText(): string { return decode(Buffer.concat(this.stderr)); }
  finish(): void { if (this.overflow) throw this.overflow; }

  private add(stream: "stdout" | "stderr", chunk: Buffer, child: ChildProcess): void {
    const next = (stream === "stdout" ? this.stdoutSize : this.stderrSize) + chunk.length;
    if (next > STREAM_LIMIT) {
      this.overflow = new Error(`${stream} exceeded 16 MiB limit`);
      killGroup(child, "SIGKILL");
      return;
    }
    if (stream === "stdout") { this.stdout.push(chunk); this.stdoutSize = next; }
    else { this.stderr.push(chunk); this.stderrSize = next; }
    for (const callback of this.changed) callback();
  }
}

function decode(buffer: Buffer): string {
  return new TextDecoder("utf-8", { fatal: true }).decode(buffer);
}

function killGroup(child: ChildProcess, signal: NodeJS.Signals): void {
  if (child.pid === undefined || child.exitCode !== null || child.signalCode !== null) return;
  try { process.kill(-child.pid, signal); } catch { try { child.kill(signal); } catch { /* exited */ } }
}
