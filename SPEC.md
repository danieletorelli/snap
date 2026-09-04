# Snap specification

> A small local version control system with vector-clock versions,
> patch-based history, and deterministic automatic merges.

This document is the proposed **canonical behavioral contract**. Three
implementations—TypeScript, Rust, and Scala—must behave identically against one
public acceptance suite.

The words **MUST**, **MUST NOT**, **SHOULD**, and **MAY** are normative.

---

## 1. Product model

Snap starts every repository from the empty file tree at version `()`. A
repository is a causally ordered set of patches. Each patch:

1. names the exact version on which it was authored;
2. changes regular-file contents; and
3. increments its author's revision counter by one.

A version is a vector clock: a map from contributor ID to the latest revision
by that contributor. It describes a causal frontier, not a branch or one
commit. Merge imports another repository's patches, joins the frontiers, and
deterministically rebuilds the joined state. It creates no merge patch.

Snap resolves every concurrent change automatically. The result is
deterministic, but may discard a desirable effect. Both original patches stay
in history and Snap records path/reason warning facts when whole-file conflict
resolution occurs. The manual-resolution workflow is to edit the merged tree
and commit a corrective patch.

**Binary name:** `snap`

### 1.1 Core invariants

1. A version is a finite vector of nonzero contributor counters.
2. One patch owns exactly one `(contributor, revision)` dot.
3. A patch's complete causal base is present and immutable.
4. Every known version is reproducible from the empty tree and its patches.
5. The same validated patch set always produces the same file tree.
6. Import is set union: idempotent, commutative, and associative.
7. The same dot with different patch values is corruption, not a merge
   conflict.
8. `.snap/` metadata is never part of the tracked tree.

Under §3.5's serial-contributor rule, a valid version globally identifies one
causal patch closure and one materialized tree.

---

## 2. Repository and working tree

`snap init [path]` creates `.snap/` beneath an existing or newly created
working directory. The initial version and tracked tree are both empty.

Snap tracks every regular file below the repository root except `.snap/` and
its contents.

- File contents are arbitrary bytes.
- Directories are implicit; empty directories are not tracked.
- Symlinks and other non-regular filesystem entries are unsupported. Snap MUST
  report them and MUST NOT follow them.
- Permissions, ownership, timestamps, and extended attributes are not tracked.

A tracked path is a UTF-8 relative path using `/` separators. It MUST be
nonempty, contain no ASCII control character or backslash, contain no empty,
`.` or `..` segment, and have no first segment equal to `.snap`. Snap performs
no Unicode or case normalization. Paths sort by unsigned lexicographic UTF-8
bytes.

Every tracked tree is **prefix-free by path segment**: if `a` is a file, no
`a/...` path is present. This is validated for every patch's authored result
and enforced during concurrent replay by §6.4.

The **current tree** is the materialization of the repository's current
frontier. The working tree is **clean** when its path/byte map exactly equals
the current tree and contains no unsupported entry. Otherwise it is dirty.

`commit` records a dirty working tree. `merge` and `revert` refuse to replace a
dirty tree. Read-only commands may inspect one.

---

## 3. Versions

### 3.1 Contributor IDs and revisions

A contributor ID is an ASCII email-shaped string. It MUST contain exactly one
`@` with nonempty text on both sides; contain no control character, whitespace,
`,`, `(`, `)`, or substring `->`; and be at most 254 bytes. Snap preserves its
spelling exactly.

A revision is a positive integer no greater than JavaScript's maximum safe
integer, `9007199254740991`. Zero means “no revision” and is omitted.

### 3.2 Canonical syntax

The empty version is `()`. A nonempty version sorts contributors by unsigned
UTF-8 bytes and contains no spaces:

```text
(jdegoes@example.com->2323,vigoo@example.com->239)
```

CLI arguments MUST use this exact form. Duplicate IDs, explicit zeroes,
leading zeroes, overflow, invalid IDs, whitespace, and noncanonical ordering
are errors.

