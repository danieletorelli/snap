# Snap — Rust implementation plan

## 0. Ground rules

**Full compliance with `SPEC.md` and a green test harness are the bare minimum,
not a goal to trade against.** Nothing in this plan deviates from the spec.

`RESEARCH.md` exists to answer one question: *how did other projects solve the
problems Snap has to solve, and which of their techniques should we adopt?* It
is a source of implementation technique — data structures, algorithms, cost
models — never of behaviour. Snap's behaviour comes from `SPEC.md` alone. §4.0
lists what we harvest from it; §4 and §8.2 apply it.

Three items in `RESEARCH.md` are product-design opinions rather than techniques,
so they are simply **not applicable** here and are recorded once so nobody
re-raises them mid-implementation: minting per-clone actor ids (would violate
§3.1/§7.2), adding a sixth `text-overlap` warning reason (would violate §6.4),
and caching a materialized tree inside `.snap/` (would violate §4.1). Each is
closed. See §8.6 for the one place this costs us something real, and what we do
about it within the spec.

**Two hard constraints discovered while reading the suite**, which shape
several decisions below:

1. **Nothing may be written inside `.snap/` except `repository.json` and
   `config.json`.** This removes on-disk caching entirely. All performance work
   must therefore pay off *within a single process invocation*, because every
   CLI command is a fresh process.
2. **`repository.json` must be byte-identical between two repositories that
   converged by different merge routes.** `trees_equal` compares bytes. Since
   both sides are written by our own serializer, this reduces to: identical
   typed value must produce identical bytes. The serializer must be
   canonical and deterministic — fixed key order, fixed two-space indentation,
   trailing LF — with no map iteration order anywhere in the write path.

---

## 1. Target and toolchain

Create `rust/` at the capstone root. The bundled launcher already supports it:
`run` looks for `rust/src/main.rs`, builds with `cargo build --quiet`, and
executes `rust/target/debug/snap`. So the binary **must** be named `snap`.

```
rust/
  Cargo.toml          # [lib] name = "snap"  +  [[bin]] name = "snap"
  Cargo.lock          # committed
  src/lib.rs          # every module; all logic lives here
  src/main.rs         # thin bin target: arg vector in, exit code out
  benches/            # criterion benchmarks
  tests/              # cargo integration tests (see them via the lib target)
```

**Both a `[lib]` and a `[[bin]]` target are required, not optional.** Cargo
integration tests under `rust/tests/` can only `use` a library target; a
binary-only crate is reachable solely from `#[cfg(test)]` modules inside itself.
Since the goal is broad unit and integration coverage, all logic lives in the
lib and `main.rs` stays a shell that parses `std::env::args_os`, calls one lib
entry point, and maps its result to an exit code. `run` executes
`target/debug/snap`, so the bin target must keep that name.

`run` decides which language to use by the mtime of the main source file, so
`rust/` becomes the default once we start editing it. `verify --lang rust`
forces it regardless.

**Dependency policy: exactly one runtime dependency, and it is TLS.** Everything
the acceptance suite exercises is std-only; `https://` (§8.1) is the single
exception, because §9 makes it a MUST and TLS cannot be hand-rolled
responsibly. This is not asceticism, it is the shortest path to the strictness
the spec demands:

- **JSON** — we need duplicate-key rejection (`15-repository-validation` feeds
  `{"format":1,"format":1,...}`), rejection of non-integer numbers, safe-integer
  range enforcement, unknown-field rejection with a precise field path, and a
  canonical writer. A hand-written reader that parses *straight into our typed
  structs in one pass* — no intermediate DOM — gives us all of that and is
  faster than parse-to-`Value`-then-validate.
- **base64** — §4.3 requires standard padded RFC 4648 with non-canonical input
  rejected. Roughly sixty lines, and we control the strictness.
- **HTTP** — §9 is one fixed resource, `GET`/`HEAD /repository.json`, plus
  404 and 405-with-`Allow`. `std::net::TcpListener` and a minimal HTTP/1.1
  reader/writer is less code than wiring an async runtime, and gives exact
  control over header bytes and the SIGINT/SIGTERM exit path. Redirects are
  *not* followed — `14-cli-errors` asserts on `HTTP 302`.
- **TLS** — `rustls` + `webpki-roots`, reachable from one module only (§8.1).

