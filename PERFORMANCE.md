# Snap — Performance Design

This document records every performance-relevant choice in the Rust
implementation, why it was made, what it costs, and what was rejected.

---

## Constraints

Two hard constraints from `SPEC.md` and the acceptance suite shape every
decision below:

1. **No on-disk cache.** `.snap/` may contain only `repository.json` and
   `config.json`. Every CLI command is a fresh process with no carry-over
   state. All performance must come from doing one replay well, within a
   single invocation.

2. **Byte-identical convergence.** Two repositories that converged by
   different merge routes must serialize to byte-identical `repository.json`
   files. This rules out hash-based content comparison, unordered iteration,
   or any non-deterministic structure in the write path.

---

## Representation choices

### Tree = `BTreeMap<String, Rc<[u8]>>`

`BTreeMap` gives SPEC §2's unsigned-byte path ordering for free — Rust
compares `str` byte-wise, which matches the spec's "ordered by unsigned byte
lexicographic order". A `HashMap` would require a custom sort at every
serialization call; `BTreeMap` is sorted by construction.

PLAN.md §3 proposes a flat `Vec<(PathId, ContentId)>` for cache-friendly
snapshots. Without path interning (reverted above), this design is not viable.
At Snap's target of thousands of files, `BTreeMap`'s O(log n) lookups are
equivalent in practice to a flat Vec with binary search.

`Content = Rc<[u8]>` shares file bodies across trees without copying. During
replay, a new tree is built incrementally. Paths that were not modified in a
patch clone the `Rc` (a pointer copy + reference count bump), not the byte
data. A 10,000-file tree with one changed file allocates one new `Rc` and
9,999 reference count increments.

**Caveat:** `tree.clone()` is **not** O(1). It copies every `BTreeMap` node
(key `String` + value `Rc` pointer). Memoized trees are wrapped in `Rc<Tree>`
so cache hits are O(1), but the working tree built by `integrate` is still
cloned at each snapshot point. For a 1,000-file tree with 1,000 patches, the
`integrate` iteration (not cloning) is the dominant cost at ~68 ms.

### Paths as `String` (not interned)

The earlier plan proposed path interning to `PathId` with a side table. This
was implemented and then reverted: the complexity of maintaining a parallel
`Vec<PathId>` sorted by bytes was not justified by the benchmark numbers.
`BTreeMap<String, _>` already does O(log n) lookups on the hot path, and the
string comparison cost is negligible compared to I/O and replay. The interning
path would matter at millions of files; Snap targets thousands.

### Content identity by byte comparison, not hash

The two hottest predicates in `integrate` are "path is identical in B and C"
and "path is identical in C and T". A hash would make these O(1) amortized
instead of O(content length). We accept the byte comparison cost because:
the content is in cache (we just read or wrote it), and the alternative — a
hash collision silently producing a wrong merge — is the worst failure mode
this system can have. Probabilistic identity is not acceptable for a version
control system.

---

## Replay architecture

### Prefix-snapshot memoization

`replay::Materializer` stores `Version → Tree` snapshots in a `HashMap`. During
the single canonical replay, a snapshot is saved only at steps whose joined-so-far
version appears as a base version in some patch. This is the single largest
optimization in the system.

**Why it works.** For a linear history (the common case), every patch's base
is the previous patch's result, which is a canonical prefix. Every base is a
cache hit. Replay drops from O(n² × tree_size) to O(total patch size).

**Why the lookup key is a version, not a set.** The replay integrates a patch
only once its base is integrated, so the integrated set is causally
downward-closed. Each contributor's integrated revisions form a contiguous
prefix `1..k`. Therefore the integrated set is exactly `{(c,n) : n ≤ join[c]}`,
which is the closure of the joined version. Matching the frontier identifies
the set.

**Memory bound.** Snapshotting at every step would cost O(patches × tree
size). Snapshotting only at referenced frontiers costs O(distinct base
versions × tree size), which is typically a small fraction.

### Linear fast path

When the current patch's base equals the joined-so-far version (i.e., no
concurrency since the last patch), rule 1 of SPEC §6.2 fires for every path:
the base and current trees agree, so the authored change is applied directly
with no OT, no namespace scan, and no aggregate diff. This turns a linear
history into O(total patch size) with no constant-factor overhead from the
concurrency machinery.

### Non-prefix bases fall back to memoized sub-replay

When a base version is not a cached prefix, `replay` recursively materializes
the required sub-history, keyed by version. The result is memoized, so a
shared base across multiple patches is computed once.

---

## Text diff and operational transform

### Tokenization