In repository JSON, a version is an ordered array of `[id, revision]` pairs:

```json
[["jdegoes@example.com",2323],["vigoo@example.com",239]]
```

### 3.3 Causal comparison and join

An absent component is zero. For every contributor `c`:

- `V = W` iff every component is equal.
- `V < W` (before) iff every `V[c] <= W[c]` and at least one is strict.
- `V > W` (after) is the converse.
- `V || W` (concurrent) iff `V != W`, `V` is not before `W`, and `W` is not
  before `V`.
- `join(V, W)[c] = max(V[c], W[c])`.

The version type MUST preserve all four comparison outcomes. Concurrency is not
equivalent to before or after.

### 3.4 Snap order

Snap needs an arbitrary total order only to integrate concurrent patches.
Take the sorted union of contributor IDs and lexicographically compare the
counter at each ID. The first unequal counter decides.

This **Snap order** extends causal order, but its ordering of concurrent
versions has no chronological or authorship meaning.

### 3.5 Serial contributor rule

For each contributor, revision `n` has exactly one patch and follows revision
`n-1`. One contributor ID MUST NOT author concurrently in disconnected copies.
If import finds the same dot with structurally different patches, the
repository is corrupt and merge fails before writing.

This is a deliberate limitation of using an email address as the vector-clock
writer ID. Snap cannot automatically repair such a collision.

---

## 4. Repository and patch format

### 4.1 Files

Every repository has this interoperable layout:

```text
.snap/
  repository.json
  config.json          # optional local configuration
```

`.snap/repository.json` contains the complete repository value:

```json
{
  "format": 1,
  "frontier": [["alice@example.com",1]],
  "patches": [
    {
      "author": "alice@example.com",
      "revision": 1,
      "base": [],
      "message": "add greeting",
      "changes": [
        {
          "type": "text",
          "path": "hello.txt",
          "edit": [{"insert":["hello\n"]}]
        }
      ]
    }
  ]
}
```

The example is pretty-printed for readability. Readers accept ordinary JSON
whitespace and object-key order. Valid input has unique object keys. The parsed
typed value—not its serialized bytes—is authoritative. Writers SHOULD use
two-space indentation and a trailing LF so repositories remain pleasant to
inspect.

Unknown fields, non-integer numbers, and invalid typed values are errors.
`patches` contains exactly the causal closure of `frontier`, sorted by author
and then numeric revision, with no unreachable patches.

A version is **known** (or **materializable**) in a repository when it is
syntactically valid, every patch `(c, n)` selected by `n <= V[c]` exists, and
that selected set contains the complete base of every selected patch. A vector
need not equal the repository frontier or one patch result, but `diff` and
`revert` reject it unless it is known by this definition.

### 4.2 Patch identity and result

A patch's dot is `(author, revision)`. For base `B`:

```text
revision = B[author] + 1
result   = B with result[author] = revision
```

All other result components equal the base. One patch therefore increments one
contributor. A merge may advance several components by importing several
patches.

Patches with the same dot are duplicates only when their parsed typed values
are structurally equal. Different values at one dot are corruption.

`message` is a nonempty UTF-8 string. It may contain tab and LF but no other
ASCII control character. `snap commit` limits user-supplied messages to 4096
bytes; generated revert messages may be longer because they contain a complete
version.
`changes` is nonempty, sorted by path, and contains at most one change per path.

### 4.3 Change variants

**Text create or edit:**

```json
{"type":"text","path":"notes.txt","edit":[{"insert":["hello\n"]}]}
```

**Atomic create or replacement:**

```json
{"type":"put","path":"image.bin","content":"AAEC"}
```

`content` is standard padded RFC 4648 base64. It may contain any file bytes.

**Delete:**

```json
{"type":"delete","path":"obsolete.txt"}
```

A text or put creation requires the path to be absent in the patch's exact base
tree. An edit, replacement, or delete requires it to be present. A change that
does not alter path existence or bytes is invalid, except that an empty text
edit may create an empty file.

