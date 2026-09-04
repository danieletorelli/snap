# Snap: prior art review

A pre-implementation review of the systems Snap resembles, borrows from, and
diverges from. Read this alongside `SPEC.md`; section references like "§6.3"
point into that spec.

## 0. What Snap actually is, in one paragraph

Strip away the CLI and Snap is a **state-based CRDT whose query function is a
deterministic replay**. The replicated state is a pair: a grow-only set of
patches (each owning exactly one `(contributor, revision)` dot) and a version
vector frontier. Both halves are join-semilattices, so `merge` is a join and is
trivially idempotent, commutative and associative (§1.6). The *value* of that
state — the file tree — is not stored; it is recomputed by totally ordering the
patch set (§3.4 Snap order, §6.1), replaying from the empty tree, and
transforming each patch through a re-derived context diff (§6.3) or, for
non-text and structural cases, through six ordered winner rules (§6.4).
Convergence therefore does not rest on any algebraic property of the transform
— it rests on the determinism of the total order. That is a genuinely
defensible architecture with close prior art (Jupiter's server ordering,
eg-walker's event-graph replay), and this document is mostly about the three
places where it will hurt: replay cost, the choice of an email address as the
vector-clock actor id, and the fact that Snap's most common real-world conflict
— two people editing the same line — resolves *silently*, with no warning.

---

## 1. Comparison table

| System | Version model | Conflict handling | Granularity | Relevance to Snap |
| --- | --- | --- | --- | --- |
| **Snap** | Version vector over email ids; history = unordered patch set with dots | Always auto-resolves: line OT vs. re-derived context diff, else 6 whole-file winner rules + warning | Line (token = text up to and including LF) | — |
| **Darcs** | History = ordered sequence of patches, reorderable by commutation | Merge cannot fail; conflicts become "conflictors"/mergers that record the alternatives | Hunk of lines, plus rename/file primitives | Closest classical VCS to Snap's "history is patches, not snapshots"; also the cautionary tale on merge cost |
| **Pijul** | Set of hash-identified changes; state = pristine graph of vertices/edges | Conflict-tolerant: both sides applied into the graph, conflict *detected* as a graph property afterwards | Line/byte-interval vertices | Shows how to get Snap's "never fail" property while still *reporting* the conflict |
| **Git** | Snapshot DAG; version = commit hash (Merkle) | 3-way merge on `(base, ours, theirs)`; unresolved → index stages + conflict markers, human resolves | Line, via diff3 | The baseline Snap is defined against; its merge-base machinery is exactly what Snap's replay-from-empty avoids |
| **Mercurial** | Snapshot DAG, revlog per file, changeset hashes | 3-way merge; explicit merge changeset with two parents | Line | Per-file history and phases; contrast with Snap's whole-repo patch |
| **Subversion** | Global monotonic integer revision, centralized | 3-way merge with mergeinfo tracking; server is authority | Line | The "single writer id" world Snap's serial-contributor rule half-assumes |
| **Fossil** | Append-only Merkle DAG of immutable artifacts, in SQLite | 3-way merge; history immutable, nothing rewritten | Line | Validates Snap's "history is append-only, revert is a forward patch" |
| **Jujutsu (jj)** | Git-compatible commit DAG + operation log; anonymous branches | **First-class conflicts**: a commit stores an *ordered list of trees* `A+(C-B)+(E-D)`; conflicted state is a legal committed state | Tree/file, materialized to markers on demand | The best counter-model to Snap: keeps determinism *and* records unresolvedness |
| **Sapling** | Lazy commit graph, commits never stripped, hidden/unhidden | 3-way merge; linear-graph bias for monorepo perf | Line | Evidence that graph shape drives operation cost at scale |
| **Automerge** | DAG of changes, each `(actorId, seq)` + hash; version = set of heads | CRDT: all concurrent ops commute by construction; app resolves semantics | Character | Its `(actor, seq)` pair *is* Snap's dot; its actor-id rules are the direct precedent for §3.5 |
| **Yjs** | YATA sequence CRDT, client id + clock | CRDT, integration by origin/right-origin | Character/item, run-length compressed | The performance-tuned end of the CRDT spectrum |
| **Diamond Types / eg-walker** | Event graph (DAG of ops) replayed on demand; critical versions prune replay | Replay + transform, maximally non-interleaving | Character, run-compressed | **Architecturally the same idea as Snap**, with the replay-cost fix Snap lacks |
| **RGA** | Per-character unique timestamps, tombstones | CRDT; total order by timestamp | Character | Source of the tombstone-growth problem Snap sidesteps by not storing per-element ids |
| **Logoot / LSEQ** | Dense position identifiers, no tombstones | CRDT; identifier order decides | Character | Known interleaving anomalies; why "just pick a total order" is not automatically safe |
| **Peritext** | Sequence CRDT + formatting spans anchored to char ids | Deterministic span merge preserving formatting intent | Character + marks | Shows what "intention preservation" costs; Snap explicitly disclaims it (§6.5) |
| **Jupiter / Google Wave OT** | Client op + server op, single server ordering | Transform against the server's ordered history; convergence from the ordering, not TP2 | Character/item runs | Snap's §6.3 is a Wave-style `retain/insert/delete` transform; Snap's total order plays the server's role |
| **ShareDB / ot.js / Etherpad** | Central server, op log | OT with server as sequencer | Character | Practical implementation pitfalls: validation, malformed ops, non-associativity in the wild |
| **Dynamo** | Version vector per key, actor = coordinating node, truncated at ~10 entries | Siblings returned to client; client reconciles; LWW fallback | Whole value | Precedent for vector-clock growth and truncation-induced false conflicts |
| **Riak (classic VV)** | Version vector, actor = client id, then vnode id | Siblings | Whole value | The **sibling explosion** story: per-client actor ids do not work |
| **Riak (DVV)** | Dotted version vector: VV + a per-value dot | Siblings bounded by replication factor | Whole value | Shows what a version vector *cannot* express and what a dot adds |
| **Ficus / Coda / WinFS** | Per-file version vectors (Parker 1983) | Detect mutual inconsistency, then type-specific resolvers or manual | Whole file | The original "version vector over files" design; Snap is a descendant |

---

## 2. Patch-theory version control

This is Snap's closest family: history as a bag of changes rather than a chain
of snapshots.

### 2.1 Darcs

**Model.** A patch is a function between *contexts*, where a context is the set
of patches preceding it; the patch itself is constant, only its representation
changes per context
([Roundy, *Theory of Patches*](https://www.cs.tufts.edu/~nr/cs257/archive/david-roundy/Theory%20of%20patches.html)).
Two primitives do everything: **inversion** (`P^-1` such that `P^-1 P` is the
identity) and **commutation** (reorder `P2 P1` into `P1' P2'` preserving the
combined effect). Merge is defined for two *parallel* patches — patches sharing
a context — and, crucially, **merge cannot fail**: when patches genuinely
conflict, Darcs manufactures a "merger" (Darcs 1) or "conflictor" (Darcs 2)
patch that carries the conflicting alternatives
([Wikibooks, *Patch theory and conflicts*](https://en.wikibooks.org/wiki/Understanding_Darcs/Patch_theory_and_conflicts)).
Conflicting patches mutually cancel, and a resolution is a *third* patch
depending on both.

**Pros over / for Snap.**
- Darcs proved the core Snap thesis two decades early: a repository can be a
  set of changes with dependencies rather than a DAG of snapshots, and users
  can then cherry-pick individual patches (`darcs pull -i`) — the killer
  feature of the model.
- "Merge never fails" is Snap's §6.5 guarantee. Darcs got there by making the
  conflict a first-class patch value; Snap gets there by picking a winner.
- Inversion is why `darcs rollback` is a forward patch, matching Snap's §7.7
  "revert never removes patches or moves the frontier backward".

**Cons.**
- The **exponential merge problem**. Real merges of two-line changes have taken
  hours; the Darcs-1 algorithm is exponential in the size of the conflict, and
  the project's own FAQ concedes "fixing the underlying patch theory problems
  will potentially take us a very long time"
  ([darcs.net FAQ/Performance](https://darcs.net/FAQ/Performance)). Darcs 2's
  conflictors and, later, `darcs rebase` (2.10) are mitigations rather than
  cures.
- The cost is not incidental: it comes from *nested* conflictors — conflicts
  about conflicts — which is precisely the structure Snap refuses to create.
- Commutation over a real filesystem (renames, file creation/deletion) makes
  the algebra fiddly and the failure modes hard to explain to users.

**What Snap should take.** Snap's flat, non-recursive resolution (a conflict
produces a *value*, never a new conflict object) is exactly the property that
buys it linear-ish behaviour where Darcs went exponential. That is a real,
defensible design win and should be stated as such. Conversely, Darcs shows
what Snap gives up: because a Snap version is a vector `n <= V[c]`, a
contributor's patches are always a *downward-closed prefix*. Snap **cannot
represent "I have alice@3 but not alice@2"**, so cherry-pick is not merely
out of scope (§12) — it is unrepresentable without changing the version type
to a dot set. Worth saying out loud in the spec's rationale.

### 2.2 Pijul

**Model.** Pijul replaced Darcs' patch-sequence algebra with a graph. The
repository state ("pristine") is a directed graph `G=(V,E)` whose vertices are
intervals of text, each identified by "the hash of the change that introduced
them, along with a position in that change"; edges are labelled with the change
that introduced them, and deletion re-labels an edge rather than removing a
vertex ([Pijul manual, *Theory*](https://pijul.org/manual/theory.html)). This
is an append-only structure — i.e. a CRDT — and Pijul says so: "Pijul
implements a conflict-free replicated datatype (CRDT)". The theoretical
grounding is Mimram and Di Giusto's *A Categorical Theory of Patches* (ENTCS,
2013), where a merge is a pushout in a category of files and patches
([discussion](https://lobste.rs/s/9ga9cw/theory_pijul_version_control_system)).

**The part Snap should copy: conflicts as detectable graph states.** Pijul does
not stop the merge, but it does not pretend the conflict never happened either.
It defines conflicts structurally:

- two alive vertices with no directed path between them (a genuine
  "both edited here" conflict);
- a cycle in the alive subgraph (an ordering conflict);
- "zombie" vertices — vertices with both alive and dead incoming edges (someone
  edited inside a region someone else deleted).

**Pros over Snap.**
- Same "no information is ever lost, merge never blocks" property, *plus* a
  principled, after-the-fact conflict report. Snap has half of this: it warns
  for whole-file rules but is silent for the line-level case (see §8.3 below).
- Explicit dependency relation: "for any two changes A and B, either A and B
  can be applied in any order, or A depends on B, or B depends on A"
  ([*Why Pijul*](https://pijul.org/manual/why_pijul.html)) — the same trichotomy
  Snap's §3.3 exposes as before/after/concurrent.
- **Associativity is guaranteed**, which Pijul explicitly contrasts with Git.
  Snap's §1.6 makes the same claim and it is the strongest thing in the spec.
- The pristine is a *cache* of applied changes, so applying a new change does
  not recompute history. This is the direct answer to Snap's replay cost.

**Cons.**
- Substantial implementation complexity: an on-disk graph database, hashes,
  vertex splitting, zombie tracking. Far beyond a capstone.
- Vertex ids are `(change hash, offset)`, so the metadata is per-text-interval;
  Snap stores none of this, at the cost of having to re-derive context by
  diffing.

**What Snap can learn.** Two things. First, "conflict-tolerant, resolve later"
is a legitimate third option between Git's blocking and Snap's silent winner —
and Pijul reaches it without Darcs' exponential blowup. Second, Pijul's
"pristine as cache" is the architectural answer to §6's full replay: Snap could
cache the materialized tree per integrated prefix of the canonical order
without changing any observable behaviour.

---

## 3. Snapshot / DAG version control

### 3.1 Git

**Model.** Content-addressed object store; a commit names a tree hash and
parent hashes; a "version" is a commit hash, and history is a Merkle DAG.
Merging is not part of the data model at all: `git merge` computes a merge base
via `git merge-base`, runs a 3-way merge of `(base, ours, theirs)`, and if
hunks overlap it writes conflict markers and leaves stages 1/2/3 in the index
for a human ([git-merge docs](https://git-scm.com/docs/git-merge)).

**Pros over Snap.**
- Hashes give integrity, cheap equality, cheap sync negotiation, and O(1) "do
  we have this?" checks. Snap deliberately has none of this (§12), so
  duplicate-dot detection requires structural comparison of full patch values
  (§4.2) and §9's transport ships the entire `repository.json` every time.
- Rename detection at merge time (similarity heuristics) prevents the
  "edit vs. rename silently loses the edits" failure that Snap's
  delete-wins rule (§6.4 rule 2) will produce.
- Conflict markers, `merge.conflictstyle=diff3`/`zdiff3`, and `git rerere`
  exist because 3-way merge's output is frequently *wrong* and humans need to
  see the base. Snap has no equivalent escape hatch.

**Cons — and where Snap is genuinely better.**
- **Merge is not associative and the merge base can be ambiguous.** With
  criss-cross history there is no unique nearest common ancestor; `-s recursive`
  merges the candidate bases into a synthetic base, and the documented
  consequence is that "merging together multiple things which merge cleanly
  will sometimes give different answers depending on the order in which the
  merges happen" — in a pathological criss-cross a value can flip-flop forever
  without ever producing an unclean merge
  ([revctrl, *CrissCrossMerge*](https://tonyg.github.io/revctrl.org/CrissCrossMerge.html)).
  Snap has **no merge base problem at all**: every patch carries its exact base
  (§4.2), replay always starts from the empty tree, and §6.5 guarantees
  order-independence. This is Snap's single strongest advantage over Git and
  should be a headline acceptance test (§11.6 already is).
- 3-way merge only sees three snapshots and has no notion of temporal ordering,
  so it can silently resurrect or drop changes. Snap's `Q = diff(B, C)` inherits
  the *same* limitation (see §5.3 below) — Snap is not immune here, it is
  merely in the same boat.
- Merge commits inflate the graph; Meta reports that wide, non-linear graphs
  degrade `log` and `blame` in monorepos
  ([Branching in a Sapling Monorepo](https://engineering.fb.com/2025/10/16/developer-tools/branching-in-a-sapling-monorepo/)).
  Snap creates **no merge patch** (§7.8), which keeps the causal structure
  exactly as wide as the real concurrency and no wider. Good call.

### 3.2 Mercurial, Subversion, Fossil

- **Mercurial**: same DAG idea with per-file revlogs and an explicit two-parent
  merge changeset. Its `evolve`/obsolescence-marker work is the acknowledgement
  that "history is immutable" and "users want to amend" are in tension. Snap
  dodges this by having no amend at all (§12).
- **Subversion**: a single global monotonically increasing integer revision,
  because there is exactly one writer of record — the server. This is the
  degenerate case of Snap's vector clock (one contributor). It is worth noting
  that Snap's §3.5 serial-contributor rule quietly imports an SVN-shaped
  assumption (one identity, one linear counter) into a decentralized system.
- **Fossil**: append-only Merkle DAG of immutable artifacts stored in SQLite,
  designed for the "cathedral" model where contributors know each other
  ([Fossil versus Git](https://fossil-scm.org/home/doc/tip/www/fossil-v-git.wiki)).
  Fossil is the closest cultural match to Snap: small trusted group, no
  history rewriting, everything append-only. Its choice to keep the whole
  repository in one queryable file is also close to Snap's single
  `repository.json`. Fossil's counterpoint is that it still *hashes* every
  artifact, precisely to get non-repudiation and tamper detection — which Snap
  forgoes.

### 3.3 Jujutsu (jj) — the most instructive comparison

**Model.** Git-compatible objects underneath, but two ideas matter here.

**First-class conflicts.** A conflicted commit stores "an ordered list of tree
objects linked from the commit (instead of the usual single tree per commit)",
always an odd number, interpreted algebraically: with trees A, B, C, D, E the
content is `A + (C - B) + (E - D)`; an ordinary 3-way merge is `A + (C - B)`
([jj docs/technical/conflicts.md](https://github.com/jj-vcs/jj/blob/main/docs/technical/conflicts.md)).
Conflicts are *simplified* algebraically (canceling terms are removed), so
rebasing a conflicted commit does not produce nested conflict markers, and the
conflicted content is *materialized on demand* rather than stored as markers.
jj also auto-resolves the case where all sides made the identical change —
exactly Snap's §6.2 rule 2.

**Anonymous branches + auto-rebase.** Branches have no names; rewriting a
commit automatically rebases all descendants
([jj glossary](https://docs.jj-vcs.dev/latest/glossary/)). An "operation log"
records every repo mutation atomically, generalizing reflog.

**Pros over Snap.**
- jj demonstrates that you can have Snap's "the merge always completes and
  produces a committable state" property **without** discarding either side.
  The conflict is data, not a marker and not a lost edit. If Snap ever wants a
  seventh outcome besides its five warning reasons, this is the design to copy:
  materialize a conflicted path on demand from an ordered tree list.
- The algebraic simplification rule is what stops jj from becoming Darcs.
- The operation log is a cheap, well-understood answer to Snap's §10
  admission that an interrupted merge can leave a half-updated working tree.

**Cons.**
- Requires a real object store and a notion of tree identity — squarely in
  Snap's §12 exclusions.
- Users still must eventually resolve; jj defers, it does not decide.

**Takeaway for Snap.** Snap's position ("all conflicts auto-resolve, always") is
the *opposite* end of the spectrum from jj, and the spec should own that
explicitly: Snap trades recoverability for the invariant that every version
materializes to exactly one byte-exact tree (§1, closing note). That invariant
is what makes the three-implementation acceptance suite possible, so it is
worth the trade — for a capstone. It would not be worth it for a product.

### 3.4 Sapling

Meta's Sapling emphasises scale: a lazy commit graph that does not need
explicit deepening, commits that are hidden rather than stripped, and
commit-rewriting as a first-class operation
([Sapling: differences from Mercurial](https://sapling-scm.com/docs/introduction/differences-hg/),
[engineering.fb.com announcement](https://engineering.fb.com/2022/11/15/open-source/sapling-source-control-scalable/)).
The relevant lesson for Snap is narrow but real: **graph shape determines
operation cost**. Sapling actively works to keep the graph linear because wide
merge graphs make `log`/`blame` slow. Snap has no merge nodes, so its causal
graph is exactly as wide as real concurrency — but every one of Snap's
operations is `O(all patches)` regardless, so Snap has traded a shape problem
for a constant-factor problem.

---

## 4. CRDTs for text

### 4.1 The family, briefly

- **RGA (Replicated Growable Array)**, Roh et al. 2011: every character gets a
  unique timestamp id; deletion leaves a tombstone so concurrent inserts still
  have an anchor
  ([Replicated abstract data types, JPDC](http://csl.skku.edu/papers/jpdc11.pdf)).
- **Logoot** (2009) / **LSEQ** (2013): dense *position identifiers* between
  neighbours, avoiding tombstones — at the cost of identifiers that can grow
  without bound (LSEQ adds an adaptive allocation strategy with a sub-linear
  spatial bound).
- **Automerge**: a DAG of changes; each change is `(actorId, seq)` plus a hash
  and dependency hashes; a document version is a *set of heads*.
- **Yjs**: YATA-based, run-length-compressed items, aggressive trimming of
  stale metadata.
- **Diamond Types / eg-walker**: event-graph replay (below).
- **Peritext** (Litt, Lim, Kleppmann, van Hardenberg, CSCW 2022): rich text,
  formatting spans anchored to stable character ids so that concurrent
  formatting operations commute and preserve intent
  ([paper](https://www.inkandswitch.com/peritext/)).

### 4.2 Metadata overhead — the reason Snap is right not to be a CRDT

Character-level CRDTs pay per-character metadata. Reported figures: roughly
16–32 bytes per character for typical text CRDTs; Fugue measures about 23 bytes
per character (13 bytes per character-including-tombstones)
([The Art of the Fugue](https://arxiv.org/pdf/2305.00583)). Tombstones are the
worse half: "a document with a history of a million operations and finally
containing a single line can have as much as 499,999 tombstones", and garbage
collecting them requires expensive distributed protocols
([LSEQ](https://www.researchgate.net/publication/262162421_LSEQ_an_Adaptive_Structure_for_Sequences_in_Distributed_Collaborative_Editing)).
The eg-walker paper's blunt summary: "even the best CRDTs available today use
more than 10 times as much memory as OT to view and edit a document"
([Eg-walker](https://arxiv.org/html/2409.14252v1)). Automerge specifically is
noted for large load time and memory because it retains full operation history
"in the style of a version control system"
([crdt-benchmarks](https://github.com/dmonad/crdt-benchmarks)).

Snap stores **zero per-character metadata**. It stores edit scripts and
re-derives context by diffing. That is the right call for a VCS, where the
steady state is "a file on disk that other tools must read", not "a live
in-memory replica".

### 4.3 Eg-walker: the closest architectural relative of Snap

Eg-walker (Gentle, Kleppmann et al., PaPoC/EuroSys 2024) records an event graph
— a DAG of insert/delete operations with parent references — and **reconstructs
document state by replaying the graph**, transforming each event so that
transformed operations apply in topological order
([paper](https://arxiv.org/html/2409.14252v1),
[Loro's writeup](https://loro.dev/docs/concepts/event_graph_walker)). This is
structurally what Snap §6 does, at line granularity, over a file tree.

Two of its findings are directly load-bearing for Snap.

**(a) Naive OT merging is at least quadratic in divergence.** "If the users each
performed n operations since their last common version, merging their states
using OT has a cost of at least O(n²)", because each of one user's operations
must be transformed against all of the other's. The paper reports a real
document taking **one hour** to merge with OT versus **24 ms** with eg-walker.

Snap partially escapes this: §6.3 says explicitly "Snap performs this transform
once against the aggregate context edit, not once per historical patch". So per
integrated patch, the transform is O(size of the aggregate diff), not O(number
of prior patches). That is a genuinely good design decision and it is worth
flagging in the spec as the reason Snap does not inherit OT's quadratic merge.

**(b) You do not have to replay everything.** Eg-walker's key optimization is
the **critical version**: a version that partitions the event graph such that
all events before it happened-before all events after it. At a critical version
the algorithm can discard internal state and replace it with a placeholder,
so on receiving new concurrent events it replays only from "the most recent
critical version that happened before the new events".

**Snap has critical versions and does not use them.** Any version `V` such that
every patch is either `<= V` or `> V` is exactly a critical version. In a
typical Snap repository — a handful of contributors, mostly sequential work
with occasional divergence — critical versions are *frequent*. Snap replays
from `()` unconditionally (§6.1). This is the single highest-value optimization
available, and it is behaviour-preserving: the canonical order restricted to a
downward-closed prefix is a prefix of the canonical order of the whole set
(*inference from §3.4 and §6.1; worth proving before relying on it*).

### 4.4 Interleaving anomalies — Snap probably gets this right

Kleppmann, Gomes, Mulligan and Beresford, *Interleaving anomalies in
collaborative text editors* (PaPoC 2019,
[PDF](https://martin.kleppmann.com/papers/interleaving-papoc19.pdf)) documents
that concurrently inserted runs of text with a well-defined internal order can
be **randomly interleaved character by character**, producing an unreadable
jumble. Logoot and LSEQ exhibit one variant; RGA exhibits another. Eg-walker
and Fugue explicitly target "maximal non-interleaving": concurrent insertion
sequences at the same position are placed one after another.

Snap's §6.3 transform table gives the `Q insert` row priority and consumes the
*entire* `Q` insert run (emitting `retain(length(Q insert))`) before `P`'s
insert can be emitted. Concurrent insert runs at one cursor therefore appear as
whole blocks in canonical integration order — Snap is maximally
non-interleaving at a single cursor. **Caveat (inference, not cited):** this is
a property of the *diff*, not of the algorithm. `Q` is re-derived by §5's
canonical diff, and with repeated lines the deletion-on-tie walk can split what
was semantically one inserted run into several `insert`/`retain` alternations.
Snap's §11.3 already calls for golden diffs "especially repeated lines and
deletion ties"; §11.4 should additionally assert non-interleaving of two
concurrent multi-line insertions at the same position *in the presence of
repeated lines*.

### 4.5 Peritext and intention preservation

Peritext's contribution is a model of *intent* — what a user meant by bolding a
range while someone else typed inside it — and an algorithm satisfying it.
The relevance to Snap is negative but clarifying: Snap §6.5 says it "does not
guarantee intention preservation". Peritext shows that intention preservation
is a separate, provable property that costs stable per-character anchors. Snap
cannot have it without adopting exactly the metadata it excluded. The
disclaimer is honest and should stay.

---

## 5. Operational Transform

### 5.1 Jupiter and Wave: convergence from ordering, not from algebra

Nichols, Curtis, Dixon and Lamping's Jupiter system (UIST 1995,
[ACM](https://dl.acm.org/doi/10.1145/215585.215706)) introduced the design in
which operations are sequenced through a central server and delivered to all
clients in the same order; this is the lineage behind Google Docs, Microsoft
Word Online, Etherpad and Wave.

Google Wave's operation model is very close to Snap's §4.4/§6.3: components are
`retain` / `insert` / `delete` over a positional item sequence; operations
**compose** (the composition of two document operations is itself a document
operation); and the transform function consumes two operations as linear
streams and emits two transformed operations
([Wave OT whitepaper](https://svn.apache.org/repos/asf/incubator/wave/whitepapers/operational-transform/operational-transform.html)).
Wave's critical modification to textbook OT is that clients wait for server
acknowledgement before sending more operations, giving a **single ordering** —
"one state space representing its operation history".

### 5.2 TP1, TP2, and why Snap does not need TP2

- **TP1**: for concurrent `O1, O2`, `O1 ∘ T(O2, O1) ≡ O2 ∘ T(O1, O2)` — the two
  ways of converging two operations agree. Easy to satisfy.
- **TP2**: for three concurrent operations,
  `T(O3, O1 ∘ T(O2, O1)) ≡ T(O3, O2 ∘ T(O1, O2))` — transformation is
  independent of the order in which the other two were applied. **Rarely
  satisfied**; most systems use a control algorithm so TP2 is never exercised
  ([overview](https://en.wikipedia.org/wiki/Operational_transformation),
  [On Consistency of the OT Approach](https://arxiv.org/pdf/1302.3292)).
  Google Docs and Wave do not satisfy TP2; the global ordering is "exactly what
  allows the Wave OT algorithm to avoid the need to satisfy the TP2 constraint"
  ([Think Bottom Up](http://www.thinkbottomup.com.au/site/blog/Google_Wave_Operational_Transform_and_Server_Acknowledgments)).

**This is the most important correctness finding in this document for Snap.**
Snap has no server, but §3.4's Snap order plus §6.1's ready-set selection give
every replica the *same* total order over the same patch set. Snap therefore
never transforms the same pair in two different orders, and never composes
transforms along divergent paths. **Snap does not need TP2, and it does not
even need TP1** — its convergence obligation reduces to: (i) the canonical
order is a deterministic function of the patch set, and (ii) `transform` and
`diff` are deterministic total functions. Property (i) is a property of §3.4
and §6.1 alone; §11.6 already tests it.

The spec should say this explicitly, because a reviewer who knows OT will
otherwise ask "where is your TP2 proof?" and the honest answer — "we replaced
it with a total order, like Jupiter did" — is a strong one. **Risk:** if anyone
later optimizes replay by reusing a cached transform result from a different
integration order, that argument silently evaporates.

### 5.3 The re-derived context edit: Snap's real OT deviation

Wave, Jupiter and ShareDB transform against **the actual concurrent
operations**. Snap transforms against `Q = diff(B, C)` (§6.2 rule 3) — a diff
of two *endpoint states*, recomputed by §5.

Composition of operations and diff-of-endpoints are not the same function.
Insert-then-delete of the same line composes to nothing and is invisible in the
diff; a block move appears as a delete plus an unrelated insert; and with
repeated lines the DP's deletion-on-tie rule may attribute a retain to a
different occurrence than the author touched. Git's 3-way merge has the same
weakness — it "only sees snapshots o, a, and b" with "no notion of temporal
ordering" — and this is a documented source of mis-merges
([diff3 discussion, James Coglan](https://blog.jcoglan.com/2017/05/08/merging-with-diff3/)).

The trade is deliberate and good: it is what makes the transform O(diff size)
rather than O(history), per §4.3(a) above. But it means Snap's §6.3 is better
described as **"rebase a patch onto a re-derived context diff"** than as OT
proper, and §6.5's disclaimer of intention preservation is doing real work.
Recommend saying so in the spec so implementers do not go looking for OT
literature guarantees that do not apply.

### 5.4 Implementation pitfalls from production OT

Etherpad's Easysync/Changeset library remains the canonical reference for a
text OT engine
([Easysync technical manual](https://github.com/ether/etherpad-lite/tree/develop/doc/easysync)).
Two recurring practical lessons from the ShareJS/ShareDB/ot.js line:

1. **Most OT libraries under-validate**, assuming clients send well-formed
   operations, and a malformed changeset corrupts server state. Snap's §4.4
   invariants (no adjacent same-kind ops, script must consume the *complete*
   old token sequence, no implicit trailing retain, result must equal the
   canonical token sequence) are exactly the right defence and are unusually
   strict for a spec of this size. Keep them; they are cheap and they turn a
   silent corruption class into a validation error.
2. **Composition invariants must be checked, not assumed.** Snap's §6.3 note
   that "both scripts consume the same base token count ... no unmatched retain
   or delete can remain" is an invariant that should be asserted at runtime in
   all three implementations, not merely tested.

---

## 6. Vector clocks and version vectors in distributed storage

### 6.1 The lineage

Version vectors for replicated *files* go back to Parker et al., *Detection of
Mutual Inconsistency in Distributed Systems* (IEEE TSE 9(3), 1983), and were
the basis for conflict detection in **Ficus** and **Coda**
([Ficus conflict resolution](https://ant.isi.edu/~johnh/PAPERS/Reiher94a.pdf)).
Snap is squarely in this tradition: one causal vector per repository,
before/after/concurrent detection, then a resolver. Ficus is a useful precedent
because it also auto-resolved with type-specific resolvers and fell back to
user intervention — Snap keeps the resolvers and drops the fallback.

### 6.2 Dynamo: growth and truncation

Dynamo keeps a vector clock per key with the coordinating node as the actor.
Section 4.4 of the paper documents the growth problem and the mitigation: when
the number of `(node, counter)` pairs reaches a threshold — "say 10" — the
oldest pair is dropped, with a stored timestamp to decide which is oldest. The
paper concedes this "can lead to inefficiencies in reconciliation as the
descendant relationships cannot be derived accurately", i.e. it manufactures
false conflicts, while noting the problem had not surfaced in production
([Dynamo paper](https://www.cs.cornell.edu/courses/cs5414/2017fa/papers/dynamo.pdf)).

**Relevance to Snap.** Snap's vector grows monotonically with the number of
contributors who have *ever* committed and is never pruned. That is correct and
Snap **must not** adopt Dynamo-style truncation: truncating a component would
break §4.1's causal-closure requirement and §1.4's reproducibility invariant.
The cost is concrete and should be acknowledged: every patch stores a full base
vector, so `repository.json` is `O(patches × contributors)` in the base fields
alone, versus Git's `O(1)` parent hashes per commit. For a capstone with a
handful of contributors this is fine; for a project with 500 historical
committers it is not. A hash-based parent set would be `O(parents)`; Snap chose
readability over size, which is defensible for a teaching artifact.

### 6.3 Riak, sibling explosion, and dotted version vectors

This is the most directly transferable body of experience.

Riak originally used **client-id version vectors** — one actor per client. That
made vectors grow with the number of clients, forcing pruning (Riak's limit was
"in the 20 to 50 range"). Moving the actor to the **vnode** (the replica)
bounded the size but introduced **false concurrency**: "Vnode Version Vectors
are incapable of tracking causality in a fine grained manner, and these
interleaving writes generate false concurrency", producing **sibling
explosion** — an unbounded pile of conflicting values for one key
([Vector Clocks Revisited, part 2](https://riak.com/posts/technical/vector-clocks-revisited-part-2-dotted-version-vectors/index.html)).

**Dotted Version Vectors** (Preguiça, Baquero, Almeida, Fonte, Gonçalves;
[arXiv:1011.5808](https://arxiv.org/pdf/1011.5808),
[DVV 2012](https://gsd.di.uminho.pt/members/vff/dotted-version-vectors-2012.pdf))
fix this by attaching a **dot** — a single `(actor, counter)` event marker — to
each sibling value alongside the version vector. Reported outcome: DVV
stabilizes at the replication factor (2–3 siblings) where client-id VVs grew
indefinitely.

The general statement of the pitfall, from the DVV line of work: version
vectors with one entry per *server* correctly detect concurrent updates handled
by different servers, but **if concurrent updates are handled by the same
server, there is no way to identify the concurrent values, and typically the
last write prevails.**

**Read that sentence with "server" replaced by "contributor email".** That is
Snap §3.5 verbatim, except Snap cannot even do last-write-wins — it declares
corruption and refuses to merge (§1.7).

Note also the terminology alignment: Snap already calls `(contributor,
revision)` a **dot** (§4.2). That is the same word the DVV literature uses, and
correctly so — the difference is that DVV pairs a *dot per value* with a vector,
whereas Snap requires the dot set to be exactly the downward closure of the
vector. That requirement is what forbids gaps (and therefore cherry-pick, §2.1).

---

## 7. Serial-writer assumptions: what real systems do about §3.5

Snap §3.5: "One contributor ID MUST NOT author concurrently in disconnected
copies. If import finds the same dot with structurally different patches, the
repository is corrupt". The spec is admirably honest that this is "a deliberate
limitation of using an email address as the vector-clock writer ID".

Every mature system in this space resolves the same tension by **decoupling the
causal actor id from the human identity**:

- **Automerge**: the actor id is not a user. The documentation is explicit that
  actor ids "must be unique per concurrent editing context", that you must
  "never use the same actor ID in multiple threads or processes editing
  simultaneously", that this holds "even if you have two different processes
  running on the same machine", that "all changes by a given actor ID are
  expected to be sequential", and that the recommended practice is to let the
  actor id be **auto-generated at random**, maintaining at least one per device
  ([Automerge concepts: documents](https://www.mintlify.com/automerge/automerge/concepts/documents), [ActorId API](https://docs.rs/automerge/latest/automerge/struct.ActorId.html)). Author
  attribution is carried as ordinary application metadata, separately.
- **Riak/Dynamo**: actor = vnode/coordinating node, i.e. a replica, never a
  user.
- **Ficus/Coda**: actor = replica site.
- **Git/Mercurial/Fossil/jj**: no counters at all — parentage is by content
  hash, so two commits by the same author in two clones are simply two distinct
  hashes and merge normally. This is why the problem does not exist there.

Snap's rule is therefore an outlier, and the failure it guards against is not
exotic: **laptop + desktop, or clone + re-clone, is the normal way a single
developer works.** Two commits by `alice@example.com` in two clones both claim
dot `(alice@example.com, 4)`; the repositories are then permanently
unmergeable, with no recovery path in the spec, and the only remedy is manual
re-authoring under a different identity.

**Concrete mitigations, cheapest first (all are inferences/recommendations, not
citations):**

1. **Keep the wire format; change what goes in the id.** The contributor ID
   grammar (§3.1) is "ASCII email-shaped, exactly one `@`, no `,`, `(`, `)`, or
   `->`" — it already permits `alice+laptop@example.com` and
   `alice+7f3a2c@example.com`. Have `snap init` (or first `commit`) mint a
   per-clone suffix by default and reuse the local `.snap/config.json` id
   thereafter. Zero format change, zero acceptance-suite change, and §3.5
   becomes true by construction rather than by user discipline.
2. **Fail loudly and early.** Today the collision is only detected at merge, by
   which point both sides have real work. `snap commit` can additionally warn
   when the configured id came from *global* config (`$HOME/.snapconfig.json`)
   rather than local, since that is the exact configuration that makes the
   collision likely.
3. **If the id must stay human-readable**, document the recovery procedure
   (export the losing side's patches as a working-tree diff, re-commit under a
   fresh id) rather than leaving §3.5 as a dead end.

At minimum, add an acceptance test for the collision path (§11.2 mentions
"dot-collision" — make sure it covers *structurally different patches at the
same dot across two repositories*, not just within one).

---

## 8. Implications for Snap

### 8.1 Design choices the research validates

1. **Merge as a semilattice join with no merge patch (§1.6, §7.8).** Git's
   merge is documented as non-associative under criss-cross history and its
   merge base can be ambiguous; Pijul advertises associativity as a headline
   feature precisely because Git lacks it. Snap gets associativity for free
   from set union + componentwise max. This is the strongest claim in the spec
   — test it hard (§11.6) and state it in the intro.
2. **No merge base computation at all.** Each patch names its exact base
   (§4.2), so Snap never runs anything like `git merge-base`, never has to pick
   among candidate ancestors, and never needs a recursive/`ort`-style synthetic
   base. A whole documented class of Git mis-merges is structurally absent.
3. **Flat, non-recursive conflict resolution.** Darcs' exponential merge comes
   from conflicts about conflicts. Snap's §6.4 always produces a *value*, never
   a new conflict object, so that blowup is impossible by construction. Say
   this in the rationale.
4. **Transform once against an aggregate diff, not once per historical patch
   (§6.3).** Eg-walker shows naive OT merging of diverged branches is at least
   O(n²) and measured an hour-long merge. Snap's aggregate-context design
   avoids inheriting that. This is a real result and deserves a sentence in the
   spec.
5. **No per-element metadata.** CRDT text costs roughly 16–32 bytes per
   character plus unbounded tombstones; Snap costs zero. For a VCS whose
   artifact is "files other tools read", this is correct.
6. **Line granularity.** Matches Darcs' hunks, Git's diff3 and every VCS user's
   mental model; character granularity is a collaborative-editor concern, not a
   VCS one.
7. **Insert-priority tie-breaking that places concurrent insert runs as whole
   blocks.** Aligns with the "maximal non-interleaving" property eg-walker and
   Fugue target, and avoids the Logoot/LSEQ/RGA interleaving anomaly.
8. **Strict edit-script validation (§4.4).** Under-validation is the classic
   production OT bug class. Snap's "must consume the complete old token
   sequence, no implicit trailing retain, result must equal canonical tokens"
   is exactly right.
9. **Append-only history; revert is a forward patch (§7.7).** Fossil and Darcs'
   `rollback` agree. It also keeps invariant §1.4 (every known version is
   reproducible) intact.

### 8.2 Defensible but costly

1. **Replay from the empty tree on every operation (§6.1).** Eg-walker solves
   exactly this with **critical versions** — versions that partition the event
   graph — allowing replay to start from the most recent critical version
   rather than from nothing. Pijul solves it by keeping the pristine as a
   cache. Snap's spec-level behaviour need not change; this is purely an
   implementation optimization, and it is the highest-value one available.
   *Additionally:* §6.2 requires materializing **each incoming patch's exact
   base tree `B`**. Implemented naively that is one full replay per patch —
   O(n²) patch applications. Memoize `B` along the canonical order.
2. **The §5 diff recurrence as written is O(n·m) time and memory.** The spec
   permits Myers/Hirschberg "only if it produces the same script". Someone must
   actually verify that equivalence, including the deletion-on-tie rule and
   repeated lines, or all three implementations will ship the DP and be slow on
   large files. Recommend: pick one reference implementation of the DP, then
   differential-test any optimized variant against it as part of §11.3.
3. **No content hashing (§12).** Costs: duplicate-dot detection requires
   structural comparison of full parsed patch values (§4.2, §7.6); there is no
   integrity check; and §9's transport must ship the entire `repository.json`
   with no delta negotiation, so every merge is O(entire history) over the
   wire. All acceptable for a capstone, all disqualifying for a real tool. Git,
   Fossil, Pijul and Automerge all hash, and they hash for these reasons.
4. **Version vectors are O(contributors) and appear in every patch's `base`.**
   `repository.json` is therefore O(patches × contributors) before content.
   Dynamo hit this and truncated; Snap **must not** truncate (it would break
   §1.4 and §4.1 closure). Just know the bound.
5. **No cherry-pick, and it is unreachable rather than merely unimplemented.**
   Because a version selects patches by `n <= V[c]` (§6.1), a contributor's
   patches are always a downward-closed prefix; "have alice@3, not alice@2" has
   no representation. Darcs and Pijul make selective pulls their headline
   feature. If a future Snap wants it, the version type must become a dot set
   (cf. DVV / dot clouds), not a vector. Worth a sentence in §12 so nobody
   plans a v2 cherry-pick on the current type.
6. **No rename tracking (§12).** A rename is delete + create. Concurrent
   "rename file" and "edit file" resolves via §6.4 rule 2 (`delete-wins`) —
   the edits are dropped, with a warning but no recovery. Git spends real
   effort on similarity-based rename detection precisely because this is
   common. Defensible omission; document the failure mode in user-facing docs,
   not just in the spec's exclusions list.

### 8.3 Genuine risks the research surfaces

**R1 — Email as actor id is the highest-severity issue.** Every comparable
system uses a per-replica or per-session actor and carries human identity
separately: Automerge ("never use the same actor ID in multiple threads or
processes", "one actor ID per device", prefer auto-generated), Riak (client ids
→ vnode ids, after sibling explosion), Dynamo (coordinating node), Ficus/Coda
(replica site). Snap does the opposite, and §3.5's collision is the *expected*
outcome of one developer using two machines, with **no repair path**. The
existing ID grammar already permits `alice+laptop@example.com`, so a per-clone
suffix minted at `init` fixes this with no format or test-suite change. Treat
this as a design bug, not a documented limitation.

**R2 — Line-level OT silently duplicates content on the single most common
conflict.** Trace §6.3 for the canonical case: base line `L`, Alice edits it to
`L_a`, Bob concurrently edits it to `L_b`. The aggregate context edit is
`Q = [delete 1, insert [L_a]]`; Bob's patch is `P = [delete 1, insert [L_b]]`.
Row `P delete, Q delete` consumes both and emits nothing; row `Q insert` emits
`retain(1)`; then `P insert` emits `insert [L_b]`. The merged file contains
**both `L_a` and `L_b`, adjacent, with no marker and — per §6.4, "Line OT emits
no warning" — no warning at all.** Nothing is lost, which is arguably better
than Git, but the user is never told. Pijul detects exactly this shape ("two
alive vertices with no directed path between them") and reports it; jj records
it as a first-class conflicted state. **Recommendation:** emit a warning
(e.g. `text-overlap`) when the transform consumes a `P delete` against a
`Q delete` at the same base tokens, or when `P` and `Q` both touch an
overlapping base token range. This is computable inside the existing §6.3
stream walk, costs nothing, and closes the spec's biggest observability gap.
It does change §6.4's warning set, so decide before writing the acceptance
suite.

**R3 — Concurrent insertions survive inside deleted regions, silently.** §6.3
states it plainly: "Deletion consumes only base tokens, so concurrent inserted
text survives." Concretely: Alice deletes a whole function; Bob adds a line
inside it; the merge deletes the function but keeps Bob's orphan line, dangling
in whatever context follows. No warning. This is standard OT behaviour and it
is why conflict markers exist. Same recommendation as R2.

**R4 — `Q = diff(B, C)` is a re-derived diff, not the real operations.**
Composition of ops and diff-of-endpoints differ: insert-then-delete vanishes,
moved blocks are re-attributed, and repeated lines can attach a retain to the
wrong occurrence under the deletion-on-tie rule. Git's 3-way merge has the same
blind spot ("only sees snapshots o, a, and b", no temporal ordering), so Snap
is not uniquely bad — but it means Snap's §6.3 is *rebase against a recomputed
diff*, not OT in the Jupiter/Wave sense, and the OT literature's guarantees do
not transfer. Say so in the spec; and make §11.3's repeated-line golden tests
carry through into §11.4's OT tests, not just the diff tests.

**R5 — The convergence argument rests entirely on the canonical total order.**
Snap does not satisfy TP2 (almost nothing does; Wave and Google Docs do not
either) and does not need to, because §3.4 + §6.1 give every replica the same
sequence — the decentralized analogue of Jupiter's server ordering. This is
sound, but it is *fragile against optimization*: any future caching that reuses
a transform result computed under a different integration order breaks the
argument silently, with no test failure until a specific three-way case
appears. Write the argument down in the spec next to §6.5 so it survives
contact with a performance-minded implementer, and make §11.4's "at least three
concurrent text patches" requirement test all permutations of import order, not
just one.

**R6 — Replay cost is O(entire history) on every merge, status, diff and log.**
Every command that needs the current tree replays everything, and §6.2 needs
each patch's base tree too. Prior art has the fix (eg-walker's critical
versions, Pijul's pristine cache); Snap's spec does not forbid caching, so this
is purely an implementation risk — but with three independent implementations
it is a risk that will show up as one of them being unusably slow on the
acceptance suite. Budget for it.

**R7 — No crash recovery, and the mutation order makes a bad interleaving
observable.** §10 already admits an interrupted merge can leave "a dirty,
partially updated working tree with the old `repository.json`". jj's operation
log is the standard cheap answer. Out of scope for v1; make sure the error
message tells the user what state they are in and that re-running `merge` after
cleaning the tree is safe (it is, by §1.6 idempotence — worth saying).

---

## 9. Suggested spec edits (small, concrete)

1. §1/§6.5: add one sentence stating that convergence follows from the
   canonical total order, not from OT transformation properties, and that Snap
   deliberately does not attempt TP2.
2. §6.3: rename or gloss the mechanism as "transform against a re-derived
   aggregate context diff" and note the consequence (R4).
3. §6.4: add a line-level warning reason (R2/R3) or explicitly document, in
   user-facing terms, that overlapping line edits silently coexist.
4. §3.1/§7.1: mint a per-clone contributor id suffix by default (R1).
5. §12: note that cherry-pick is *unrepresentable* under the current version
   type, not merely deferred.
6. §11: strengthen tests for (a) cross-repository dot collision with
   structurally different patches, (b) non-interleaving of concurrent
   multi-line inserts with repeated lines, (c) all import-order permutations
   for three or more concurrent text patches.

---

## 10. Sources

Patch theory: [Roundy, *Theory of Patches*](https://www.cs.tufts.edu/~nr/cs257/archive/david-roundy/Theory%20of%20patches.html) ·
[Understanding Darcs: patch theory and conflicts](https://en.wikibooks.org/wiki/Understanding_Darcs/Patch_theory_and_conflicts) ·
[darcs.net FAQ/Performance](https://darcs.net/FAQ/Performance) ·
[Pijul manual: Theory](https://pijul.org/manual/theory.html) ·
[Pijul manual: Why Pijul](https://pijul.org/manual/why_pijul.html) ·
Mimram & Di Giusto, *A Categorical Theory of Patches*, ENTCS 2013.

Snapshot VCS: [git-merge](https://git-scm.com/docs/git-merge) ·
[git merge-strategies](https://git-scm.com/docs/merge-strategies) ·
[CrissCrossMerge (revctrl)](https://tonyg.github.io/revctrl.org/CrissCrossMerge.html) ·
[Coglan, *Merging with diff3*](https://blog.jcoglan.com/2017/05/08/merging-with-diff3/) ·
[Fossil versus Git](https://fossil-scm.org/home/doc/tip/www/fossil-v-git.wiki) ·
[jj technical/conflicts.md](https://github.com/jj-vcs/jj/blob/main/docs/technical/conflicts.md) ·
[jj glossary](https://docs.jj-vcs.dev/latest/glossary/) ·
[Sapling: differences from Mercurial](https://sapling-scm.com/docs/introduction/differences-hg/) ·
[Branching in a Sapling Monorepo](https://engineering.fb.com/2025/10/16/developer-tools/branching-in-a-sapling-monorepo/).

CRDTs: [Eg-walker: Better, Faster, Smaller](https://arxiv.org/html/2409.14252v1) ·
[Loro: Event Graph Walker](https://loro.dev/docs/concepts/event_graph_walker) ·
[Kleppmann et al., *Interleaving anomalies*](https://martin.kleppmann.com/papers/interleaving-papoc19.pdf) ·
[*The Art of the Fugue*](https://arxiv.org/pdf/2305.00583) ·
[Roh et al., *Replicated abstract data types* (RGA)](http://csl.skku.edu/papers/jpdc11.pdf) ·
[LSEQ](https://www.researchgate.net/publication/262162421_LSEQ_an_Adaptive_Structure_for_Sequences_in_Distributed_Collaborative_Editing) ·
[Peritext](https://www.inkandswitch.com/peritext/) ·
[crdt-benchmarks](https://github.com/dmonad/crdt-benchmarks) ·
[Automerge documentation](https://automerge.org/docs/).

OT: [Nichols et al., *Jupiter* (UIST '95)](https://dl.acm.org/doi/10.1145/215585.215706) ·
[Wave OT whitepaper](https://svn.apache.org/repos/asf/incubator/wave/whitepapers/operational-transform/operational-transform.html) ·
[Wave OT and server acknowledgements](http://www.thinkbottomup.com.au/site/blog/Google_Wave_Operational_Transform_and_Server_Acknowledgments) ·
[Operational transformation (overview)](https://en.wikipedia.org/wiki/Operational_transformation) ·
[On Consistency of the OT Approach](https://arxiv.org/pdf/1302.3292) ·
[Etherpad Easysync](https://github.com/ether/etherpad-lite/tree/develop/doc/easysync).

Version vectors: [Dynamo (SOSP '07)](https://www.cs.cornell.edu/courses/cs5414/2017fa/papers/dynamo.pdf) ·
[Riak: Vector Clocks Revisited, part 2 (DVV)](https://riak.com/posts/technical/vector-clocks-revisited-part-2-dotted-version-vectors/index.html) ·
[Preguiça et al., *Dotted Version Vectors* (arXiv:1011.5808)](https://arxiv.org/pdf/1011.5808) ·
[Improving Logical Clocks in Riak with DVV](https://www.researchgate.net/publication/265148663_Improving_Logical_Clocks_in_Riak_with_Dotted_Version_Vectors_A_Case_Study) ·
Parker et al., *Detection of Mutual Inconsistency in Distributed Systems*, IEEE TSE 9(3), 1983 ·
[Reiher et al., *Resolving File Conflicts in the Ficus File System*](https://ant.isi.edu/~johnh/PAPERS/Reiher94a.pdf).

Claims marked "inference" above are my analysis of Snap's spec against these
sources, not statements from them. In particular: the R2 duplicate-line trace,
the critical-version applicability to §6.1, the O(n²) base-materialization
cost, and the per-clone-suffix mitigation are reasoning about `SPEC.md`, not
citations.
