# Snap test harness

Status: implemented. The format below is the public harness contract.

## Goal

Build a public, process-level acceptance harness that can drive any executable
Snap candidate through realistic multi-repository workflows in an isolated
temporary sandbox. YAML tests must be able to create all input bytes inline,
run commands in order, capture dynamic versions and server URLs, and assert
process output plus exact filesystem state.

The harness tests only public behavior. It does not import candidate code or
assume TypeScript, Rust, or Scala.

## Reference lessons

The TabbyShell harness contributes sorted YAML discovery, one sandbox per case,
exact stdout/stderr assertions, filtering, concise reporting, and wrapper
scripts that install harness dependencies. The agent harness contributes
multi-step cases and step-local failure reporting.

Snap differs in four ways:

1. Each CLI command is a fresh process, while history lives on disk.
2. Tests must edit and copy repositories between candidate invocations.
3. Later commands need versions printed by earlier commands.
4. HTTP tests need a candidate server or a controlled inline-response server.

Arbitrary setup shell commands will not be part of the public format. Typed
operations are portable, safely confined to the sandbox, and produce clearer
failure reports.

## Stable YAML envelope

Each `.yaml` or `.yml` file contains one case:

```yaml
format: 1
name: commit and inspect a text file
description: Optional explanation.
timeout: 30
env:
  EXAMPLE_FLAG: value
  HOME: null
steps: []
```

- `format: 1` is required.
- Unknown fields and malformed tagged variants fail during loading with the
  filename and field path.
- Files are discovered in byte-lexicographic filename order.
- `timeout` is the whole-case timeout in seconds; individual process steps may
  use a smaller timeout.
- Environment values are strings or `null`; null removes a variable. Case
  `env` modifies the deterministic harness environment, and a run step may add,
  override, or remove values for that process.

The format evolves through new tagged step and assertion variants. Existing
fields retain their meaning for all format-1 tests. A format bump is reserved
for an incompatible reinterpretation, not an additive capability.

## Variables

Payload strings support `{{name}}` interpolation. This includes paths and
working directories; file bodies and link targets; argument, stdin, URL,
header, and environment values; expected output, regex, and JSON string values;
and HTTP route bodies and targets. Built-ins are:

- `{{sandbox}}`: absolute case sandbox path;
- `{{candidate}}`: absolute candidate executable path.

Capture operations add variables for later steps. Names match
`[A-Za-z_][A-Za-z0-9_]*`, cannot replace built-ins, and cannot be redefined.
Tags, enum values, operation/process/server IDs, mapping keys, and capture
variable names are structural and are never interpolated. This lets the loader
fully validate discriminated unions before execution. An unknown variable in a
payload is a harness error. Paths and working directories are confinement-
validated after interpolation. Argument arrays pass directly to the process
API; interpolation never invokes a shell.

Interpolation is a single pass. `{{{{` emits literal `{{` and `}}}}` emits
literal `}}`, so `{{{{name}}}}` passes `{{name}}` without expanding it. Captured
text is never re-interpolated.

## Filesystem operations

All operation paths and command working directories are sandbox-relative,
must be normalized, and may not escape through `..` or symlink traversal.
Parent directories are created for file writes.

```yaml
steps:
  - mkdir:
      path: alice

  - write_file:
      path: alice/hello.txt
      text: |
        hello

  - write_file:
      path: alice/image.bin
      base64: AAEC

  - copy_tree:
      from: alice
      to: bob

  - remove:
      path: bob/hello.txt

  - symlink:
      path: alice/link
      target: hello.txt

  - fifo:
      path: alice/named-pipe
```

`write_file` requires exactly one of `text` and canonical base64. `copy_tree`
copies recursively and fails if its destination exists. `remove` removes a
file, symlink, or directory tree and fails if absent. `symlink` exists so the
suite can verify Snap's required rejection of unsupported entries; its target
is the literal link payload. `copy_tree` preserves links without following
them. The typed `fifo` operation invokes the host's `mkfifo` without a shell,
covering one non-link special-file representative on the POSIX platforms the
workshop supports.

## Candidate process operations

### Completed command

```yaml
- run:
    cwd: alice
    args: [commit, "add greeting"]
    stdin: ""
    timeout: 5
    env:
      HOME: "{{sandbox}}/home"
    capture:
      stdout:
        as: first_version
        trim: true
    expect:
      - type: exit_code
        value: 0
      - type: stdout_matches
        pattern: '^\(.+->1\)\n$'
      - type: stderr_equals
        value: ""
```