### 4.4 Text tokens and edit scripts

A file is text when its bytes are valid UTF-8 and contain no NUL. Split it
immediately after every LF byte, retaining LF in the token. For example,
`"a\r\nb"` becomes `"a\r\n"`, `"b"`. The empty file has no tokens.

An edit script is an array of these one-key operations:

- `{"retain": n}` copies `n` old tokens;
- `{"delete": n}` consumes and removes `n` old tokens;
- `{"insert": [s...]}` inserts one or more nonempty text tokens.

Counts are positive safe integers. Adjacent operations of the same kind are
forbidden. The script MUST consume the complete old token sequence; there is no
implicit trailing retain. Applying it MUST produce exactly the canonical token
sequence of the result: every token except possibly the final one ends in LF,
and no token contains LF before its final byte.

An empty script is valid only when creating an empty text file.

### 4.5 Repository validation

Before using a repository, Snap validates:

1. its exact schema and all versions, IDs, paths, messages, and changes;
2. patch sorting, one value per dot, and contiguous contributor revisions;
3. every patch's complete base closure and `revision = base[author] + 1`;
4. acyclic causality;
5. every change against its materialized exact base; and
6. deterministic replay of the declared frontier.

If no ready patch remains before replay is complete, the history has a cycle or
missing dependency. Validation fails. Snap never fuzzily applies a patch.

---

## 5. Canonical text diff

Patch creation, displayed diffs, and OT use one deterministic token diff.
Given old tokens `A` of length `n` and new tokens `B` of length `m`, define
`D(i, j)` as the minimum inserts/deletes needed to transform `A[i..]` into
`B[j..]`:

```text
D(n, m) = 0
D(i, m) = n - i
D(n, j) = m - j

if A[i] == B[j]:
  D(i, j) = D(i + 1, j + 1)
else:
  D(i, j) = 1 + min(D(i + 1, j), D(i, j + 1))
```

Walk from `(0, 0)`:

1. Equal tokens produce `retain 1`.
2. Otherwise choose `delete 1` when
   `D(i + 1, j) <= D(i, j + 1)`.
3. Otherwise choose `insert [B[j]]`.
4. At an exhausted side, insert or delete the remaining tokens.
5. Coalesce adjacent operations of the same kind.

This recurrence and deletion-on-tie rule define the output. Implementations MAY
use Myers, Hirschberg, or another optimization only if it produces the same
script, including for repeated equal lines.

---

## 6. Deterministic replay and OT

### 6.1 Selecting and ordering patches

To materialize version `V`, select every patch `(c, n)` where `n <= V[c]`.
The set must contain every selected patch's base.

Start from the empty tree. Repeatedly find patches whose bases are fully
integrated, choose the least ready patch by:

1. Snap order of their result versions;
2. unsigned UTF-8 order of author; then
3. numeric revision.

Valid histories normally decide at the first key. Integrate that one patch,
recompute the ready set, and repeat. This puts causal dependencies before
concurrent patches.

### 6.2 Integrating one patch

For incoming patch `P`, materialize its exact base tree `B`. Let `C` be the
canonical tree built so far. It contains `B` plus only earlier concurrent
effects.

First resolve namespace conflicts for the patch as a whole. Let `S` be the
paths that `P` makes present, and let `C'` be `C` with every path that `P`
authored as a deletion removed. If a path in `S` has a different current
ancestor or descendant in `C'`, mark the incoming path for installation as its
authored result `T` and mark every conflicting current path for removal. Each
removed path emits `namespace-wins`. These decisions override the per-path
rules below. The authored result is prefix-free, so two paths in `S` cannot
conflict; duplicate removals and warnings collapse. Form the patch's target
tree by removing the union of marked current paths and then installing every
marked authored result simultaneously with all other resolved path changes.

For each path changed by `P`, let `T` be the authored result of applying that
change to `B`. For paths not settled by the namespace rule, evaluate every path
against the same `B` and `C`:

