//! Deterministic replay and conflict resolution (SPEC §6).
//!
//! Materializing a version means selecting every patch it covers, ordering
//! them canonically, and integrating them one at a time from the empty tree.
//! The same valid patch set and frontier must produce the same bytes and the
//! same warning set in every implementation (SPEC §6.5).

use crate::error::{self, Result};
use crate::model::{Change, ChangeKind, Content, Patch, Repository, Tree};
use crate::ot;
use crate::text;
use crate::version::Version;
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::rc::Rc;

/// Why a whole concurrent effect was discarded (SPEC §6.4).
///
/// The vocabulary is closed: SPEC §6.4 lists exactly these five reasons, and
/// line-level OT deliberately emits none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    DeleteWins,
    LaterCreateWins,
    LaterPutWins,
    NamespaceWins,
    PutWins,
}

impl Reason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::DeleteWins => "delete-wins",
            Reason::LaterCreateWins => "later-create-wins",
            Reason::LaterPutWins => "later-put-wins",
            Reason::NamespaceWins => "namespace-wins",
            Reason::PutWins => "put-wins",
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Unique `(path, reason)` pairs sorted by path, then reason (SPEC §6.4).
/// `BTreeSet` gives both properties for free, since `String` compares by bytes.
pub type Warnings = BTreeSet<(String, Reason)>;

/// The canonical integration order of every patch covered by `target`
/// (SPEC §6.1). `log` prints the reverse of this.
pub fn canonical_order<'a>(repo: &'a Repository, target: &Version) -> Result<Vec<&'a Patch>> {
    let materializer = Materializer::new(repo);
    let selected = materializer.select(target)?;
    Materializer::order(&selected)
}

/// Materialize `target` from `repo`, returning the tree and the warning set.
pub fn materialize(repo: &Repository, target: &Version) -> Result<(Tree, Warnings)> {
    Materializer::new(repo).run(target)
}

/// Materialize `target`, discarding warnings. Used for base trees, where only
/// the bytes matter.
pub fn materialize_tree(repo: &Repository, target: &Version) -> Result<Tree> {
    Ok(materialize(repo, target)?.0)
}

/// Naive SPEC §6 replay without memoization or prefix resume.
///
/// Intended for differential testing against the optimized [`materialize`].
/// Follows the spec literally: order, then integrate each patch against the
/// tree produced by replaying its own base from scratch.
pub fn naive_materialize(repo: &Repository, target: &Version) -> Result<(Tree, Warnings)> {
    let selected = Materializer::new(repo).select(target)?;
    let ordered = Materializer::order(&selected)?;
    let mut tree = Tree::new();
    let mut warnings = Warnings::new();
    for patch in ordered {
        let base_tree = naive_tree(repo, &patch.base)?;
        integrate(patch, &base_tree, &mut tree, &mut warnings)?;
    }
    Ok((tree, warnings))
}

/// Rebuild a base tree from scratch — no memoization, no shortcut.
fn naive_tree(repo: &Repository, target: &Version) -> Result<Tree> {
    let selected = Materializer::new(repo).select(target)?;
    let ordered = Materializer::order(&selected)?;
    let mut tree = Tree::new();
    let mut warnings = Warnings::new();
    for patch in ordered {
        let base_tree = naive_tree(repo, &patch.base)?;
        integrate(patch, &base_tree, &mut tree, &mut warnings)?;
    }
    Ok(tree)
}

struct Materializer<'a> {
    repo: &'a Repository,
    /// Memoized version -> tree, shared across the whole invocation so a base
    /// tree is built at most once however often it is referenced.
    memo: HashMap<Version, Rc<Tree>>,
    /// Every version referenced as some patch's base. Snapshots are taken only
    /// at these frontiers, so memory stays proportional to distinct bases
    /// rather than to patches x tree size.
    referenced: HashSet<Version>,
    depth: usize,
    /// High-water mark of `depth`, kept so tests can assert that memoization
    /// really does keep the fallback recursion out of the common paths.
    /// `depth` alone cannot show this: it is decremented on the way out and is
    /// therefore always zero once a replay returns.
    max_depth: usize,
}