Dev-dependencies are unconstrained; we will want a property-testing crate.

---

## 2. Module map

Mirrors the separation `AGENTS.md` mandates. Everything except `main.rs` is
library code so unit tests can reach it.

| Module | Owns | Spec |
| --- | --- | --- |
| `version` | Vector clocks: canonical parse/format, four-way compare, join, Snap order, contributor-id and revision validation | §3 |
| `json` | Strict single-pass reader into typed values; canonical writer | §4.1 |
| `b64` | Strict padded base64 encode/decode | §4.3 |
| `text` | Text detection, LF-retaining tokenization, token interning, the §5 diff, edit-script validation and application | §4.4, §5 |
| `ot` | The six-row transform of an edit against an aggregate context edit | §6.3 |
| `model` | Repository, patch, change types; the interned tree representation | §4 |
| `validate` | The six validation passes, dot-collision detection, known-version test | §4.5 |
| `replay` | Patch selection, ready-set ordering, namespace resolution, per-path integration, path-level rules, warning collection | §6.1, §6.2, §6.4 |
| `worktree` | Tracked-tree scan, path validation, status computation, materialization | §2, §10 |
| `config` | Local-then-global resolution with strict shape | §8 |
| `present` | `SNAP_COLOR`/`NO_COLOR` resolution, plain and terminal renderers | §7.11 |
| `http` | Server and one-shot client | §9 |
| `error` | One typed error enum, its one-line `snap: <detail>` rendering, and the exit-code mapping | §10 |
| `cmd/*` | `init`, `config`, `status`, `log`, `commit`, `diff`, `revert`, `merge` | §7 |
| `main` | Arg grammar, repository discovery, exit codes, stream routing | §7, §10 |

`error` is a module rather than ad-hoc strings because the suite pins error text
with 103 exact `stderr_equals` assertions. Wording is part of the contract, so
it belongs in one reviewable table, not scattered across call sites.

Rule enforced throughout: **commands compute a result value; `present` renders
it.** No command formats its own output and no renderer makes a decision. This
is what keeps §7.11's "presentation MUST NOT change execution" true by
construction rather than by discipline, and it makes `28-terminal-presentation`
a rendering test rather than a second pass over every command.

---

## 3. Representation decisions

These are where most of the performance comes from, and they are cheap to make
now and expensive to retrofit.

**Paths.** Rust's `Ord` for `str` is already unsigned-byte lexicographic, which
is exactly §2's ordering — no custom comparator, unlike the JavaScript edition.
Intern every path once per process into a `PathId`; keep a side table sorted by
bytes so ordered iteration is a table walk.

**Content interning.** Intern every file body to a `ContentId` in an arena.
This matters because the two hottest checks in §6.2 — "path is identical in `B`
and `C`" and "path is identical in `C` and `T`" — become integer comparisons
instead of byte comparisons. In a mostly-unchanged tree that is the difference
between O(tree bytes) and O(changed paths) per integrated patch.

**Interning must be exact, not probabilistic.** A hash collision here silently
produces a wrong merge, which is the worst failure mode this system has. The
arena is a hash map keyed by the full bytes, so a hash is only a bucket hint and
equality is always confirmed byte-wise. We do **not** use "same 128-bit digest
implies same content" anywhere; §4.2 makes the parsed typed value authoritative
for patch identity, and we hold content to the same standard.

**Paths sort by bytes, not by id.** Interning assigns ids in first-encounter
order, which is *not* byte order, so a `Vec<(PathId, ContentId)>` cannot be
binary-searched by path without indirecting through the path table on every
comparison. Two ways out; we take the first:

1. **Two-phase interning.** Paths are all known after parsing the repository
   and scanning the working tree, so intern once, then sort the table by bytes
   and renumber. After that `PathId` order *is* byte order, ordered iteration is
   a linear walk, and binary search is a plain integer compare. New paths
   discovered later go through a small overflow table that is merged at the next
   phase boundary.
2. Store `Box<str>` inline and compare bytes. Simpler, measurably slower in the
   replay inner loop.

**Trees.** A tree is then a sorted `Vec<(PathId, ContentId)>` — eight bytes per
entry, cache-friendly, and a snapshot is a flat memcpy. Deliberately *not* a
persistent HAMT: at the repository sizes this spec targets, a memcpy of a few
thousand pairs beats pointer chasing, and the code is far easier to prove
correct. If a profile ever says otherwise, swapping in a persistent map is a
contained change behind the same interface.