1. If the path is identical in `B` and `C`, apply the authored change directly.
2. If the path is identical in `C` and `T`, keep it unchanged. This collapses
   identical concurrent changes before OT rather than duplicating their effect.
3. If `B`, `C`, and `T` are text and `P` is a text change, derive the aggregate
   context edit `Q = diff(B, C)` with §5, transform `P` through `Q` by §6.3,
   and apply it to `C`.
4. Otherwise use §6.4's path-level rules.

Apply all resulting path changes from one patch together to form the next
canonical tree. Installation removes files that block required directories,
creates required directories, writes target files, and removes newly empty
directories so the filesystem represents exactly that target path/byte map.

### 6.3 Transforming a text edit

Transform incoming edit `P` so it applies after aggregate context edit `Q`.
Process both streams left to right, splitting counts as needed:

| Next operations | Output in transformed `P` | Consumption |
| --- | --- | --- |
| `Q insert` | `retain(length(Q insert))` | Q only |
| `P insert` | same `P insert` | P only |
| `P retain`, `Q retain` | `retain(min)` | both |
| `P delete`, `Q retain` | `delete(min)` | both |
| `P retain`, `Q delete` | nothing | both |
| `P delete`, `Q delete` | nothing | both |

`length(Q insert)` is its token count. The `Q insert` row has priority.
Concurrent inserts at one cursor therefore appear in canonical integration
order. Deletion consumes only base tokens, so concurrent inserted text
survives. Both scripts consume the same base token count; continue until both
streams end, processing a trailing insertion with its applicable row. No
unmatched retain or delete can remain. Coalesce adjacent output operations.

Snap performs this transform once against the aggregate context edit, not once
per historical patch.

### 6.4 Path-level rules

For one incoming change, let `B` be the base path, `C` the current canonical
path, and `T` the incoming authored result. Resolve in this order:

1. If `C` and `T` are identical, keep `C` and emit no warning.
2. If `T` is absent, the incoming delete wins (`delete-wins`).
3. If `B` is present and `C` is absent, the earlier concurrent delete wins
   (`delete-wins`).
4. If `B` is absent and `C` and `T` are present, the incoming (canonically
   later) create wins (`later-create-wins`).
5. If the incoming change is `put`, the incoming atomic replacement wins
   (`later-put-wins`).
6. Otherwise `P` is text and `C` is non-text, so the incompatible current
   content wins (`put-wins`).

Changes to unrelated paths commute. “Later” always means canonical integration
order, never wall-clock time.

Rules that discard a whole effect add one warning pair:

```text
(<path>, <delete-wins|later-create-wins|later-put-wins|namespace-wins|put-wins>)
```

Replay returns the set of unique warning pairs sorted by path, then reason.
Line OT emits no warning. Merge prints only pairs present in the joined replay
but absent from the pre-merge local replay, one per line:

```text
warning: auto-resolved <path>: <reason>
```

### 6.5 Guarantee

The same valid patch set and frontier MUST produce the same bytes and warning
set in every implementation. Re-merging the same history is a no-op, and merge
direction cannot change the joined result.

Snap does not guarantee intention preservation or desirable merged text.

---

## 7. Commands

Snap locates the nearest repository by walking from the current directory to
the filesystem root. A repository operand is an explicit `http://` or
`https://` URL, or otherwise a local path to a repository root.

The command examples in this section show stable **plain mode**. Non-TTY
streams use plain mode by default, as do all streams when presentation is
disabled; `SNAP_COLOR=always` is the documented exception. §7.11 defines the
richer terminal presentation.

Options occur exactly in the positions shown below and may appear at most once.
Unknown options, extra operands, and missing option values are errors. Local
repository operands resolve against the process working directory.

### 7.1 `snap init [path]`

- `path` defaults to `.` and is created if absent.
- Creates an empty `.snap/repository.json`.
- Existing working files remain uncommitted.
- Reinitializing a repository is an error.
- Initializing a target inside an existing repository is an error.
- Prints `()`.