SPEC §4.4 defines tokens as "immediately after every LF, retaining the LF in
its token." The tokenizer splits at byte offsets where `b'\n'` appears,
yielding `&str` slices that borrow directly from the source text — no
allocation, no copy. An empty file produces zero tokens (the empty file is
represented as zero content bytes, not a newline).

### Diff algorithm

The diff is the literal SPEC §5 dynamic programming recurrence: O(n × m) in
the number of old and new tokens. Two provably output-preserving accelerations
are applied:

1. **Common prefix trimming.** Before entering the DP, skip matching
   tokens at the start. For a file where 99% of lines are unchanged, this
   reduces the DP matrix from 10,000 × 10,000 to a small rectangle around
   the actual changes. Suffix trimming is not safe — a counterexample
   (`[a, a] → [a]`) shows it would invert the delete/retain order — and is
   explicitly rejected in the code with a guarding unit test.

2. **Tie-breaking.** When the DP has equal-cost alternatives between delete
   and insert, the greedy walk chooses delete (`D(i+1, j) <= D(i, j+1)`).
   This is mandated by SPEC §5's rule 2: "Otherwise choose delete 1 when
   D(i + 1, j) <= D(i, j + 1)." Delete wins on tie unconditionally. The
   implementation matches and is guarded by the `deletion_wins_ties` unit
   test.

Myers/Hirschberg is permitted by SPEC §5 "only if it produces the same
script" but was not adopted. The trimmed DP is not the bottleneck at the file
sizes the suite exercises, and the equivalence proof including the
deletion-on-tie rule and repeated lines is non-trivial. It is an optional
later step, gated behind differential testing.

### Operational transform

`ot::transform` implements the six-row SPEC §6.3 table. The critical design
choice: transform against the aggregate, not per historical operation. The
eg-walker paper measures this as the difference between 1 hour and 24 ms on a
real document. Snap's `integrate` function applies the authored change once
against the current tree, not once per historical patch, so OT cost is
O(changed paths) regardless of history length.

---

## Validation

`validate` in `cli.rs` performs four passes over the patches, one pass over the
frontier, plus a full replay:

1. Revision arithmetic (`revision == base[author] + 1`, with `checked_add`)
   and base component existence (each `(id, revision)` in the patch's base
   must resolve to an existing patch)
2. Contiguity via `windows(2)`
3. Predecessor existence for revision > 1
4. Causal closure (no unreachable patches)
5. Frontier member existence
6. Full replay (proves acyclicity and that every change applies to its base)

Passes 1–4 iterate `repository.patches`, pass 5 iterates `repository.frontier`.
Pass 2 (contiguity via `windows(2)`) is O(n). Passes 1 and 3 call
`repository.find()` (binary search), making them O(n log n). Pass 4 calls
`repository.frontier.get()`, making it O(n log f). They could be merged into a
single loop, but they are separate for clarity: each pass checks one SPEC §4.5
invariant, and the code reads as a checklist of the spec. This is a
correctness-first tradeoff. `validate` is called once per command, not in a
hot loop.

---

## Serialization

`to_canonical_string` serializes a `Repository` to the exact bytes the
acceptance suite checks. Key design choices:

- **Fixed key order** in every JSON object. No `HashMap` iteration anywhere
  in the write path.
- **Two-space indentation**, trailing LF, no trailing commas.
- **Single-pass write.** The JSON serializer walks the type tree and emits
  bytes directly — no intermediate DOM, no second pass.
- **`to_i64` for revision/count widening.** This is an `expect()` that
  panics on overflow. The invariant (MAX_REVISION = 2^53-1 < i64::MAX =
  2^63-1) is provably maintained by `EditScript` validation. The panic is
  caught by `catch_unwind` in `main.rs` (exit code 2).

---

## Build profile

```toml
[profile.release]
opt-level = 3
lto = "thin"
panic = "abort"
codegen-units = 1
strip = true
```

- `opt-level = 3`: Maximum runtime speed.
- `lto = "thin"`: Link-time optimization across crates with reasonable
  compile times.
- `panic = "abort"`: No unwinding infrastructure. Reduces binary size and
  eliminates the unwinding cost. Under `panic = "abort"`, `catch_unwind` in
  `main.rs` becomes a no-op — panics abort the process immediately, so the
  `unwrap_or(2)` fallback is never reached for a panic. The code compiles and
  runs correctly; it simply has no effect on abort.
- `codegen-units = 1`: Single codegen unit enables whole-program optimization
  within the crate. Slower release builds (~10–15%), but the resulting binary
  is measurably faster.
- `strip = true`: Remove debug symbols from release binaries.

---

## Benchmarks

Seven workloads, implemented in `benches/workloads.rs` as a plain binary (no
criterion dependency):