**Text tokens.** Intern tokens per diff into `u32` ids so the diff and the
transform compare integers. Token identity is exact bytes, so interning is
sound.

**Patches.** Stored parsed, indexed by dot in a map, plus a canonical-order
`Vec` built once per process.

---

## 4. Performance strategy

The spec's cost model is brutal if taken literally: §6.1 replays from the empty
tree on every operation, and §6.2 asks for "each incoming patch's exact base
tree", which naively is one full replay per patch — O(n²) patch applications.
`RESEARCH.md` §8.2 flags this as the single highest-value optimization
available. Since we cannot cache to disk (constraint 1), the whole win must
come from doing one replay well.

### 4.0 What we harvest from the competition

`RESEARCH.md` surveyed twenty-one systems. These are the techniques worth
taking, each mapped to where it lands in Snap. All are internal; none is
observable in Snap's behaviour.

| Source | Technique | How we apply it |
| --- | --- | --- |
| **eg-walker / Diamond Types** | *Critical versions* — a version that partitions the event graph lets replay start there instead of from nothing | §4.1's prefix-snapshot memoization and §4.2's linear fast path. Snap has critical versions everywhere a history is sequential; we stop paying for concurrency machinery we do not need. |
| **eg-walker** | Transform against an aggregate, not per historical operation — the paper measures 1 hour vs 24 ms on a real document | §6.3 already mandates this. We record *why* in `ot` so it is never "optimized" into a per-patch loop. |
| **Pijul** | Keep a materialized *pristine* so state is not recomputed from history each time | Cannot cache to disk (§4.1 layout is closed), so the same idea moves in-process: memoize version→tree for the lifetime of one invocation. |
| **Automerge / Yjs** | `(actor, seq)` pairs as primary keys; run-length compression of adjacent items | Patches indexed by dot in a map; text tokens interned to `u32` so diff and transform compare integers. |
| **RGA / Logoot** | *What to avoid* — per-element identifiers and tombstones cost 16–32 bytes per character and grow without bound | Snap stores no per-element ids at all. We keep it that way: content interning gives O(1) identity checks without per-character metadata. |
| **Git** | *What to avoid* — merge-base computation is the source of a documented mis-merge class under criss-cross history | Snap never computes a merge base. Replay-from-empty is the reason, and it is a feature to preserve, not a cost to optimize away. |
| **Dynamo / Riak** | Version-vector truncation to bound growth | **Explicitly not adopted.** Truncating would break §4.1's closure requirement. We accept the O(patches × contributors) bound and document it. |
| **Darcs** | *What to avoid* — recursive conflict resolution produces exponential merge cost | Snap's §6.2 resolution is flat by construction. Keep it flat. |
| **Fossil / Sapling** | Append-only history; nothing is ever rewritten | Matches §7.7's additive revert. Our storage layer never mutates a patch in place. |


### 4.1 Prefix-snapshot memoization for base trees

Collect the set of base
versions the patch set actually references *before* replaying — they are right
there in the parsed patches. During the single canonical replay, snapshot the
tree only at steps whose joined-so-far version is in that set.

This bound matters. Snapshotting after *every* step, as an earlier draft of this
plan said, costs O(patches x tree size) memory — a thousand patches over ten
thousand files is 80 MB of snapshots to answer at most a thousand queries.
Snapshotting only at referenced frontiers costs O(distinct base versions x tree
size) and is usually a small fraction of that.

**Why matching the frontier is enough to identify the patch set.** The lookup
key is a version, but what we need is that the integrated set equals the base's
causal closure. It does: the replay integrates a patch only once its base is
integrated, so the integrated set is causally downward-closed; §3.5's serial
contributor rule makes each contributor's integrated revisions a contiguous
prefix `1..k`; therefore the integrated set is exactly
`{(c,n) : n <= join[c]}`, which is the closure of the joined version. Matching
the frontier does identify the set.

**Why the order within that prefix is the same.** Replay greedily takes the
Snap-minimum ready patch. For a prefix `S`, the ready set within `S` is a subset
of the full ready set at the same step (readiness depends only on the integrated
set, which is equal by induction). The full replay's pick lies in `S`, and a
minimum over a superset is no greater than one over a subset, so the two minima
coincide. Hence replaying `S` alone reproduces the prefix order.