`cwd` defaults to the sandbox root, `args` defaults to empty, `stdin` defaults
to closed/empty, and step timeout defaults to the remaining case timeout.
Output is decoded as strict UTF-8. Capture occurs only after process completion
and supports stdout or stderr with explicit optional trimming. Assertions run
before captured variables become visible to later steps.

Supported process assertions initially are:

- `exit_code`;
- `stdout_equals`, `stdout_contains`, `stdout_matches`;
- `stderr_equals`, `stderr_contains`, `stderr_matches`.

Regexes use JavaScript syntax with multiline mode. Exact assertions compare
every byte after UTF-8 decoding, including final LF.

Every `run` and `stop` requires exactly one `exit_code` assertion; crashes and
unexpected failures can never pass by omission. The harness caps each stdout
and stderr stream at 16 MiB, terminates a candidate that exceeds it, and reports
a harness failure instead of retaining unbounded output.

### Candidate background process

```yaml
- start:
    id: origin
    cwd: alice
    args: [--serve, "0"]
    ready:
      stream: stdout
      pattern: '^(http://[^\n]+)\n'
      capture:
        as: origin_url
        group: 1
    timeout: 5

- run:
    cwd: bob
    args: [merge, "{{origin_url}}"]
    expect:
      - type: exit_code
        value: 0

- stop:
    id: origin
    signal: SIGTERM
    timeout: 5
    expect:
      - type: exit_code
        value: 0
```

`start` launches the candidate and waits until the selected accumulated stream
matches `ready.pattern`. IDs are case-local and unique. The optional capture
stores a regex group. `stop` signals the whole process group, waits, then checks
the process's complete stdout/stderr and exit code. Any process still running
at cleanup is terminated by the harness.

## Controlled HTTP operation

Malformed and edge-case remote tests need a server that is not Snap:

```yaml
- start_http:
    id: malformed
    capture_url: malformed_url
    routes:
      - method: GET
        target: /repository.json
        status: 200
        headers:
          content-type: application/json
        text: '{not json}'

- run:
    cwd: alice
    args: [merge, "{{malformed_url}}/repository.json"]
    expect:
      - type: exit_code
        value: 1

- assert:
    - type: http_requests_equal
      server: malformed
      value:
        - method: GET
          target: /repository.json

- stop_http:
    id: malformed
```

`start_http` binds an OS-selected loopback port. Route bodies require exactly
one of `text` and base64 when present. A route matches exact `(method, target)`,
where `target` is the raw origin-form request target (path plus query) without
decoding or normalization. Unmatched routes return 404. The server records the
same method/target pairs in order. Candidate redirects, invalid JSON, status
handling, and exact request targets can therefore be tested without external
fixtures or network services.

## HTTP client operation

The harness can directly inspect a candidate `--serve` process:

```yaml
- http_request:
    method: HEAD
    url: "{{origin_url}}"
    timeout: 5
    expect:
      - type: status
        value: 200
      - type: header_equals
        name: content-type
        value: application/json; charset=utf-8
      - type: body_base64_equals
        value: ""
```

Redirects are disabled. HTTP assertions are `status`, `header_equals`,
`body_text_equals`, `body_base64_equals`, and `body_json_equals`. Header names
are case-insensitive; values are exact after the Node HTTP client's normal
optional-whitespace handling. Bodies are capped at 16 MiB and strict UTF-8 is
used by text/JSON assertions. `body_json_equals` rejects duplicate object keys.
For HEAD only, the harness uses a raw `node:net` connection with
`Connection: close`, parses through the HTTP header terminator, and exposes any
trailing bytes as the body; this makes a body-writing protocol violation
observable instead of letting a high-level client discard it. Other methods use
the normal Node HTTP client. This operation covers GET/HEAD, headers, 404/405,
exact response bytes, and startup-snapshot behavior.

The offline public harness exercises `http://`. Establishing trust for a local
self-signed `https://` server requires runtime-specific Node, JVM, and native
TLS configuration, so HTTPS certificate/trust integration belongs to
language-specific CI rather than this one-executable format. URL recognition
and all transport-independent remote validation remain public-harness concerns.

## State assertions

An `assert` step contains tagged assertions evaluated at that point:

```yaml
- assert:
    - type: tree_equals
      path: alice
      entries:
        - path: .snap
          kind: directory
        - path: .snap/repository.json
          kind: file
        - path: hello.txt
          kind: file
        - path: link
          kind: symlink
          target: hello.txt

    - type: file_text_equals
      path: alice/hello.txt
      value: |
        hello

    - type: file_base64_equals
      path: alice/image.bin
      value: AAEC

    - type: json_equals
      path: alice/.snap/config.json
      value:
        contributor:
          id: alice@example.com

    - type: path_exists
      path: alice/.snap
      kind: directory

    - type: path_not_exists
      path: alice/obsolete.txt

    - type: trees_equal
      left: alice
      right: bob
      ignore: [.snap]
```

