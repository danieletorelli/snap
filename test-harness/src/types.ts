export type Environment = Record<string, string | null>;
export type EntryKind = "file" | "directory" | "symlink" | "fifo" | "other";

export interface CaptureValue {
  as: string;
  trim?: boolean;
}

export type ProcessAssertion =
  | { type: "exit_code"; value: number }
  | { type: "stdout_equals"; value: string }
  | { type: "stdout_contains"; value: string }
  | { type: "stdout_matches"; pattern: string }
  | { type: "stderr_equals"; value: string }
  | { type: "stderr_contains"; value: string }
  | { type: "stderr_matches"; pattern: string };

export type HttpAssertion =
  | { type: "status"; value: number }
  | { type: "header_equals"; name: string; value: string }
  | { type: "body_text_equals"; value: string }
  | { type: "body_base64_equals"; value: string }
  | { type: "body_json_equals"; value: unknown };

export interface ExpectedTreeEntry {
  path: string;
  kind: EntryKind;
  target?: string;
}

export interface ExpectedHttpRequest {
  method: string;
  target: string;
}

export type StateAssertion =
  | { type: "tree_equals"; path: string; entries: ExpectedTreeEntry[] }
  | { type: "file_text_equals"; path: string; value: string }
  | { type: "file_base64_equals"; path: string; value: string }
  | { type: "json_equals"; path: string; value: unknown }
  | { type: "path_exists"; path: string; kind?: EntryKind }
  | { type: "path_not_exists"; path: string }
  | { type: "trees_equal"; left: string; right: string; ignore?: string[] }
  | { type: "http_requests_equal"; server: string; value: ExpectedHttpRequest[] };

export interface RunStep {
  cwd?: string;
  args?: string[];
  stdin?: string;
  timeout?: number;
  env?: Environment;
  capture?: { stdout?: CaptureValue; stderr?: CaptureValue };
  expect: ProcessAssertion[];
}

export interface StartStep {
  id: string;
  cwd?: string;
  args?: string[];
  stdin?: string;
  env?: Environment;
  timeout?: number;
  ready: {
    stream: "stdout" | "stderr";
    pattern: string;
    capture?: { as: string; group?: number };
  };
}

export interface StopStep {
  id: string;
  signal?: "SIGTERM" | "SIGINT";
  timeout?: number;
  expect: ProcessAssertion[];
}

export interface HttpRoute {
  method: string;
  target: string;
  status: number;
  headers?: Record<string, string>;
  text?: string;
  base64?: string;
}

export interface StartHttpStep {
  id: string;
  capture_url: string;
  routes: HttpRoute[];
}

export interface HttpRequestStep {
  method: string;
  url: string;
  headers?: Record<string, string>;
  timeout?: number;
  expect: HttpAssertion[];
}

export type Step =
  | { type: "mkdir"; path: string }
  | { type: "write_file"; path: string; text?: string; base64?: string }
  | { type: "copy_tree"; from: string; to: string }
  | { type: "remove"; path: string }
  | { type: "symlink"; path: string; target: string }
  | { type: "fifo"; path: string }
  | { type: "run"; value: RunStep }
  | { type: "start"; value: StartStep }
  | { type: "stop"; value: StopStep }
  | { type: "start_http"; value: StartHttpStep }
  | { type: "stop_http"; id: string }
  | { type: "http_request"; value: HttpRequestStep }
  | { type: "assert"; assertions: StateAssertion[] };

export interface TestCase {
  source: string;
  format: 1;
  name: string;
  description?: string;
  timeout?: number;
  env?: Environment;
  steps: Step[];
}

export interface ProcessResult {
  stdout: string;
  stderr: string;
  exitCode: number | null;
  signal: NodeJS.Signals | null;
}

export interface HttpResult {
  status: number;
  headers: Record<string, string>;
  body: Buffer;
}

export interface StepResult {
  index: number;
  label: string;
  passed: boolean;
  failures: string[];
  process?: ProcessResult;
}

export interface TestResult {
  source: string;
  name: string;
  passed: boolean;
  durationMs: number;
  steps: StepResult[];
  error?: string;
  sandbox?: string;
}