Both arguments are *ours, not the spec's*, and `RESEARCH.md` flags the second as
inferred. §5 turns them into a property test and §8.3 into a runtime
differential check. Until both are green, the optimization stays behind a flag
and the memoized sub-replay is the shipping path.

In a linear history — the common case — every base is a hit and replay drops to
O(total patch size). Non-prefix bases fall back to a memoized sub-replay keyed
by version.

### 4.2 Let the linear fast path fall out of rule 1

With content interning,
§6.2 rule 1 ("identical in `B` and `C`, apply directly") fires for every path of
every patch in a fully sequential history. No OT runs, no namespace scan, no
aggregate diff. This is the eg-walker *critical version* insight arriving for
free: a repository with no concurrency spanning a point costs nothing extra
there. We add one cheap global check — is the patch set causally totally
ordered? — to skip the concurrency machinery wholesale.

### 4.3 Transform once, not once per patch

§6.3 already specifies this, and
it is the reason Snap escapes OT's documented quadratic merge cost — the
eg-walker paper measures a one-hour OT merge against 24 ms. Worth a comment in
`ot` so nobody "optimizes" it into a per-patch loop later.

### 4.4 Diff

Ship the literal §5 recurrence first, with two accelerations that
are provably output-preserving: token interning (integer compares) and trimming
the common prefix and suffix before the DP. Trimming needs its own proof — the
greedy walk emits `retain` on equal tokens at the frontier, and the tie rule
never prefers a delete over an available equal-token retain — so it goes in the
property-test set alongside 4.1.

Myers/Hirschberg is explicitly permitted by §5 "only if it produces the same
script". We treat that as an *optional later step, gated behind differential
testing against the DP*, not as a starting point. `RESEARCH.md` §8.2 warns that
nobody has verified the equivalence including the deletion-on-tie rule and
repeated lines; we will not be the ones who assume it. At the file sizes the
suite exercises, the trimmed DP is not the bottleneck.

### 4.5 Single-pass parse

Reading `repository.json` straight into typed structs
avoids building and re-walking a DOM. Since §9 ships the entire repository on
every remote merge, parse throughput is the actual remote-merge cost.

### 4.6 Validation ordering

§10 requires all validation, replay, and target
construction to complete before any write. That is a correctness requirement,
but it also lets us do exactly one replay per invocation and reuse it for the
dirty check, the target tree, and the warning set.

---

## 5. Correctness and testing strategy

Every feature lands with tests in the same commit. Three layers:

**Unit tests, colocated per module.** Version algebra including all four
comparison outcomes and the concurrent case that a naive `PartialOrd` collapses;
canonical parse rejections (duplicate ids, explicit zeroes, leading zeroes,
overflow, whitespace, misordering); tokenization edge cases (empty file, no
final LF, lone CR, CRLF); edit-script validation (adjacent same-kind ops, not
consuming the full old sequence, empty script only for empty creation); each of
the six transform rows in isolation; each of the six path-level rules; strict
JSON rejections; base64 non-canonical input.

**Property tests** (§11 asks for these). Version join is a semilattice —
idempotent, commutative, associative — and Snap order is a strict total order
extending causal order. `apply(diff(a, b), a) == b` for random token sequences.
Diff output is stable under the trimming optimization (differential against the
untrimmed DP). Import is idempotent, commutative, and associative: generate
random valid causal patch graphs and assert every permutation yields the same
frontier, patch set, warning set, and tree. And the prefix-snapshot claim from
§4.1.

**Coverage.** `cargo llvm-cov` in CI with a line-coverage floor that starts at
80% and ratchets upward; the build fails if coverage drops. A floor is not a
target — the property tests and the rejection-class tests are what actually
give confidence — but a ratchet stops silent erosion. `replay`, `ot`, and
`text` should sit near-total; `main` and `http` will not, and that is expected.

**Benchmarks.** "Maximum performance" is unfalsifiable without a workload, so we
define three and track them with `criterion` under `rust/benches/`: a linear
history (1,000 sequential patches, one file each) to measure the fast path; a
divergent history (two branches of 500 patches merged) to measure OT and
base-tree memoization; and a wide tree (10,000 files, small patch) to measure
scan and materialization. Each optimization in §4 must show a number on one of
these before it is kept. Optimizations that do not move a benchmark get reverted
— they are pure risk.