`tree_equals` recursively lists all descendants, including directories,
symlinks, and FIFOs, and compares exact entries in unsigned UTF-8 path order.
Symlink entries require their literal `target`; traversal never follows them.
`json_equals` compares parsed JSON values, not serialization, and rejects
duplicate keys in the actual file before comparison. `trees_equal` compares
kinds, file bytes, and symlink targets recursively, with ignored relative path
prefixes omitted from both sides. Assertions never mutate the sandbox.

## Isolation and lifecycle

For every case the runner:

1. creates a fresh `snap-test-*` temporary directory;
2. creates isolated `home/` and `tmp/` directories;
3. starts from only inherited `PATH`, sets sandbox `HOME` and `TMPDIR`, sets
   `NO_COLOR=1`, `LANG=C`, `LC_ALL=C`, and `NO_PROXY=127.0.0.1,localhost`, and
   removes all other parent and proxy environment variables before applying
   case/step `env`;
4. runs all steps sequentially, stopping at the first harness/operation error
   but reporting all failed assertions in an assertion list;
5. terminates managed processes and servers in `finally`; and
6. removes the sandbox, unless `--keep-failed` preserves a failed case and
   prints its path.

Candidate nonzero exits are normal observable results, not harness errors.
Timeouts are failures and kill the candidate process group so wrappers cannot
leave children behind.

## CLI and scripts

Public entry point:

```bash
./capstones/snap/verify
./capstones/snap/verify --lang ts|rust|scala
./capstones/snap/verify --candidate /absolute/or/relative/path/to/snap
```

Options:

```text
--lang LANGUAGE    bundled implementation to test (default: latest modified)
--candidate PATH   executable to test (defaults to the bundled run launcher)
--tests PATH       alternate YAML directory
--filter TEXT      filename/name substring
--verbose, -v      print step stdout/stderr and retained state
--keep-failed      preserve failed sandboxes
--summary PATH     write a plain-text machine/agent-friendly summary
--list             validate and list tests without running
```

With no candidate, the harness uses `capstones/snap/run`, which selects the
most recently modified available bundled implementation. `--lang` selects a
specific bundled implementation, while `--candidate` remains the
language-neutral path for any independently built executable; the two options
cannot be combined. `verify` delegates to `run_tests`, which installs locked
harness dependencies on first use and invokes the TypeScript CLI. For a bundled
language selected with `--lang`, `run_tests` prepares the workspace: it installs
TypeScript dependencies or builds Rust/Scala. An explicit `--candidate` remains
responsible for providing its executable.

## Implementation layout

```text
capstones/snap/
  TEST-HARNESS.md
  tests/
  run_tests
  verify
  test-harness/
    package.json
    package-lock.json
    tsconfig.json
    src/
      types.ts
      yaml-loader.ts
      interpolate.ts
      filesystem.ts
      process.ts
      http-server.ts
      assertions.ts
      runner.ts
      reporter.ts
      cli.ts
    test/
```

The implementation uses discriminated unions and exhaustive switches.
Loader validation owns external YAML shape; execution code receives validated
typed values. Filesystem confinement is centralized rather than reimplemented
by operations. Strict JSON assertions use a duplicate-aware parser rather than
plain `JSON.parse` alone.

## Verification of the harness

Node tests create a small temporary fake candidate executable and cover:

- YAML discovery, strict diagnostics, filtering, and format rejection;
- text/base64 writes, copy, removal, symlink creation, and path confinement;
- run ordering, cwd/env/stdin, exact output assertions, capture/interpolation,
  nonzero exits, and timeout cleanup;
- exact tree, text, bytes, JSON, existence, and tree-comparison assertions;
- candidate background readiness/termination and controlled HTTP requests;
- direct HTTP response/status/header/body checks; and
- mandatory exit assertions and stdout/stderr limits.

The public Snap YAML suite exercises `init` and asserts both the complete
directory listing and parsed `repository.json`. The same suite runs against the
bundled language workspace or any executable supplied with `--candidate`.

## Deliberate non-goals

- No arbitrary shell setup or cleanup.
- No language-specific candidate discovery or build logic.
- No external fixture files or Internet dependency.
- No inspection of implementation internals.
- No parallel case execution initially; deterministic diagnostics matter more
  than suite throughput for this capstone.
- No cross-language producer/consumer orchestration in the public harness. The
  YAML suite tests one supplied candidate without requiring two other language
  toolchains.
- No public self-signed TLS trust setup; language-specific CI covers HTTPS.
