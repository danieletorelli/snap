import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { discoverTests, loadTest } from "../src/yaml-loader.js";
import { parseJsonUnique } from "../src/json.js";
import { interpolate } from "../src/interpolate.js";
import { deterministicEnvironment, runProcess } from "../src/process.js";
import { runCase } from "../src/runner.js";

test("loader validates the versioned tagged format", () => {
  const root = mkdtempSync(join(tmpdir(), "snap-loader-test-"));
  try {
    const valid = join(root, "valid.yaml");
    writeFileSync(valid, `
format: 1
name: valid
steps:
  - run:
      args: [echo, hello]
      expect:
        - type: exit_code
          value: 0
`);
    assert.equal(loadTest(valid).steps[0]?.type, "run");

    const missingExit = join(root, "missing-exit.yaml");
    writeFileSync(missingExit, `
format: 1
name: invalid
steps:
  - run:
      expect:
        - type: stdout_equals
          value: ""
`);
    assert.throws(() => loadTest(missingExit), /requires exactly one exit_code/);

    const typo = join(root, "typo.yaml");
    writeFileSync(typo, `
format: 1
name: invalid
steps:
  - mkdir:
      paths: repo
`);
    assert.throws(() => loadTest(typo), /unknown field.*paths/);
    rmSync(missingExit);
    rmSync(typo);
    assert.deepEqual(discoverTests(root, "valid").map((item) => item.name), ["valid"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("JSON parser rejects duplicate keys at any depth", () => {
  assert.deepEqual(parseJsonUnique('{"a":[{"b":1}]}'), { a: [{ b: 1 }] });
  assert.throws(() => parseJsonUnique('{"a":{"x":1,"x":2}}'), /duplicate JSON key.*x/);
});

test("interpolation is single-pass and supports literal delimiters", () => {
  const variables = new Map([["value", "{{other}}"], ["other", "expanded"]]);
  assert.equal(interpolate("{{value}}", variables), "{{other}}");
  assert.equal(interpolate("{{{{other}}}}", variables), "{{other}}");
  assert.throws(() => interpolate("{{missing}}", variables), /unknown variable/);
});

test("foreground process timeouts are harness failures", async () => {
  const root = mkdtempSync(join(tmpdir(), "snap-timeout-test-"));
  try {
    await assert.rejects(
      runProcess({
        candidate: "/bin/sh",
        args: ["-c", "sleep 10"],
        cwd: root,
        env: deterministicEnvironment(root),
        stdin: "",
        timeoutMs: 20,
      }),
      /did not exit within 20ms/,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("fast process exit does not turn a closed stdin pipe into a harness failure", async () => {
  const root = mkdtempSync(join(tmpdir(), "snap-stdin-epipe-test-"));
  try {
    const result = await runProcess({
      candidate: "/bin/sh",
      args: ["-c", "exit 7"],
      cwd: root,
      env: deterministicEnvironment(root),
      stdin: "x".repeat(1024 * 1024),
      timeoutMs: 5_000,
    });
    assert.equal(result.exitCode, 7);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("process output is bounded", async () => {
  const root = mkdtempSync(join(tmpdir(), "snap-output-limit-test-"));
  try {
    await assert.rejects(
      runProcess({
        candidate: process.execPath,
        args: ["-e", "process.stdout.write('x'.repeat(17 * 1024 * 1024))"],
        cwd: root,
        env: deterministicEnvironment(root),
        stdin: "",
        timeoutMs: 5_000,
      }),
      /stdout exceeded 16 MiB limit/,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("runner executes ordered process, filesystem, HTTP, and background operations", async () => {
  const root = mkdtempSync(join(tmpdir(), "snap-runner-test-"));
  const candidate = join(root, "candidate.mjs");
  const yaml = join(root, "case.yaml");
  writeFileSync(candidate, `#!/usr/bin/env node
import { createServer } from "node:http";
const [command, ...args] = process.argv.slice(2);
if (command === "echo") {
  process.stdout.write(args[0] + "\\n");
  process.stderr.write(("HOME" in process.env ? process.env.HOME : "absent") + "\\n");
} else if (command === "input") {
  for await (const chunk of process.stdin) process.stdout.write(chunk);
} else if (command === "serve") {
  const server = createServer((_req, res) => {
    res.setHeader("content-type", "application/json; charset=utf-8");
    res.end('{"ok":true}');
  });
  server.listen(0, "127.0.0.1", () => {
    const address = server.address();
    process.stdout.write('http://127.0.0.1:' + address.port + '/repository.json\\n');
  });
  process.on("SIGTERM", () => server.close(() => process.exit(0)));
} else {
  process.stderr.write("unknown\\n");
  process.exitCode = 7;
}
`);
  chmodSync(candidate, 0o755);
  writeFileSync(yaml, `
format: 1
name: complete harness workflow
timeout: 15
steps:
  - mkdir: {path: repo}
  - mkdir: {path: source}
  - write_file: {path: source/copied.txt, text: copied}
  - copy_tree: {from: source, to: clone}
  - assert:
      - {type: trees_equal, left: source, right: clone}
  - remove: {path: clone}
  - assert:
      - {type: path_not_exists, path: clone}
  - write_file: {path: repo/text.txt, text: "hello\\n"}
  - write_file: {path: repo/data.bin, base64: AAEC}
  - write_file: {path: repo/value.json, text: '{"answer":42}'}
  - symlink: {path: repo/link, target: text.txt}
  - fifo: {path: repo/pipe}
  - run:
      cwd: repo
      args: [echo, captured]
      env: {HOME: null}
      capture:
        stdout: {as: token, trim: true}
      expect:
        - {type: exit_code, value: 0}
        - {type: stdout_equals, value: "captured\\n"}
        - {type: stderr_equals, value: "absent\\n"}
  - run:
      cwd: repo
      args: [input]
      stdin: "input bytes\\n"
      expect:
        - {type: exit_code, value: 0}
        - {type: stdout_equals, value: "input bytes\\n"}
  - run:
      args: [unknown]
      expect:
        - {type: exit_code, value: 7}
        - {type: stderr_equals, value: "unknown\\n"}
  - write_file: {path: repo/captured.txt, text: "{{token}}"}
  - assert:
      - type: tree_equals
        path: repo
        entries:
          - {path: captured.txt, kind: file}
          - {path: data.bin, kind: file}
          - {path: link, kind: symlink, target: text.txt}
          - {path: pipe, kind: fifo}
          - {path: text.txt, kind: file}
          - {path: value.json, kind: file}
      - {type: file_text_equals, path: repo/captured.txt, value: captured}
      - {type: file_base64_equals, path: repo/data.bin, value: AAEC}
      - {type: json_equals, path: repo/value.json, value: {answer: 42}}
      - {type: path_exists, path: repo/pipe, kind: fifo}
  - start_http:
      id: fixture
      capture_url: fixture_url
      routes:
        - method: GET
          target: /value?q=1
          status: 200
          headers: {content-type: application/json}
          text: '{"answer":42}'
  - http_request:
      method: GET
      url: "{{fixture_url}}/value?q=1"
      expect:
        - {type: status, value: 200}
        - {type: header_equals, name: content-type, value: application/json}
        - {type: body_json_equals, value: {answer: 42}}
  - assert:
      - type: http_requests_equal
        server: fixture
        value: [{method: GET, target: /value?q=1}]
  - stop_http: {id: fixture}
  - start:
      id: candidate_server
      cwd: repo
      args: [serve]
      ready:
        stream: stdout
        pattern: '^(http://[^\\n]+)\\n'
        capture: {as: candidate_url, group: 1}
  - http_request:
      method: HEAD
      url: "{{candidate_url}}"
      expect:
        - {type: status, value: 200}
        - {type: header_equals, name: content-type, value: "application/json; charset=utf-8"}
        - {type: body_base64_equals, value: ""}
  - stop:
      id: candidate_server
      expect:
        - {type: exit_code, value: 0}
`);
  try {
    const result = await runCase(loadTest(yaml), { candidate, keepFailed: true });
    assert.equal(result.passed, true, JSON.stringify(result, null, 2));
    if (result.sandbox) rmSync(result.sandbox, { recursive: true, force: true });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("runner rejects paths that traverse a fixture symlink", async () => {
  const root = mkdtempSync(join(tmpdir(), "snap-confinement-test-"));
  const candidate = join(root, "candidate");
  const yaml = join(root, "case.yaml");
  writeFileSync(candidate, "#!/bin/sh\nexit 0\n");
  chmodSync(candidate, 0o755);
  writeFileSync(yaml, `
format: 1
name: confinement
steps:
  - symlink: {path: escape, target: /tmp}
  - write_file: {path: escape/owned, text: nope}
`);
  try {
    const result = await runCase(loadTest(yaml), { candidate });
    assert.equal(result.passed, false);
    assert.match(result.error ?? "", /traverses symlink/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("runner stops after the first failed assertion", async () => {
  const root = mkdtempSync(join(tmpdir(), "snap-fail-fast-test-"));
  const candidate = join(root, "candidate");
  const yaml = join(root, "case.yaml");
  writeFileSync(candidate, "#!/bin/sh\nprintf 'actual\\n'\n");
  chmodSync(candidate, 0o755);
  writeFileSync(yaml, `
format: 1
name: fail fast
steps:
  - run:
      capture: {stdout: {as: output}}
      expect:
        - {type: exit_code, value: 0}
        - {type: stdout_equals, value: "expected\\n"}
  - write_file: {path: should-not-exist, text: "{{output}}"}
`);
  try {
    const result = await runCase(loadTest(yaml), { candidate });
    assert.equal(result.passed, false);
    assert.equal(result.steps.length, 1);
    assert.match(result.steps[0]?.failures[0] ?? "", /stdout did not match/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("runner does not follow a final symlink for file writes or working directories", async () => {
  const root = mkdtempSync(join(tmpdir(), "snap-final-symlink-test-"));
  const candidate = join(root, "candidate");
  writeFileSync(candidate, "#!/bin/sh\nexit 0\n");
  chmodSync(candidate, 0o755);
  try {
    for (const [name, operation, expected] of [
      ["write", "  - write_file: {path: escape, text: nope}", /write target is a symlink/],
      ["cwd", "  - run:\n      cwd: escape\n      expect: [{type: exit_code, value: 0}]", /working directory is a symlink/],
    ] as const) {
      const yaml = join(root, `${name}.yaml`);
      writeFileSync(yaml, `
format: 1
name: final symlink ${name}
steps:
  - symlink: {path: escape, target: /tmp}
${operation}
`);
      const result = await runCase(loadTest(yaml), { candidate });
      assert.equal(result.passed, false);
      assert.match(result.error ?? "", expected);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