**Testability constraints these impose on the design.** Two are worth naming
now, because retrofitting them is expensive:

- `present` must take stream TTY-ness as an injected parameter, not call
  `isatty` internally. Otherwise §11's required `auto`-selection test cannot be
  written without a PTY, which is exactly what the harness lacks.
- `worktree` and `http` must take their filesystem root and listener as
  parameters. This lets replay, materialization, and the HTTP client be tested
  against in-memory or temp fixtures instead of real global state.

**Extracting goldens from the suite** (§8.5) needs to read YAML from Rust. Use a
YAML crate as a **dev-dependency** — it never touches the shipped binary — or,
if we prefer to keep even dev-deps thin, a small build-time script that emits a
generated Rust fixture file. Either is fine; it must be a deliberate choice
rather than discovered at milestone 14.

**The two things the YAML suite structurally cannot test**, both required by
§11: `SNAP_COLOR=auto` TTY detection on stdout and stderr *independently* (the
harness pipes both, and sets `NO_COLOR=1` in its base environment), and the
`https://` operand path (no test in the suite references https at all — see
§9 risk R1).

Harness environment facts that will otherwise cost debugging time: the
candidate is spawned directly with no shell, with only `PATH` inherited plus
`HOME` and `TMPDIR` pointed inside the sandbox, `NO_COLOR=1`, `LANG=C`,
`LC_ALL=C`. Processes are detached so timeouts can kill the group — so
`--serve` must genuinely handle SIGINT/SIGTERM and exit 0 rather than rely on
being killed. Streams are decoded as UTF-8 with `fatal: true`, so any invalid
byte on stdout or stderr fails the step outright.

---

## 6. Build order

### 6.1 Why the obvious ordering does not work

The YAML suite is end-to-end. Almost every file drives `init`, `config`, and
`commit` before it reaches its actual subject, so **YAML files cannot gate early
milestones**. Extracted from the suite, the commands each file needs:

| Needs only | Files |
| --- | --- |
| `init` | 01, 02 |
| `+ status` | 15, 23, 27 |
| `+ config`, `commit` | 03 |
| `+ log` | 04 |
| `+ diff` | 05, 06, 08, 25 |
| `+ revert` | 07, 19 |
| `+ merge` | 09, 10, 11, 16, 17, 18, 20, 21, 22, 26 |
| `+ --serve` | 12, 13 |
| every command, incl. `--version` and unknown-command handling | 14, 24 |
| every command, in terminal mode | 28 |

An earlier draft of this plan gated milestone 0 on `14-cli-errors` and
`24-cli-grammar-matrix`; both in fact exercise every command in the product,
including `merge` and `--serve`. It also gated milestone 1 on
`21-version-algebra` (needs `merge`), milestone 3 on
`25-config-version-path-boundaries` (needs `commit` and `diff`), and milestones
6-7 on `09`/`22`/`18` (all need `merge`). Those gates were unachievable in that
order. The table above replaces them.

### 6.2 Consequence: two kinds of gate

- **Rust tests gate each milestone.** They are the only thing that can give
  fine-grained, early, per-feature signal, and they are what makes the internals
  auditable. A milestone is done when its unit and property tests pass.
- **YAML files gate each *tier*.** A file becomes available only once every
  command it invokes exists. We do not chase a YAML file before its tier.

This inverts the earlier plan's emphasis and is the right way round: the
acceptance suite is a conformance check, not a development loop.

### 6.3 Milestones