### 7.2 `snap config [--global] contributor.id <id>`

- Validates the ID before writing.
- Without `--global`, writes `.snap/config.json` in the nearest repository.
- With `--global`, writes `$HOME/.snapconfig.json` and needs no repository.
- Preserves no unknown fields and prints nothing on success.

### 7.3 `snap status`

Prints the current version and working changes sorted by path:

```text
version (alice@example.com->3)
A notes.txt
D old.txt
M src/main.ts
```

Codes are `A` for absent-to-present, `M` for changed bytes, and `D` for
present-to-absent. A clean repository prints only the version line.

### 7.4 `snap log`

Prints patches in reverse canonical integration order, one tab-separated line
each:

```text
(alice@example.com->1)\talice@example.com\tadd greeting
```

The first field is the patch's result version. In messages, backslash, tab, and
LF are escaped as `\\`, `\t`, and `\n` in that order.

### 7.5 `snap commit <message>`

- Requires contributor configuration and a dirty working tree.
- Rejects a message longer than 4096 UTF-8 bytes.
- Diffs the complete current tree against the complete working tree.
- Creates one patch based on the current frontier and increments the configured
  contributor's revision.
- Uses a text change when the new content is text and the old path is absent or
  text. Otherwise it uses `put`; removed paths use `delete`.
- Atomically replaces `repository.json` and prints the new version.
- A clean tree, invalid message, overflow, or dot collision is an error.

```console
$ snap commit "hello"
(alice@example.com->1)
```

### 7.6 `snap diff`

With no arguments, compares the current tree with the working tree.

`snap diff <old> <new>` compares two locally known versions. Add
`--repo <repository>` to resolve `old` locally and `new` in another local or
HTTP repository without importing it:

```text
snap diff <old> <new> [--repo <repository>]
```

Validate every repository and version before producing output. For a
cross-repository diff, also compare every dot present in both repositories and
fail as corrupt if its parsed patch values differ. Changed paths sort by path.
For each text path, print one whole-file unified-style block:

```text
--- a/<path>
+++ b/<path>
@@ -1,<old-token-count> +1,<new-token-count> @@
 <retained token>
-<deleted token>
+<inserted token>
```

For an absent side, use `/dev/null` in its header. Emit operations from §5's
canonical script. A token without final LF is followed by LF and then:

```text
\ No newline at end of file
```

For a binary change, print one line:

```text
Binary files a/<path> and b/<path> differ
```

Again substitute `/dev/null` for an absent side. No differences means no
stdout and success.

### 7.7 `snap revert <version>`

- Requires contributor configuration, a clean working tree, and a locally
  known target version.
- Diffs the current tree to the target tree and authors one new patch with
  message `revert to <version>`.
- Installs the target contents, updates the repository, and prints the **new**
  version.
- If current and target trees are equal, it fails with
  `snap: target tree is already current`.

Revert never removes patches or moves the frontier backward.

### 7.8 `snap merge <repository>`

- Requires a clean working tree, but no contributor configuration.
- Loads and validates the other repository.
- Unions the patch sets and joins the frontiers.
- Canonically replays, installs the result, and updates `repository.json`.
- Creates no patch and increments no revision.
- Prints new warnings from §6.4 to stderr and the joined version to stdout.

Merging equal or already-contained history succeeds, changes nothing, emits no
warnings, and prints the unchanged version.

### 7.9 `snap --serve [port]`

- Validates and snapshots the current repository at startup.
- Binds only to `127.0.0.1`; port defaults to `8765`, while `0` asks the OS to
  select one.
- Prints and flushes `http://127.0.0.1:<actual-port>/repository.json`.
- Serves the startup snapshot until SIGINT or SIGTERM, then exits 0.

### 7.10 `snap --version`

Prints `snap <semver>` without locating a repository.

### 7.11 Terminal presentation and color