/// Stack-overflow backstop for the `base_tree` fallback recursion.
///
/// This is *not* a tuning parameter, and it does not scale with history
/// length. Recursion happens only when a patch's base is not already memoized;
/// because the top-level replay memoizes each referenced frontier as it goes,
/// a linear history never recurses at all. Instrumenting the whole test suite
/// and every benchmark — including a 100,000-patch linear history, a 2x250
/// divergent merge, and overlapping concurrent text edits — showed a maximum
/// observed depth of **1**.
///
/// The guard therefore exists purely so that adversarial or corrupt input
/// cannot drive unbounded recursion into a stack overflow: failing cleanly
/// beats aborting the process. The limit is deliberately far above anything
/// a real history reaches.
const MAX_BASE_DEPTH: usize = 4096;

impl<'a> Materializer<'a> {
    fn new(repo: &'a Repository) -> Self {
        let referenced = repo.patches.iter().map(|p| p.base.clone()).collect();
        Self {
            repo,
            memo: HashMap::new(),
            referenced,
            depth: 0,
            max_depth: 0,
        }
    }

    /// SPEC §6.1: select every patch `(c, n)` with `n <= V[c]`, and require
    /// the selection to contain every selected patch's base.
    fn select(&self, target: &Version) -> Result<Vec<&'a Patch>> {
        let selected: Vec<&Patch> = self
            .repo
            .patches
            .iter()
            .filter(|p| p.revision <= target.get(&p.author))
            .collect();
        for patch in &selected {
            // A base is covered exactly when every one of its components is
            // matched by a selected patch, which the selection rule makes
            // equivalent to `base <= target` componentwise.
            for (id, revision) in patch.base.iter() {
                if revision > target.get(id) {
                    return Err(error::missing_base(&format!("{id}@{revision}")));
                }
            }
        }
        Ok(selected)
    }

    fn run(&mut self, target: &Version) -> Result<(Tree, Warnings)> {
        let selected = self.select(target)?;
        self.replay(&selected)
    }

    /// SPEC §6.1's canonical order, without integrating anything.
    ///
    /// SPEC §6.1 describes a greedy loop: repeatedly take the least *ready*
    /// patch by Snap order of result version, then author, then revision. That
    /// is equivalent to simply sorting every selected patch by the same key,
    /// and the equivalence is worth stating because it turns an O(n^2) scan
    /// into an O(n log n) sort.
    ///
    /// Proof. Snap order extends causal order (SPEC §3.4). Take the
    /// un-integrated patch `P` minimal under the key. Every patch `R` in `P`'s
    /// base satisfies `R.result <= P.base < P.result` causally, so `R` is
    /// strictly smaller under the key and has already been integrated. Hence
    /// the global minimum is always ready, and the greedy loop's pick is the
    /// global minimum at every step.
    ///
    /// The sort cannot detect a cycle or a missing dependency, so readiness is
    /// verified explicitly afterwards — which is also what SPEC §4.5's "no
    /// ready patch remains" failure becomes here.
    fn order(selected: &[&'a Patch]) -> Result<Vec<&'a Patch>> {
        // Decorate-sort-undecorate. `Patch::result` clones the base vector, so
        // calling it inside the comparator costs one allocation per comparison
        // — O(n log n) of them, which on a 100,000-patch history is millions.
        // Computing it once per patch makes it O(n), and the readiness check
        // below reuses the same values instead of recomputing them again.
        let mut decorated: Vec<(Version, &'a Patch)> = selected
            .iter()
            .map(|patch| (patch.result(), *patch))
            .collect();
        decorated.sort_by(|(left_result, left), (right_result, right)| {
            left_result
                .snap_cmp(right_result)
                .then_with(|| left.author.cmp(&right.author))
                .then_with(|| left.revision.cmp(&right.revision))
        });
        let mut joined = Version::empty();
        for (result, patch) in &decorated {
            if !patch.base.is_before_or_equal(&joined) {
                return Err(error::cyclic_history());
            }
            joined = joined.join(result);
        }
        Ok(decorated.into_iter().map(|(_, patch)| patch).collect())
    }

    /// SPEC §6.1's canonical order plus SPEC §6.2's integration.
    fn replay(&mut self, selected: &[&'a Patch]) -> Result<(Tree, Warnings)> {
        let ordered = Self::order(selected)?;
        let mut tree = Tree::new();
        let mut warnings = Warnings::new();
        let mut joined = Version::empty();

        self.memo.entry(Version::empty()).or_default();

        for patch in ordered {
            let base_tree = self.base_tree(&patch.base)?;
            integrate(patch, &base_tree, &mut tree, &mut warnings)?;
            joined = joined.join(&patch.result());
            if self.referenced.contains(&joined) {
                self.memo
                    .entry(joined.clone())
                    .or_insert_with(|| Rc::new(tree.clone()));
            }
        }
        Ok((tree, warnings))
    }

    /// The exact base tree of a patch (SPEC §6.2).
    ///
    /// Rather than replaying from the empty tree, this resumes from the
    /// longest already-memoized canonical prefix of the base's own order and
    /// integrates only what is missing.
    ///
    /// This is sound because `order` is a deterministic total sort, so the
    /// prefixes recorded below are literal prefixes of *this base's own*
    /// canonical order. Integrating the first `k` patches of that order always
    /// yields the same tree, whatever else the repository contains, so
    /// resuming from a memoized prefix and continuing is exactly what a fresh
    /// replay of the base would have done.
    ///
    /// Without it, a divergent history costs one full sub-replay per patch,
    /// because a branch's bases are not prefixes of the *interleaved* order
    /// that the top-level replay walks. That was the dominant cost in the
    /// `divergent` and `text-ot` workloads before this existed; see
    /// PERFORMANCE.md for current figures.
    fn base_tree(&mut self, base: &Version) -> Result<Rc<Tree>> {
        if let Some(tree) = self.memo.get(base) {
            return Ok(Rc::clone(tree));
        }
        if self.depth >= MAX_BASE_DEPTH {
            return Err(error::depth_limit_reached());
        }
        self.depth += 1;
        self.max_depth = self.max_depth.max(self.depth);
        let result = self.build_base_tree(base);
        self.depth -= 1;
        let tree = Rc::new(result?);
        self.memo.insert(base.clone(), Rc::clone(&tree));
        Ok(tree)
    }

    fn build_base_tree(&mut self, base: &Version) -> Result<Tree> {
        let selected = self.select(base)?;
        let ordered = Self::order(&selected)?;

        // The joined version after each step of this base's own order.
        let mut prefixes = Vec::with_capacity(ordered.len());
        let mut joined = Version::empty();
        for patch in &ordered {
            joined = joined.join(&patch.result());
            prefixes.push(joined.clone());
        }

        // Resume from the longest memoized prefix.
        let mut tree = Tree::new();
        let mut start = 0;
        for index in (0..ordered.len()).rev() {
            if let Some(memoized) = self.memo.get(&prefixes[index]) {
                tree = (**memoized).clone();
                start = index + 1;
                break;
            }
        }

        // Warnings are discarded: only the bytes of a base tree matter, and
        // SPEC §6.4's reported set is always computed by the top-level replay.
        let mut warnings = Warnings::new();
        for index in start..ordered.len() {
            let patch = ordered[index];
            let patch_base = self.base_tree(&patch.base)?;
            integrate(patch, &patch_base, &mut tree, &mut warnings)?;
            if self.referenced.contains(&prefixes[index]) {
                self.memo
                    .entry(prefixes[index].clone())
                    .or_insert_with(|| Rc::new(tree.clone()));
            }
        }
        Ok(tree)
    }
}

/// The authored result of one change applied to the patch's exact base tree.
fn authored(change: &Change, base: &Tree) -> Result<Option<Content>> {
    let existing = base.get(&change.path);
    match &change.kind {
        ChangeKind::Delete => {
            if existing.is_none() {
                return Err(error::delete_of_absent_path(&change.path));
            }
            Ok(None)
        }
        ChangeKind::Put(content) => {
            // SPEC §4.3: a change that alters neither existence nor bytes is
            // invalid.
            if existing.is_some_and(|old| old.as_ref() == content.as_ref()) {
                return Err(error::no_op_change(&change.path));
            }
            Ok(Some(content.clone()))
        }
        ChangeKind::Text(script) => {
            let old_bytes = existing.map(|c| c.as_ref().to_vec()).unwrap_or_default();
            if existing.is_some() && !text::is_text(&old_bytes) {
                return Err(error::no_op_change(&change.path));
            }
            let old_text =
                String::from_utf8(old_bytes).map_err(|_| error::no_op_change(&change.path))?;
            let old_tokens = text::tokenize(&old_text);
            match (script.consumed() as usize).cmp(&old_tokens.len()) {
                Ordering::Greater => return Err(error::edit_consumes_beyond()),
                Ordering::Less => return Err(error::edit_does_not_consume()),
                Ordering::Equal => {}
            }
            let new_tokens = text::apply(script, &old_tokens)?;
            let new_text = new_tokens.concat();
            // SPEC §4.3: "An edit, replacement, or delete requires it to be
            // present", and an empty edit may only create an empty file.
            if existing.is_none() && !script.is_empty() && new_text.is_empty() {
                return Err(error::no_op_change(&change.path));
            }
            if existing.is_some_and(|old| old.as_ref() == new_text.as_bytes()) {
                return Err(error::no_op_change(&change.path));
            }
            Ok(Some(new_text.into_bytes().into()))
        }
    }
}

/// Whether `candidate` is a strict path-segment ancestor or descendant of
/// `path` — the namespace collision of SPEC §6.2.
fn collides(path: &str, candidate: &str) -> bool {
    let (short, long) = if path.len() < candidate.len() {
        (path, candidate)
    } else {
        (candidate, path)
    };
    long.len() > short.len() && long.starts_with(short) && long.as_bytes()[short.len()] == b'/'
}

/// Integrate one patch into the canonical tree (SPEC §6.2).
fn integrate(
    patch: &Patch,
    base: &Tree,
    current: &mut Tree,
    warnings: &mut Warnings,
) -> Result<()> {
    // Authored results, computed against the patch's exact base.
    let mut targets: Vec<(&str, Option<Content>)> = Vec::with_capacity(patch.changes.len());
    for change in &patch.changes {
        targets.push((change.path.as_str(), authored(change, base)?));
    }

    // SPEC §2: the authored result must itself be prefix-free.
    {
        let mut authored_tree: Vec<&str> = base
            .keys()
            .map(String::as_str)
            .filter(|p| !targets.iter().any(|(t, _)| t == p))
            .collect();
        for (path, content) in &targets {
            if content.is_some() {
                authored_tree.push(path);
            }
        }
        crate::model::check_prefix_free(authored_tree.into_iter())?;
    }

    // -- Namespace resolution, for the patch as a whole, first -------------
    //
    // `C'` is the current tree minus the paths this patch deletes; a path the
    // patch itself removes cannot conflict with one it creates.
    let deleted: Vec<&str> = targets
        .iter()
        .filter(|(_, c)| c.is_none())
        .map(|(p, _)| *p)
        .collect();
    let present: Vec<(&str, &Content)> = targets
        .iter()
        .filter_map(|(p, c)| c.as_ref().map(|c| (*p, c)))
        .collect();

    let mut settled: Vec<(&str, Option<Content>)> = Vec::new();
    let mut removals: BTreeSet<String> = BTreeSet::new();
    for (path, content) in &present {
        let conflicting: Vec<&String> = current
            .keys()
            .filter(|existing| !deleted.contains(&existing.as_str()) && collides(path, existing))
            .collect();
        if conflicting.is_empty() {
            continue;
        }
        for existing in conflicting {
            removals.insert(existing.clone());
            warnings.insert((existing.clone(), Reason::NamespaceWins));
        }
        // The incoming path installs as its authored result, overriding the
        // per-path rules below.
        settled.push((path, Some((*content).clone())));
    }

    // -- Per-path rules for everything the namespace rule did not settle ---
    let mut resolved: Vec<(&str, Option<Content>)> = Vec::new();
    for (index, (path, target)) in targets.iter().enumerate() {
        if settled.iter().any(|(p, _)| p == path) {
            continue;
        }
        let change = &patch.changes[index];
        let in_base = base.get(*path);
        let in_current = current.get(*path);

        // Rule 1: identical in B and C, so apply the authored change directly.
        if same(in_base, in_current) {
            resolved.push((path, target.clone()));
            continue;
        }
        // Rule 2: identical in C and T. Collapses identical concurrent changes
        // *before* OT, which is what stops them duplicating (see `ot`).
        if same(in_current, target.as_ref()) {
            continue;
        }
        // Rule 3: all three sides text and the change is text, so transform.
        if let ChangeKind::Text(script) = &change.kind {
            if let (Some(base_bytes), Some(current_bytes), Some(target_bytes)) =
                (in_base, in_current, target.as_ref())
            {
                if text::is_text(base_bytes)
                    && text::is_text(current_bytes)
                    && text::is_text(target_bytes)
                {
                    let base_text = std::str::from_utf8(base_bytes).expect("checked");
                    let current_text = std::str::from_utf8(current_bytes).expect("checked");
                    let base_tokens = text::tokenize(base_text);
                    let current_tokens = text::tokenize(current_text);
                    let context = text::diff(&base_tokens, &current_tokens);
                    if let Some(transformed) = ot::transform(script, &context) {
                        let merged = text::apply(&transformed, &current_tokens)?;
                        resolved.push((path, Some(merged.concat().into_bytes().into())));
                        continue;
                    }
                }
            }
        }
        // Rule 4: fall back to the path-level winners of SPEC §6.4.
        match path_level(change, in_base, in_current, target.as_ref(), warnings) {
            Resolution::Keep => {}
            Resolution::Install(content) => resolved.push((path, Some(content))),
            Resolution::Remove => resolved.push((path, None)),
        }
    }

    // Apply removals and installations together, so one patch takes effect as
    // a single step (SPEC §6.2).
    for path in &removals {
        current.remove(path);
    }
    for (path, content) in settled.into_iter().chain(resolved) {
        match content {
            Some(bytes) => {
                current.insert(path.to_string(), bytes);
            }
            None => {
                current.remove(path);
            }
        }
    }
    Ok(())
}

fn same(a: Option<&Content>, b: Option<&Content>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => x.as_ref() == y.as_ref(),
        _ => false,
    }
}

/// What a path-level rule decided for one path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Resolution {
    /// The current canonical value stands; write nothing.
    Keep,
    /// Install these bytes.
    Install(Content),
    /// Remove the path.
    Remove,
}

/// SPEC §6.4's ordered path-level rules.
fn path_level(
    change: &Change,
    in_base: Option<&Content>,
    in_current: Option<&Content>,
    target: Option<&Content>,
    warnings: &mut Warnings,
) -> Resolution {
    let path = change.path.clone();
    let mut warn = |reason: Reason| {
        warnings.insert((path.clone(), reason));
    };

    // 1. Identical current and incoming: keep, no warning.
    if same(in_current, target) {
        return Resolution::Keep;
    }
    // 2. The incoming delete wins.
    let Some(target) = target else {
        warn(Reason::DeleteWins);
        return Resolution::Remove;
    };
    // 3. An earlier concurrent delete wins.
    if in_base.is_some() && in_current.is_none() {
        warn(Reason::DeleteWins);
        return Resolution::Keep;
    }
    // 4. Both created the path concurrently; the canonically later one wins.
    if in_base.is_none() && in_current.is_some() {
        warn(Reason::LaterCreateWins);
        return Resolution::Install(target.clone());
    }
    // 5. An atomic replacement wins.
    if matches!(change.kind, ChangeKind::Put(_)) {
        warn(Reason::LaterPutWins);
        return Resolution::Install(target.clone());
    }
    // 6. The change is text but the current content is not; current wins.
    warn(Reason::PutWins);
    Resolution::Keep
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Change, ChangeKind, EditOp, EditScript};
    use crate::text;

    fn content(bytes: &str) -> Content {
        Content::from(bytes.as_bytes().to_vec())
    }

    fn tree(entries: &[(&str, &str)]) -> Tree {
        entries
            .iter()
            .map(|(p, c)| ((*p).to_string(), content(c)))
            .collect()
    }

    fn text_change(path: &str, from: &str, to: &str) -> Change {
        let (a, b) = (text::tokenize(from), text::tokenize(to));
        Change {
            path: path.to_string(),
            kind: ChangeKind::Text(text::diff(&a, &b)),
        }
    }

    fn put(path: &str, bytes: &str) -> Change {
        Change {
            path: path.to_string(),
            kind: ChangeKind::Put(content(bytes)),
        }
    }

    fn delete(path: &str) -> Change {
        Change {
            path: path.to_string(),
            kind: ChangeKind::Delete,
        }
    }

    fn patch(author: &str, revision: u64, base: &str, changes: Vec<Change>) -> Patch {
        Patch {
            author: author.to_string(),
            revision,
            base: Version::parse(base).unwrap(),
            message: "m".to_string(),
            changes,
        }
    }

    /// Integrate one patch against a prepared base and current tree.
    fn run(patch: &Patch, base: &Tree, current: &Tree) -> (Tree, Warnings) {
        let mut out = current.clone();
        let mut warnings = Warnings::new();
        integrate(patch, base, &mut out, &mut warnings).expect("integrates");
        (out, warnings)
    }

    // -- SPEC §6.2 per-path rules ------------------------------------------

    #[test]
    fn rule_1_applies_the_authored_change_when_base_and_current_agree() {
        let base = tree(&[("f", "a\n")]);
        let patch = patch("a@x", 1, "()", vec![text_change("f", "a\n", "b\n")]);
        let (out, warnings) = run(&patch, &base, &base);
        assert_eq!(out["f"].as_ref(), b"b\n");
        assert!(warnings.is_empty());
    }

    #[test]
    fn rule_2_collapses_identical_concurrent_changes_before_ot() {
        // The regression promised in `ot`: if rule 2 did not run before rule 3,
        // an identical concurrent edit would be transformed through itself and
        // the inserted line would appear twice.
        let base = tree(&[("f", "a\nb\n")]);
        let already = tree(&[("f", "a\nX\nb\n")]);
        let patch = patch(
            "a@x",
            1,
            "()",
            vec![text_change("f", "a\nb\n", "a\nX\nb\n")],
        );
        let (out, warnings) = run(&patch, &base, &already);
        assert_eq!(
            std::str::from_utf8(&out["f"]).unwrap(),
            "a\nX\nb\n",
            "identical concurrent edits must not duplicate"
        );
        assert!(
            warnings.is_empty(),
            "SPEC §6.4: this collapse emits no warning"
        );
    }

    #[test]
    fn rule_3_transforms_concurrent_text_edits() {
        let base = tree(&[("f", "a\nb\nc\n")]);
        let current = tree(&[("f", "A\nb\nc\n")]);
        let patch = patch(
            "a@x",
            1,
            "()",
            vec![text_change("f", "a\nb\nc\n", "a\nb\nC\n")],
        );
        let (out, warnings) = run(&patch, &base, &current);
        assert_eq!(
            std::str::from_utf8(&out["f"]).unwrap(),
            "A\nb\nC\n",
            "both edits survive"
        );
        assert!(warnings.is_empty(), "SPEC §6.4: line OT emits no warning");
    }

    // -- SPEC §6.4 path-level winners, one test per rule -------------------

    #[test]
    fn incoming_delete_wins() {
        let base = tree(&[("f", "a\n")]);
        let current = tree(&[("f", "changed\n")]);
        let patch = patch("a@x", 1, "()", vec![delete("f")]);
        let (out, warnings) = run(&patch, &base, &current);
        assert!(!out.contains_key("f"));
        assert!(warnings.contains(&("f".to_string(), Reason::DeleteWins)));
    }

    #[test]
    fn earlier_concurrent_delete_wins() {
        let base = tree(&[("f", "a\n")]);
        let current = Tree::new();
        let patch = patch("a@x", 1, "()", vec![put("f", "b\n")]);
        let (out, warnings) = run(&patch, &base, &current);
        assert!(!out.contains_key("f"), "the earlier delete stands");
        assert!(warnings.contains(&("f".to_string(), Reason::DeleteWins)));
    }

    #[test]
    fn later_create_wins() {
        let base = Tree::new();
        let current = tree(&[("f", "theirs\n")]);
        let patch = patch("a@x", 1, "()", vec![text_change("f", "", "mine\n")]);
        let (out, warnings) = run(&patch, &base, &current);
        assert_eq!(out["f"].as_ref(), b"mine\n");
        assert!(warnings.contains(&("f".to_string(), Reason::LaterCreateWins)));
    }

    #[test]
    fn later_put_wins() {
        let base = tree(&[("f", "a\n")]);
        let current = tree(&[("f", "theirs\n")]);
        let patch = patch("a@x", 1, "()", vec![put("f", "mine\n")]);
        let (out, warnings) = run(&patch, &base, &current);
        assert_eq!(out["f"].as_ref(), b"mine\n");
        assert!(warnings.contains(&("f".to_string(), Reason::LaterPutWins)));
    }

    #[test]
    fn incompatible_current_content_wins_over_a_text_change() {
        let base = tree(&[("f", "a\n")]);
        let current: Tree = [("f".to_string(), Content::from(vec![0u8, 1, 2]))]
            .into_iter()
            .collect();
        let patch = patch("a@x", 1, "()", vec![text_change("f", "a\n", "b\n")]);
        let (out, warnings) = run(&patch, &base, &current);
        assert_eq!(
            out["f"].as_ref(),
            &[0u8, 1, 2],
            "binary current content stands"
        );
        assert!(warnings.contains(&("f".to_string(), Reason::PutWins)));
    }

    #[test]
    fn identical_outcomes_emit_no_warning() {
        // SPEC §6.4 rule 1: C and T identical, keep C, no warning.
        let base = tree(&[("f", "a\n")]);
        let current = tree(&[("f", "same\n")]);
        let patch = patch("a@x", 1, "()", vec![put("f", "same\n")]);
        let (_, warnings) = run(&patch, &base, &current);
        assert!(warnings.is_empty());
    }

    // -- SPEC §6.2 namespace resolution ------------------------------------

    #[test]
    fn a_created_file_evicts_a_conflicting_directory() {
        let base = Tree::new();
        let current = tree(&[("a/b", "nested\n")]);
        let patch = patch("a@x", 1, "()", vec![text_change("a", "", "file\n")]);
        let (out, warnings) = run(&patch, &base, &current);
        assert_eq!(out["a"].as_ref(), b"file\n");
        assert!(
            !out.contains_key("a/b"),
            "the conflicting descendant is removed"
        );
        assert!(warnings.contains(&("a/b".to_string(), Reason::NamespaceWins)));
    }

    #[test]
    fn a_created_directory_evicts_a_conflicting_file() {
        let base = Tree::new();
        let current = tree(&[("a", "file\n")]);
        let patch = patch("a@x", 1, "()", vec![text_change("a/b", "", "nested\n")]);
        let (out, warnings) = run(&patch, &base, &current);
        assert_eq!(out["a/b"].as_ref(), b"nested\n");
        assert!(!out.contains_key("a"));
        assert!(warnings.contains(&("a".to_string(), Reason::NamespaceWins)));
    }

    #[test]
    fn a_path_the_patch_itself_deletes_does_not_collide() {
        // SPEC §6.2 evaluates the namespace rule against `C'` = C minus the
        // paths this patch removes, so replacing a file with a directory in
        // one patch is not a conflict with itself.
        let base = tree(&[("a", "file\n")]);
        let current = tree(&[("a", "file\n")]);
        let patch = patch(
            "a@x",
            1,
            "()",
            vec![delete("a"), text_change("a/b", "", "nested\n")],
        );
        let (out, warnings) = run(&patch, &base, &current);
        assert_eq!(out["a/b"].as_ref(), b"nested\n");
        assert!(!out.contains_key("a"));
        assert!(
            warnings.is_empty(),
            "no namespace warning against the patch's own delete"
        );
    }

    #[test]
    fn collision_detection_is_by_segment_not_by_prefix() {
        assert!(collides("a", "a/b"));
        assert!(collides("a/b", "a"));
        assert!(collides("a/b", "a/b/c"));
        assert!(!collides("a", "ab"), "`ab` is not inside `a`");
        assert!(!collides("a", "a"), "a path does not collide with itself");
        assert!(!collides("a/b", "a/c"));
    }

    // -- SPEC §6.4 warning ordering ----------------------------------------

    #[test]
    fn warnings_are_unique_and_sorted_by_path_then_reason() {
        let mut warnings = Warnings::new();
        warnings.insert(("z".into(), Reason::PutWins));
        warnings.insert(("a".into(), Reason::NamespaceWins));
        warnings.insert(("a".into(), Reason::DeleteWins));
        warnings.insert(("a".into(), Reason::DeleteWins));
        let listed: Vec<_> = warnings
            .iter()
            .map(|(p, r)| (p.as_str(), r.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                ("a", "delete-wins"),
                ("a", "namespace-wins"),
                ("z", "put-wins")
            ],
            "sorted by path, then reason, with duplicates collapsed"
        );
    }

    #[test]
    fn reason_names_match_the_spec_vocabulary() {
        assert_eq!(Reason::DeleteWins.as_str(), "delete-wins");
        assert_eq!(Reason::LaterCreateWins.as_str(), "later-create-wins");
        assert_eq!(Reason::LaterPutWins.as_str(), "later-put-wins");
        assert_eq!(Reason::NamespaceWins.as_str(), "namespace-wins");
        assert_eq!(Reason::PutWins.as_str(), "put-wins");
    }

    // -- SPEC §4.3 authored results ----------------------------------------

    #[test]
    fn authored_rejects_changes_that_alter_nothing() {
        let base = tree(&[("f", "a\n")]);
        assert!(
            authored(&put("f", "a\n"), &base).is_err(),
            "put with identical bytes"
        );
        assert!(
            authored(&delete("g"), &base).is_err(),
            "delete of an absent path"
        );
        let empty_edit = Change {
            path: "f".into(),
            kind: ChangeKind::Text(EditScript::default()),
        };
        assert!(
            authored(&empty_edit, &base).is_err(),
            "empty script over existing content"
        );
    }

    #[test]
    fn authored_rejects_scripts_that_mismatch_the_base() {
        let base = tree(&[("f", "a\nb\n")]);
        let short = Change {
            path: "f".into(),
            kind: ChangeKind::Text(EditScript::new(vec![EditOp::Retain(1)]).unwrap()),
        };
        assert!(
            authored(&short, &base).is_err(),
            "leaves a base token unconsumed"
        );
        let long = Change {
            path: "f".into(),
            kind: ChangeKind::Text(EditScript::new(vec![EditOp::Retain(5)]).unwrap()),
        };
        assert!(authored(&long, &base).is_err(), "consumes past the base");
    }

    #[test]
    fn a_long_linear_history_replays_without_recursing() {
        // Regression for the memoization path, not for MAX_BASE_DEPTH.
        //
        // An earlier version of this test claimed the recursion depth equalled
        // the chain length, and that a raised depth limit was what made 300
        // patches work. That is false: the top-level replay memoizes each
        // referenced frontier as it integrates, so patch n+1's base is always
        // a hit and `base_tree` never recurses on a linear history. The test
        // passes identically at MAX_BASE_DEPTH = 256, and the 100,000-patch
        // benchmark would be impossible if depth really tracked length.
        //
        // What this does guard is the thing worth guarding: a long chain of
        // sequential patches replays to the right bytes.
        let count = 300u64;
        let mut repo = Repository::default();
        let mut frontier = Version::empty();
        for rev in 1..=count {
            let from = if rev == 1 {
                String::new()
            } else {
                format!("line {}\n", rev - 1)
            };
            let to = format!("line {rev}\n");
            let patch = Patch {
                author: "a@x".to_string(),
                revision: rev,
                base: frontier.clone(),
                message: format!("r{rev}"),
                changes: vec![text_change("f.txt", &from, &to)],
            };
            frontier = patch.result();
            repo.patches.push(patch);
        }
        repo.sort_patches();
        repo.frontier = frontier;

        let (tree, warnings) = materialize(&repo, &repo.frontier).expect("replays");
        assert_eq!(tree.len(), 1, "one file");
        assert!(warnings.is_empty(), "no concurrency, so no warnings");
        assert_eq!(
            std::str::from_utf8(tree.get("f.txt").expect("f.txt exists")).unwrap(),
            format!("line {count}\n")
        );
    }

    #[test]
    fn base_tree_recursion_stays_shallow_on_a_linear_history() {
        // The claim MAX_BASE_DEPTH's documentation rests on: memoization keeps
        // the fallback recursion out of the linear path entirely. Verified here
        // by construction — every patch's base is the immediately preceding
        // frontier, which the replay has just memoized.
        let mut repo = Repository::default();
        let mut frontier = Version::empty();
        for rev in 1..=64u64 {
            let patch = Patch {
                author: "a@x".to_string(),
                revision: rev,
                base: frontier.clone(),
                message: format!("r{rev}"),
                changes: vec![put(&format!("f{rev}.txt"), "x\n")],
            };
            frontier = patch.result();
            repo.patches.push(patch);
        }
        repo.sort_patches();
        repo.frontier = frontier.clone();

        let mut materializer = Materializer::new(&repo);
        let selected = materializer.select(&frontier).expect("selects");
        materializer.replay(&selected).expect("replays");
        assert_eq!(
            materializer.max_depth, 0,
            "a linear history must never enter the base_tree fallback"
        );
    }
}