| # | Milestone | Rust-test gate | YAML unlocked |
| --- | --- | --- | --- |
| 0 | `rust/` skeleton: lib+bin, error type and exit-code mapping, arg grammar, `present` skeleton, `--version` | grammar tests over an arg-vector fixture table | — |
| 1 | `version` | parse/format round-trip, four-way compare, join semilattice laws, Snap-order totality | — |
| 2 | `json`, `b64`, `model`, canonical writer | strict-rejection tests, byte round-trip over every repository fixture in the suite | — |
| 3 | `validate` — the six passes | one test per rejection class | — |
| 4 | `worktree` scan, `config`, `init`, `status` (empty-history only) | path validation, scan rejection of symlink/FIFO, config precedence | **01, 02, 15, 23, 27** |
| 5 | `text` — tokenize, diff, edit scripts | tokenizer edges, diff goldens, `apply(diff(a,b),a)==b` property | — |
| 6 | `commit`, `log` | end-to-end in-process repository construction | **03, 04** |
| 7 | `diff` command incl. `--repo` | unified-block rendering, binary and absent-side cases | **05, 06, 08, 25** |
| 8 | `replay` — selection, ordering, base-tree memoization, linear fast path | ordering tests, prefix-snapshot property test, differential self-check | — |
| 9 | `revert` | additive-revert and no-op-error tests | **07, 19** |
| 10 | `ot` + path-level + namespace rules, warning set | each transform row, each path rule, each namespace direction | — |
| 11 | `merge` | import idempotence/commutativity/associativity property tests | **09, 10, 11, 16, 17, 18, 20, 21, 22, 26** |
| 12 | `http` server and client | self-signed TLS integration test, 404/405/redirect cases | **12, 13** |
| 13 | full CLI grammar sweep | — | **14, 24** |
| 14 | `present` terminal mode | goldens extracted from `28`, TTY-selection tests | **28** |

Milestone 4 is the first end-to-end vertical and the first real signal. Getting
there fast matters more than getting any single module perfect, because until
`init` and `status` run, nothing in the suite can be executed at all.

Milestones 10, 11 and 14 hold most of the risk.

## 7. Details that silently fail the suite

Collected from the spec because each one is a plausible wrong guess:

- Diff **deletion on tie**: choose `delete` when `D(i+1,j) <= D(i,j+1)`. The
  `<=` is load-bearing for repeated lines.
- The **`Q insert` row has priority** over every other transform row, which is
  what puts concurrent inserts at one cursor into canonical order.
- Edit scripts **consume the entire old token sequence** — no implicit trailing
  retain — and forbid adjacent same-kind operations.
- Replay integrates **one patch at a time**, recomputing the ready set, ordered
  by Snap order of *result* versions, then author bytes, then revision.
- Namespace resolution runs **for the patch as a whole, before** the per-path
  rules, against `C'` = `C` minus the paths this patch deletes.
- `merge` prints only warnings in the joined replay **minus** those already in
  the pre-merge local replay.
- `merge`/`revert` write working files first, then replace `repository.json`
  via a same-directory temp file. `commit` only needs the metadata replacement.
- `NO_COLOR`, even empty, selects the **complete plain presentation** in `auto`
  mode; `SNAP_COLOR=always` overrides it; an invalid `SNAP_COLOR` errors
  *before* command execution and does so in plain form. The `--serve` URL is
  always plain.
- The 4096-byte message limit applies to **user-supplied `commit` messages
  only**; §4.2 explicitly allows generated `revert to <version>` messages to
  exceed it. Enforcing the cap in a shared code path would break `07-revert`
  on a wide frontier.
- Exit codes: 0 success, 1 expected error, 2 unexpected internal failure.
  Results to stdout, warnings and errors to stderr.

---

## 8. Resolutions for the open issues

Each of these keeps 100% spec conformance and the full suite green. Where an
issue cannot be fixed in code, that is stated rather than worked around.

### 8.1 `https://` — adopt TLS as a single, isolated, feature-gated dependency

§7 and §9 make `https://` a MUST and no YAML case exercises it, so it cannot be
discovered by testing — only by conformance. TLS cannot be hand-rolled
responsibly.

**Proposal.** Define one `Transport` boundary in `http` with two
implementations: `http://` served by the std-only HTTP/1.1 client, `https://`
by `rustls` + `webpki-roots` behind a cargo feature `tls` that is **on by
default**. The dependency is reachable from exactly one module and one function.
The feature boundary stops TLS types leaking into `model`, `replay`, or `cmd`.

**`--no-default-features` produces a build that does not conform to §9.** It
exists for dependency audit and for confirming the std-only core compiles
alone — it is not a shipping configuration, and it must never be what `verify`
tests. `run` and `run_tests` both invoke plain `cargo build`, so the default
(TLS on) is what the suite exercises. This is worth stating because "we have a
dependency-free build" reads like a virtue and would be a conformance failure if
anyone shipped it.