Snap has two output presentations. **Plain mode** is the byte-stable interface
specified by §§6.4, 7.1–7.10, and 10. **Terminal mode** adds semantic color,
symbols, and spacing for interactive use. Selecting a presentation MUST NOT
change command execution, repository or filesystem effects, warning selection
or order, or exit status. Terminal mode MAY add labels or rearrange an
equivalent human rendering only as specified below.

`SNAP_COLOR` controls presentation:

| Value | Behavior |
| --- | --- |
| unset or `auto` | terminal mode independently on stdout or stderr when that stream is a TTY, unless `NO_COLOR` is present |
| `always` | terminal mode on both streams, even when redirected; overrides `NO_COLOR` |
| `never` | plain mode on both streams |

Snap treats `NO_COLOR` conservatively: its presence, including an empty value,
selects the complete plain presentation in `auto` mode rather than suppressing
color alone. `SNAP_COLOR=always` is an explicit Snap-specific override.

Any other value is an error before command execution:

```text
snap: SNAP_COLOR must be auto, always, or never
```

This error itself is plain because no valid presentation was selected. The
`--serve` startup URL always remains plain so it can be copied or consumed by a
client without stripping terminal escapes.

Terminal mode uses ANSI SGR sequences. Define `S(n, text)` as `ESC[`, the
decimal code `n`, `m`, `text`, then `ESC[0m`. Codes are bold `1`, dim `2`, red
`31`, green `32`, yellow `33`, magenta `35`, and cyan `36`. All spaces shown
below are literal, every nonempty record ends with LF, presentation adds no
trailing spaces of its own, and empty plain output remains empty.

- Successful `init`, `commit`, `revert`, and `merge` output is
  `S(32,"✓") + " " + S(1,label) + " " + S(36,version) + LF`, where `label`
  is respectively `Initialized repository`, `Committed`, `Reverted`, or
  `Merged`.
- `status` begins with `S(1,"Snap status") + "  " + S(36,version) + LF + LF`.
  A clean tree appends `"  " + S(32,"✓") + " Working tree clean" + LF`.
  Each dirty row is `"  " + S(color,symbol) + " " + path + " " +
  S(2,"(" + label + ")") + LF`, using `(color,symbol,label)` values
  `(32,"+","added")`, `(31,"−","deleted")`, or
  `(33,"~","modified")`.
- Each `log` entry is `S(36,"●") + " " + S(1,message) + LF + "  " +
  S(36,version) + " " + S(2,"by") + " " + S(35,author) + LF`. Entries have
  one additional LF between them. `message` is the escaped one-line message
  from §7.4.
- `diff` preserves every plain byte except that the complete text of each
  matching line, excluding LF, is wrapped by the first applicable style:
  `--- ` or `+++ ` uses `1`; `@@ ` uses `36`; `-` uses `31`; `+` uses `32`;
  `\ ` uses `2`; and `Binary files ` uses `33`. Other lines are unchanged.
- `--version` is `S(1,"snap <semver>") + LF`.
- A plain warning `warning: <detail>` becomes
  `S(33,"⚠") + " " + S(33,"<detail>") + LF`. A plain error line `<error>`
  becomes `S(31,"✗ " + <error>) + LF`.
- `config` remains silent, and the `--serve` startup URL remains plain.

Exact ANSI output is part of the shared acceptance suite. Color is never the
only signal: terminal mode retains words, path labels, and diff prefixes, so
the output remains understandable without color perception.

---

## 8. Configuration

Configuration is ordinary UTF-8 JSON with exactly this shape:

```json
{"contributor":{"id":"alice@example.com"}}
```

Snap reads and validates local `.snap/config.json` first. If it provides an ID,
Snap does not read global configuration. Otherwise it reads
`$HOME/.snapconfig.json`. A missing file means no value; a malformed file,
non-unique or unknown field, or invalid ID in a file that is read is an error.
If `$HOME` is absent, global configuration is unavailable.

Only `commit` and `revert` author patches and therefore require an ID. If it is
missing they fail with:

```text
snap: contributor.id is required; configure it locally or globally
```

---

## 9. HTTP repository

`snap --serve` supports one fixed resource:

```text
GET  /repository.json
HEAD /repository.json
```

`GET` returns the startup snapshot as JSON with
`Content-Type: application/json; charset=utf-8`. `HEAD` returns the same status
and headers without a body. Other paths return `404`; other methods return
`405` with `Allow: GET, HEAD`.

When a repository operand starts with `http://` or `https://`, Snap performs
one GET of that exact URL, requires status 200, parses the body as a repository
value, and validates it normally. HTTP is read-only. Authentication,
authorization, caching, redirects, and concurrent server updates are out of
scope.

---

## 10. Mutation and failures

For `merge` and `revert`, Snap MUST complete parsing, repository validation,
replay, dirty-tree checks, and target-tree construction before writing.
Validation failures cause no mutation.

Any command that scans the working tree fails on a symlink or other unsupported
entry rather than following or silently ignoring it.

During mutation, Snap updates working files first and replaces
`repository.json` through a same-directory temporary file only after the
working-tree update succeeds. `commit` only needs the metadata replacement
because the desired files are already present.

An I/O failure or process interruption during a multi-file update may leave a
dirty, partially updated working tree with the old `repository.json`. Snap
reports the failure; the user may repair the files and retry. Concurrent Snap
processes, crash recovery, and durability under power loss are out of scope.

Output is UTF-8 with LF line endings and follows §7.11's selected presentation.
Results go to stdout; warnings and errors go to stderr. Success exits 0,
expected errors exit 1, and unexpected internal failures exit 2. In plain mode,
errors are one line:

```text
snap: <detail>
```

---

## 11. Required acceptance tests

Acceptance coverage must include:

1. canonical version parsing, four-way comparison, Snap order, and join laws;
2. JSON schema, closure, cycle, gap, dot-collision, path, and patch validation;
3. golden canonical diffs, especially repeated lines and deletion ties;
4. every pairwise OT case and at least three concurrent text patches;
5. every path-level winner rule, identical no-warning outcomes, namespace
   collisions such as concurrent `a` and `a/b`, and exact warning order;
6. replay convergence across patch/import permutations;
7. status, log, commit, local/remote diff, additive revert, and no-op errors;
8. dirty-tree refusal and validation-before-mutation;
9. local-over-global configuration and missing identity;
10. local and HTTP merge, server snapshot behavior, and exact CLI output;
11. for maintainers with all three reference implementations, cross-language
    repository exchange in every producer/consumer pairing;
    and
12. redirected plain output, `SNAP_COLOR`/`NO_COLOR` precedence, and exact
    bytes for every terminal layout family.

The shared harness captures candidate streams through pipes and provides no
portable PTY operation, so it does not exercise `auto` when a stream is a TTY.
Each implementation MUST additionally unit-test `auto` selection for TTY and
non-TTY stdout and stderr independently.

Property tests SHOULD generate valid causal patch graphs and verify that import
permutations produce the same joined frontier, patch set, warnings, and tree.

The public YAML harness evaluates one implementation at a time.

---

## 12. Explicitly out of scope

Snap v1 has no:

- branches, tags, staging area, partial commits, amend, rebase, cherry-pick, or
  history rewriting;
- checkout that moves the frontier backward;
- unresolved conflicts, conflict markers, or built-in manual merge tool;
- rename/copy identity, directory objects, symlinks, or permission tracking;
- ignore files, sparse trees, or large-file optimizations;
- push, writable server, hosted coordination, or repository discovery;
- authentication, authorization, signing, or encryption;
- object hashes, content-addressed storage, compression, or garbage collection;
- concurrent-process safety, crash recovery, or power-loss durability; or
- guarantee of merge intention preservation.

These omissions keep the capstone focused on causal modeling, deterministic
patch replay, OT, filesystem materialization, and direct repository exchange.