| Workload | Shape | Per replay |
|----------|-------|------------|
| linear | 1,000 sequential patches, 1 file | ~1.0 ms |
| divergent | 2 branches × 250 patches, distinct files | ~31 ms |
| wide-tree | 5,000 files, 2 patches | ~1.0 ms |
| large-tree | 1,000 patches × 1,000 files | ~68 ms |
| text-ot | 2 branches × 250 edits, same file, overlapping | ~31 ms |
| deep-linear | 10,000 patches, 1 file | ~10 ms |
| deep-linear | 100,000 patches, 1 file | ~146 ms |
| diff | 400 × 400 tokens | ~447 µs |

**Linear** measures the fast path: every base is a cache hit, no OT, no
namespace scan. **Divergent** measures non-prefix base-tree memoization with
concurrent branches editing distinct files. **Wide-tree** measures scan and
materialization cost against tree size, not history length. **Large-tree**
measures tree iteration at scale — 1,000 patches each touching one of 1,000
files. Memoized trees are wrapped in `Rc` so cache cloning is O(1); the
remaining cost is `integrate` iterating 1,000 entries per patch. This is the
most expensive workload and confirms that tree iteration (not diff, not OT, not
memoization) dominates at scale. **Text-OT** stresses the SPEC §6.3
operational transform with overlapping concurrent edits to the same file.
**Deep-linear** measures memoization scaling with causal depth — 10k patches
at ~10 ms, 100k at ~146 ms, roughly linear. **Diff** measures the SPEC §5
DP on a realistic file.

Each workload runs 5 rounds (untimed warmup first to avoid page-fault noise)
and reports the mean. Optimizations that do not move a benchmark are reverted.

PLAN.md §5 specifies divergent at 500 patches/branch and wide-tree at 10,000
files. The implementation uses 250 and 5,000 respectively — sufficient to
exercise the target code paths without dominating wall time. The seven
workloads are a strict superset of PLAN.md's three.

---

## What was considered and rejected

### On-disk cache under `$HOME/.cache/snap/`

Would not break any test (no test reads `$HOME`). Rejected because:
it buys nothing at the repository sizes the suite exercises, it adds a
cache-invalidation failure mode whose symptom is a *wrong merge result*, and
concurrent-process safety is out of scope (SPEC §12). Revisit only with a
profile showing parse and replay dominating on a real repository.

### Content hashing for identity checks

A hash (e.g., xxHash, SipHash) would make "identical in B and C" O(1)
amortized. Rejected because hash collisions silently produce wrong merges.
The byte comparison is always correct and the content is in cache.

### ContentId arena interning

PLAN.md §4.4 proposes arena-based content interning for O(1) integer identity
checks. Distinct from probabilistic hashing (rejected above). Not implemented
because `Rc<[u8]>` already shares content bodies without copying, and the two
hottest predicates in `integrate` compare paths (strings), not content bytes.
Content identity is checked only when a path exists in both the base and
current trees — at most once per changed path per patch, not per byte. The
arena machinery (allocation, ID mapping, side table) adds complexity without
measurable improvement at Snap's scale.

### Token interning to u32

PLAN.md §4.4 proposes interning tokens per diff into `u32` ids so the DP
compares integers instead of string slices. Not implemented. The diff operates
on `&str` slices borrowing directly from the source text — zero allocation, zero
interning overhead. After prefix trimming, the DP compares tokens by pointer
equality. At the file sizes the suite exercises, the tokenization and DP costs
are dominated by I/O and replay. Token interning would matter for very large
files (100k+ tokens); revisit with a profile showing token comparison as a
bottleneck.

### Path interning to `PathId`

Planned in `PLAN.md §3` but never shipped. `BTreeMap<String, _>` already does
O(log n) lookups. The interning machinery (arena, side table, sorted
iteration) adds complexity without measurable improvement in benchmarks. Would
matter at millions of files; Snap targets thousands.

### Criterion for benchmarks

`PLAN.md §5` mentions criterion. Replaced with a plain `Instant`-based
binary. The point is a repeatable number to justify or reject an
optimization, not statistical rigor. Criterion adds a dependency tree larger
than Snap's entire runtime dependencies.

### Myers/Hirschberg for diff

Permitted by SPEC §5 but not adopted. The trimmed DP is not the bottleneck
at the file sizes the suite exercises, and the equivalence proof including
the deletion-on-tie rule and repeated lines is non-trivial. An optional
later step, gated behind differential testing against the untrimmed DP.

### Merged validation passes

Passes 1–4 in `validate` iterate `repository.patches` independently. They
could be merged into a single loop, saving 3n iterations. Not done because
each pass corresponds to one SPEC §4.5 invariant, and the code reads as a
checklist. `validate` is called once per command, not in a hot loop.