**Testing**, since the YAML suite gives us nothing: a Rust integration test
stands up a rustls server with a self-signed certificate and a test-only root
store, and drives `merge` and `diff --repo` against it. That converts an
untested MUST into a covered one.

**Hermetic builds.** `run_tests` invokes `cargo build --quiet`, which on a cold
machine wants the network. Commit `Cargo.lock`, and if we want the build to be
offline-clean, `cargo vendor` into `rust/vendor/` with a checked-in
`.cargo/config.toml`. Costs repository weight, buys a build that never reaches
the network. Recommended but not required — the TypeScript edition has the same
exposure through `npm ci`.

### 8.2 No on-disk cache — win it all inside one process

`.snap/` is closed and every command is a fresh process, so there is no cache to
carry between invocations. The replay must simply be fast the first time. In
descending order of value:

1. **Prefix-snapshot memoization** of base trees (§4.1) — turns §6.2's naive
   O(n²) into O(total patch size) whenever the base is a canonical prefix.
2. **Content interning** — §6.2's two hottest predicates ("identical in `B` and
   `C`", "identical in `C` and `T`") become integer comparisons.
3. **The linear fast path falls out of rule 1** — in a history with no
   concurrency, rule 1 fires for every path of every patch, so no OT, no
   namespace scan, no aggregate diff.
4. **Lazy decoding** — do not base64-decode a `put` body or tokenize a text file
   until a path actually survives into the tree under construction. On a history
   where most files are later deleted or replaced, this skips most of the input.
5. **Single-pass parse with interning done during the parse** — no DOM, no
   second walk. Since §9 ships the whole repository on every remote merge, parse
   throughput *is* the remote-merge cost.
6. **Write only the delta.** §10 requires working files to be updated before
   `repository.json`, but nothing requires rewriting files whose bytes did not
   change. Diff the target tree against the on-disk tree and touch only what
   moved.

**Considered and rejected: a cache under `$HOME/.cache/snap/`.** It would not
break any assertion — no test does `tree_equals` on the home directory — and
keying it by a hash of the canonical repository bytes would be sound under
§6.5. It is rejected anyway: it buys nothing at the repository sizes the suite
exercises, it adds a cache-invalidation failure mode whose symptom is a *wrong
merge result*, and §12 puts concurrent-process safety out of scope, which is
precisely the guarantee a shared on-disk cache would need. Revisit only with a
profile that shows parse and replay dominating on a real repository.

### 8.3 The unproven prefix-snapshot claim — make it self-checking

Property tests over random causal graphs are necessary but only sample the
space. Add a second, stronger net: a debug-only differential mode in which
**every** memoized base tree is also computed by the naive sub-replay and the
two are compared, aborting on mismatch. Run the entire YAML suite once against
a debug build with the check enabled, as a CI job. That validates the
optimization against every history the acceptance suite constructs, not just
against generated ones, and costs nothing in release.

If the claim ever fails, the fallback is memoized sub-replay everywhere:
correct, slower, no behavioural difference.

### 8.4 Byte-identical `repository.json` — make it unrepresentable to get wrong

Rather than testing for determinism after the fact, remove the ways to be
nondeterministic:

- **No hash-based container may appear in the write path.** The canonical writer
  accepts only already-ordered input — sorted slices and `BTreeMap` — so there
  is no iteration order to get wrong.
- **Canonical-by-construction types.** The patch list and frontier are newtypes
  whose only constructors sort and validate. A serializer that cannot receive
  unsorted data cannot emit it.
- The format contains no floating-point values at all, so the classic
  float-formatting divergence does not exist here.
- **Round-trip test at milestone 2**, not at the merge milestone: parse → serialize →
  parse → serialize, assert byte equality, over every repository fixture in the
  suite. Plus an in-process convergence test that builds one repository by two
  merge routes and compares serialized bytes. This moves the failure from a
  confusing `trees_equal` mismatch in a merge test to an obvious failure in the
  serializer's own tests.

### 8.5 ANSI goldens — extract, never transcribe

`28-terminal-presentation` encodes its expectations as `\u001b[...]` escapes
with literal UTF-8 glyphs. Hand-copying them is the single most likely source of
an invisible one-byte error.

