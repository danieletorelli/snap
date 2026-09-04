# Snap

Snap is a small local version control system built around vector-clock
versions, patch replay, and deterministic automatic merging. It is deliberately
compact: eight everyday commands plus a read-only HTTP mode, with most of the
challenge concentrated in exact semantics and correctness.

Interactive output is designed for humans: status and history have readable
layouts, diffs use semantic colors, and successful operations, warnings, and
errors have distinct symbols. Redirected output stays plain and byte-stable for
scripts. Set `SNAP_COLOR=always` to preserve the terminal presentation through
a pipe, `SNAP_COLOR=never` to disable it, or `NO_COLOR=1` for Snap's
conservative plain-output opt-out.

## At a glance

- **Focus:** causal modelling, canonical data formats, deterministic diffs,
  operational transform, filesystem materialization, and process-level tests.
- **Expected difficulty:** high, but slightly smaller than TabbyShell. The CLI
  is narrow; replay, conflict rules, and validation require care.
- **Prerequisites:** Node.js for the public harness and the toolchain for the
  implementation language. Snap itself uses no API key or network service.
- **Languages:** TypeScript, Rust, and Scala. Every edition is checked by the
  same language-neutral suite.

## What’s here

- [`SPEC.md`](SPEC.md) — the canonical behavioral contract.
- [`tests/`](tests/) — language-neutral YAML acceptance tests.
- [`TEST-HARNESS.md`](TEST-HARNESS.md) and [`test-harness/`](test-harness/) —
  the extensible process/filesystem/HTTP test format and driver.
- `<language>/` — the selected scaffold's location in an attendee archive.
- `run` — the bundled launcher; it selects the most recently modified
  available language implementation, or accepts `--lang`.
- `verify` — the public acceptance-test entry point.

## Run Snap

From the repository root:

```bash
./capstones/snap/run init /tmp/example
./capstones/snap/run config --global contributor.id you@example.com
cd /tmp/example
echo hello > hello.txt
/path/to/ai-workshop/capstones/snap/run commit "add greeting"
```

Choose the bundled implementation language explicitly when needed:

```bash
./capstones/snap/run --lang ts --version
```

The supported surface is:

```text
snap init [path]
snap config [--global] contributor.id <id>
snap status
snap log
snap commit <message>
snap diff [<old> <new> [--repo <repository>]]
snap revert <version>
snap merge <repository>
snap --serve [port]
snap --version
```

Read the spec before relying on familiar Git behavior: Snap has no branches,
staging area, checkout, or unresolved conflicts.

## Verify

Run the full language-neutral acceptance suite against your selected workspace:

```bash
./capstones/snap/verify --lang ts
```

Replace `ts` with `rust` or `scala` when appropriate. The verifier builds the
Rust or Scala workspace before running the suite; for TypeScript it installs
locked dependencies and executes the candidate through `tsx`. Run
`npm run build` separately when you want a static type-check.


Or test any executable implemented in any language:

```bash
./capstones/snap/verify --candidate /path/to/snap
```

The YAML suite creates isolated temporary repositories and checks exact output,
history JSON, file bytes, directory state, merge convergence, and HTTP behavior.
It imports no TypeScript implementation code.
