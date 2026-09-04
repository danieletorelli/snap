import { existsSync, lstatSync, readFileSync, type Stats } from "node:fs";
import { isDeepStrictEqual } from "node:util";
import type {
  EntryKind,
  HttpAssertion,
  HttpResult,
  ProcessAssertion,
  ProcessResult,
  StateAssertion,
} from "./types.js";
import type { ControlledServer } from "./http-server.js";
import { listTree, sandboxPath } from "./filesystem.js";
import { parseJsonUnique } from "./json.js";
import { interpolate, interpolateJson } from "./interpolate.js";

export function checkProcessAssertions(assertions: ProcessAssertion[], result: ProcessResult): string[] {
  return assertions.flatMap((assertion) => {
    switch (assertion.type) {
      case "exit_code":
        return result.exitCode === assertion.value ? [] :
          [`expected exit code ${assertion.value}, got ${result.exitCode}${result.signal ? ` (${result.signal})` : ""}`];
      case "stdout_equals": return equals("stdout", result.stdout, assertion.value);
      case "stdout_contains": return contains("stdout", result.stdout, assertion.value);
      case "stdout_matches": return matches("stdout", result.stdout, assertion.pattern);
      case "stderr_equals": return equals("stderr", result.stderr, assertion.value);
      case "stderr_contains": return contains("stderr", result.stderr, assertion.value);
      case "stderr_matches": return matches("stderr", result.stderr, assertion.pattern);
    }
  });
}

export function checkHttpAssertions(assertions: HttpAssertion[], result: HttpResult): string[] {
  return assertions.flatMap((assertion) => {
    switch (assertion.type) {
      case "status":
        return result.status === assertion.value ? [] : [`expected HTTP status ${assertion.value}, got ${result.status}`];
      case "header_equals": {
        const actual = result.headers[assertion.name.toLowerCase()];
        return actual === assertion.value ? [] :
          [`expected header ${assertion.name}: ${JSON.stringify(assertion.value)}, got ${JSON.stringify(actual)}`];
      }
      case "body_text_equals": return equals("HTTP body", decode(result.body), assertion.value);
      case "body_base64_equals": {
        const actual = result.body.toString("base64");
        return actual === assertion.value ? [] : [`HTTP body base64: expected ${assertion.value}, got ${actual}`];
      }
      case "body_json_equals": {
        try {
          const actual = parseJsonUnique(decode(result.body));
          return isDeepStrictEqual(actual, assertion.value) ? [] :
            [`HTTP JSON differed\nexpected: ${inspect(assertion.value)}\nactual:   ${inspect(actual)}`];
        } catch (error) {
          return [`HTTP body is not unique-key JSON: ${(error as Error).message}`];
        }
      }
    }
  });
}