**Proposal.** A Rust test reads `tests/28-terminal-presentation.yaml` directly
and asserts our renderer reproduces those exact bytes. The goldens are then
*derived* from the acceptance suite rather than duplicated from it, so they
cannot drift. This reads test fixtures from Rust; it does not import
implementation code into the harness, which is what `AGENTS.md` forbids.

Apply the same trick to error text. The suite pins error strings with 103
`stderr_equals` assertions against only 22 `stderr_contains` and 20
`stderr_matches` — so most messages are exact and must be discovered, not
invented. Extract them mechanically into an error catalogue at milestone 0, populate the
`error` module from it, and implement against it — instead of guessing wording
and rediscovering it as scattered failures across fourteen milestones.

### 8.6 The two risks that cannot be fixed in code

**Email-as-actor-id (RESEARCH R1).** We cannot mint per-clone ids. But the
collision *is* detectable — §3.5 already requires merge to fail before writing —
and `16-dot-collision` pins the message with `stderr_contains`, not
`stderr_equals`. So within §10's one-line error format we may say more than the
required substring: name the colliding dot **and** the cause, that one
contributor id authored different patches in two repositories. That converts a
bare corruption report into an actionable one with no spec change and no test
change. It does not prevent the collision; nothing conformant can.

**Silent text duplication and orphaned inserts (RESEARCH R2/R3).** §6.4 closes
the warning vocabulary at five reasons and states that line OT emits none, so
there is no conformant way to surface these at merge time. The user's recourse
is inspection — `status` and `diff` after a merge — which the spec does provide.
This stays a documented spec proposal.

Neither changes what we build. They are recorded because a reader who has seen
`RESEARCH.md` will otherwise wonder why the implementation does not address
them; the answer is that the spec defines the behaviour and the spec is the
contract.

---

## 9. Risk register

### 9.0 Principal risks

- **R1 — `https://` is specified but untested.** Resolved by §8.1: `rustls`
  behind a default-on `tls` feature, isolated to one module, covered by a
  self-signed-certificate integration test. Open sub-decision: whether to
  vendor dependencies for hermetic offline builds.
- **R2 — the prefix-snapshot claim (§4.1) is inferred, not proven.** Mitigated
  by making it a property test before the optimization is relied on. If it
  fails, the fallback is memoized sub-replay everywhere: correct, slower.
- **R3 — byte-identical `repository.json` across converged repositories.**
  Any nondeterminism in the write path — hash-map iteration order, float
  formatting, unstable sort — surfaces as a `trees_equal` failure in a *merge*
  test, which is a long way from the actual cause. Mitigation: the canonical
  writer is built and unit-tested in milestone 2, and takes ordered input only.
- **R4 — terminal-mode goldens are exact ANSI byte sequences**, including the
  `✓`, `−`, `~`, `●`, `⚠`, `✗` glyphs as UTF-8. Transcription errors here are
  invisible on inspection. Mitigation: build the `S(n, text)` helper once and
  derive every layout from it.

### 9.1 Smaller inconsistencies worth knowing

- **`run` does not rebuild on `Cargo.toml`-only changes.** It compares
  `target/debug/snap` against files under `src/` only. `run_tests --lang rust`
  always runs `cargo build`, so `verify` is safe; a bare `./run` after editing
  only `Cargo.toml` can execute a stale binary. Touch a source file or build
  manually.
- **Extracted error strings are literals, but many messages are
  parameterized** (they embed paths, versions, dots). The extraction in §8.5
  yields concrete expectations, not templates; turning them into format strings
  is manual work and a place to introduce a wording drift. Keep the extracted
  literals as the assertions and derive the templates from them, not the
  reverse.
- **`cargo clippy` cleanliness needs a defined bar.** Pin it: `-D warnings` with
  `clippy::pedantic` advisory rather than denied. Undefined "clean" is not an
  auditable criterion.

---

## 10. Definition of done

- `./verify --lang rust` reports all 28 cases passing.
- `cargo test` passes, including the property suite.
- `cargo clippy -- -D warnings` is clean.
- Coverage is at or above the ratchet.
- The three benchmarks in §5 have recorded baselines.
- `git status` shows no changes under `tests/` or `test-harness/`.
- The two §11 obligations the YAML suite cannot cover — `auto` TTY detection per
  stream, and the `https://` operand path — are covered by Rust tests.