export function checkStateAssertions(
  assertions: StateAssertion[],
  root: string,
  variables: ReadonlyMap<string, string>,
  servers: ReadonlyMap<string, ControlledServer>,
): string[] {
  return assertions.flatMap((assertion) => {
    try {
      switch (assertion.type) {
        case "tree_equals": {
          const actual = listTree(root, interpolate(assertion.path, variables)).map(({ bytes: _, ...entry }) => entry);
          const expected = assertion.entries.map((entry) => ({
            ...entry,
            path: interpolate(entry.path, variables),
            ...(entry.target === undefined ? {} : { target: interpolate(entry.target, variables) }),
          }));
          return isDeepStrictEqual(actual, expected) ? [] :
            [`tree differed\nexpected: ${inspect(expected)}\nactual:   ${inspect(actual)}`];
        }
        case "file_text_equals": {
          const path = interpolate(assertion.path, variables);
          const actual = decode(readRegular(root, path));
          return equals(path, actual, interpolate(assertion.value, variables));
        }
        case "file_base64_equals": {
          const path = interpolate(assertion.path, variables);
          const actual = readRegular(root, path).toString("base64");
          const expected = interpolate(assertion.value, variables);
          return actual === expected ? [] : [`${path} base64: expected ${expected}, got ${actual}`];
        }
        case "json_equals": {
          const path = interpolate(assertion.path, variables);
          const actual = parseJsonUnique(decode(readRegular(root, path)));
          const expected = interpolateJson(assertion.value, variables);
          return isDeepStrictEqual(actual, expected) ? [] :
            [`${path} JSON differed\nexpected: ${inspect(expected)}\nactual:   ${inspect(actual)}`];
        }
        case "path_exists": {
          const path = interpolate(assertion.path, variables);
          const target = sandboxPath(root, path);
          if (!existsSync(target) && !safeLstat(target)) return [`expected path to exist: ${path}`];
          if (assertion.kind !== undefined) {
            const actual = kindOf(lstatSync(target));
            if (actual !== assertion.kind) return [`expected ${path} to be ${assertion.kind}, got ${actual}`];
          }
          return [];
        }
        case "path_not_exists": {
          const path = interpolate(assertion.path, variables);
          const target = sandboxPath(root, path);
          return existsSync(target) || safeLstat(target) ? [`expected path not to exist: ${path}`] : [];
        }
        case "trees_equal": {
          const ignore = (assertion.ignore ?? []).map((value) => interpolate(value, variables));
          const left = listTree(root, interpolate(assertion.left, variables), ignore);
          const right = listTree(root, interpolate(assertion.right, variables), ignore);
          return equalTrees(left, right) ? [] :
            [`trees differed\nleft:  ${inspect(left)}\nright: ${inspect(right)}`];
        }
        case "http_requests_equal": {
          const server = servers.get(assertion.server);
          if (!server) return [`unknown controlled HTTP server: ${assertion.server}`];
          const expected = assertion.value.map((request) => ({
            method: request.method,
            target: interpolate(request.target, variables),
          }));
          return isDeepStrictEqual(server.requests, expected) ? [] :
            [`HTTP requests differed\nexpected: ${inspect(expected)}\nactual:   ${inspect(server.requests)}`];
        }
      }
    } catch (error) {
      return [(error as Error).message];
    }
  });
}

function equals(label: string, actual: string, expected: string): string[] {
  return actual === expected ? [] :
    [`${label} did not match\n--- expected ---\n${expected}\n--- actual ---\n${actual}`];
}

function contains(label: string, actual: string, expected: string): string[] {
  return actual.includes(expected) ? [] : [`${label} did not contain ${JSON.stringify(expected)}\n${actual}`];
}

function matches(label: string, actual: string, pattern: string): string[] {
  return new RegExp(pattern, "m").test(actual) ? [] : [`${label} did not match /${pattern}/\n${actual}`];
}

function decode(buffer: Buffer): string {
  return new TextDecoder("utf-8", { fatal: true }).decode(buffer);
}

function readRegular(root: string, path: string): Buffer {
  const target = sandboxPath(root, path);
  if (!existsSync(target)) throw new Error(`file not found: ${path}`);
  if (!lstatSync(target).isFile()) throw new Error(`not a regular file: ${path}`);
  return readFileSync(target);
}

function kindOf(stat: Stats): EntryKind {
  if (stat.isFile()) return "file";
  if (stat.isDirectory()) return "directory";
  if (stat.isSymbolicLink()) return "symlink";
  if (stat.isFIFO()) return "fifo";
  return "other";
}

function safeLstat(path: string): boolean {
  try { lstatSync(path); return true; } catch { return false; }
}

function equalTrees(left: ReturnType<typeof listTree>, right: ReturnType<typeof listTree>): boolean {
  if (left.length !== right.length) return false;
  return left.every((entry, index) => {
    const other = right[index];
    return entry.path === other.path && entry.kind === other.kind && entry.target === other.target &&
      (entry.bytes === undefined ? other.bytes === undefined : other.bytes !== undefined && entry.bytes.equals(other.bytes));
  });
}

function inspect(value: unknown): string {
  return JSON.stringify(value, (_key, item) => Buffer.isBuffer(item) ? item.toString("base64") : item, 2);
}
