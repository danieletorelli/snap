//! Repository, patch and change types, plus their strict JSON mapping
//! (SPEC §4).
//!
//! Every type here is canonical by construction: `Repository::from_json`
//! rejects unsorted patches or changes rather than sorting them, and
//! `to_json` emits a fixed key order. That combination is what makes two
//! repositories which converged by different merge routes serialize to
//! identical bytes, which the acceptance suite checks with `trees_equal`.

use crate::error::{self, Result};
use crate::json::Json;
use crate::version::{validate_contributor_id, Version, MAX_REVISION};
use crate::{b64, json};
use std::collections::BTreeMap;
use std::rc::Rc;

/// File bytes, shared cheaply between the many trees a replay builds.
pub type Content = Rc<[u8]>;

/// A path/byte map. `BTreeMap<String, _>` gives SPEC §2's unsigned-byte path
/// ordering for free, because Rust compares `str` byte-wise.
pub type Tree = BTreeMap<String, Content>;

/// SPEC §4.2: `commit` limits user-supplied messages to 4096 bytes.
pub const MAX_COMMIT_MESSAGE_BYTES: usize = 4096;

/// Validate a tracked path per SPEC §2.
pub fn validate_path(path: &str) -> Result<()> {
    let bad = || error::invalid_path(path);
    if path.is_empty() {
        return Err(bad());
    }
    if path.bytes().any(|b| b.is_ascii_control() || b == b'\\') {
        return Err(bad());
    }
    let mut segments = path.split('/');
    let first = segments.next().ok_or_else(bad)?;
    if first == ".snap" {
        return Err(bad());
    }
    for segment in std::iter::once(first).chain(segments) {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(bad());
        }
    }
    Ok(())
}

/// Validate a patch message per SPEC §4.2: nonempty UTF-8, tab and LF allowed,
/// no other ASCII control character.
pub fn validate_message(message: &str) -> Result<()> {
    if message.is_empty() {
        return Err(error::message_is_empty());
    }
    if message
        .bytes()
        .any(|b| b.is_ascii_control() && b != b'\t' && b != b'\n')
    {
        return Err(error::invalid_commit_message());
    }
    Ok(())
}

/// SPEC §2: a tracked tree is prefix-free by path segment — if `a` is a file,
/// no `a/...` may be present.
pub fn check_prefix_free<'a>(paths: impl Iterator<Item = &'a str>) -> Result<()> {
    let mut sorted: Vec<&str> = paths.collect();
    sorted.sort_unstable();
    // For each path, look for an ancestor of it in the set.
    //
    // Checking only *adjacent* sorted pairs is not enough, and the bug it hides
    // is easy to miss: every byte below `/` (0x2F) sorts between a path and its
    // own descendants, so an unrelated sibling can separate them. For
    // ["x", "x-y", "x/z"] the sorted order is exactly that, and neither
    // adjacent pair is a parent/child — yet `x` and `x/z` do conflict.
    //
    // Testing each path against its ancestor prefixes instead is complete
    // regardless of what sorts in between. Only paths containing `/` can have
    // an ancestor, and the separator count is the path depth, so this stays
    // O(n log n) for the shallow trees that dominate in practice.
    // Single pass with a stack of still-open ancestors. In sorted order every
    // possible ancestor of a path precedes it, so the stack holds exactly the
    // chain of string-prefixes still in scope. Popping what is no longer a
    // prefix leaves the nearest candidate on top; if that candidate is an
    // ancestor *by segment*, the tree is not prefix-free.
    let mut open: Vec<&str> = Vec::new();
    for path in &sorted {
        while let Some(top) = open.last() {
            if path.starts_with(*top) {
                break;
            }
            open.pop();
        }
        if let Some(top) = open.last() {
            // `top` is a string prefix; it is an ancestor only at a separator.
            // The length guard also covers `path == top`: callers pass unique
            // paths today, but indexing at `top.len()` would panic on a
            // duplicate and this is a public function.
            if path.len() > top.len() && path.as_bytes()[top.len()] == b'/' {
                return Err(error::tree_paths_conflict(path));
            }
        }
        open.push(path);
    }
    Ok(())
}

/// Narrow a parsed JSON integer to a count. Callers guard positivity first;
/// this keeps the conversion explicit rather than an unchecked cast.
fn to_u64(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| error::invalid_json("value out of range"))
}

/// Widen a count for serialization. Revisions and edit counts are bounded by
/// `MAX_REVISION` (2^53 - 1), well within `i64::MAX` (2^63 - 1), so this
/// conversion is infallible for any data that passed `EditScript` validation.
/// A failure here is caught by `catch_unwind` in `main.rs` (exit code 2).
fn to_i64(value: u64) -> i64 {
    i64::try_from(value).expect("counts are bounded by MAX_REVISION")
}

// -- Edit scripts (SPEC §4.4) ---------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOp {
    Retain(u64),
    Delete(u64),
    Insert(Vec<String>),
}

impl EditOp {
    fn kind(&self) -> u8 {
        match self {
            EditOp::Retain(_) => 0,
            EditOp::Delete(_) => 1,
            EditOp::Insert(_) => 2,
        }
    }

    fn kind_name(&self) -> &'static str {
        match self {
            EditOp::Retain(_) => "retain",
            EditOp::Delete(_) => "delete",
            EditOp::Insert(_) => "insert",
        }
    }
}

/// A validated edit script. The invariant — positive counts, no adjacent
/// operations of the same kind, nonempty inserted tokens — is checked once at
/// construction so every consumer can rely on it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditScript {
    ops: Vec<EditOp>,
}

impl EditScript {
    pub fn new(ops: Vec<EditOp>) -> Result<Self> {
        for op in &ops {
            match op {
                EditOp::Retain(0) | EditOp::Delete(0) => {
                    return Err(error::not_positive_safe_integer("edit count"))
                }
                EditOp::Retain(n) | EditOp::Delete(n) if *n > MAX_REVISION => {
                    return Err(error::not_positive_safe_integer("edit count"))
                }
                EditOp::Insert(tokens) if tokens.is_empty() => return Err(error::insert_is_empty()),
                EditOp::Insert(tokens) if tokens.iter().any(String::is_empty) => {
                    return Err(error::insert_is_empty())
                }
                _ => {}
            }
        }
        for pair in ops.windows(2) {
            if pair[0].kind() == pair[1].kind() {
                return Err(if pair[0].kind() == 2 {
                    error::adjacent_insert()
                } else {
                    error::adjacent_same_kind(pair[0].kind_name())
                });
            }
        }
        Ok(Self { ops })
    }

    #[must_use]
    pub fn ops(&self) -> &[EditOp] {
        &self.ops
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Total old tokens the script consumes (retains plus deletes).
    #[must_use]
    pub fn consumed(&self) -> u64 {
        self.ops
            .iter()
            .map(|op| match op {
                EditOp::Retain(n) | EditOp::Delete(n) => *n,
                EditOp::Insert(_) => 0,
            })
            .sum()
    }

    fn from_json(value: &Json) -> Result<Self> {
        let Json::Arr(items) = value else {
            return Err(error::invalid_json("edit must be an array"));
        };
        let mut ops = Vec::with_capacity(items.len());
        for item in items {
            let Json::Obj(fields) = item else {
                return Err(error::invalid_json("edit operation must be an object"));
            };
            // SPEC §4.4: operations are single-key objects.
            let [(key, value)] = &fields[..] else {
                return Err(error::edit_must_have_one_operation());
            };
            // The key decides which operation this is; a recognised key with a
            // bad value is a count error, not a shape error. `23-strict-
            // validation-matrix` distinguishes the two.
            ops.push(match key.as_str() {
                "retain" | "delete" => {
                    let count = match value {
                        Json::Int(n) if *n > 0 => to_u64(*n)?,
                        _ => return Err(error::not_positive_safe_integer("edit count")),
                    };
                    if key == "retain" {
                        EditOp::Retain(count)
                    } else {
                        EditOp::Delete(count)
                    }
                }
                "insert" => {
                    let Json::Arr(tokens) = value else {
                        return Err(error::insert_is_empty());
                    };
                    EditOp::Insert(
                        tokens
                            .iter()
                            .map(|t| match t {
                                Json::Str(text) => Ok(text.clone()),
                                _ => Err(error::insert_is_empty()),
                            })
                            .collect::<Result<Vec<_>>>()?,
                    )
                }
                _ => return Err(error::edit_must_have_one_operation()),
            });
        }
        Self::new(ops)
    }

    fn to_json(&self) -> Json {
        Json::Arr(
            self.ops
                .iter()
                .map(|op| match op {
                    EditOp::Retain(n) => Json::Obj(vec![("retain".into(), Json::Int(to_i64(*n)))]),
                    EditOp::Delete(n) => Json::Obj(vec![("delete".into(), Json::Int(to_i64(*n)))]),
                    EditOp::Insert(tokens) => Json::Obj(vec![(
                        "insert".into(),
                        Json::Arr(tokens.iter().map(|t| Json::Str(t.clone())).collect()),
                    )]),
                })
                .collect(),
        )
    }
}

// -- Changes (SPEC §4.3) ---------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Text(EditScript),
    Put(Content),
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
}

impl Change {
    fn from_json(value: &Json) -> Result<Self> {
        let Some(Json::Str(kind)) = value.get("type") else {
            return Err(error::invalid_json("change requires a string type"));
        };
        let allowed: &[&str] = match kind.as_str() {
            "text" => &["type", "path", "edit"],
            "put" => &["type", "path", "content"],
            "delete" => &["type", "path"],
            _ => return Err(error::invalid_json("unknown change type")),
        };
        value.exact_fields_with(allowed, &|k| error::change_unknown_field(k))?;
        let Some(Json::Str(path)) = value.get("path") else {
            return Err(error::invalid_json("change requires a string path"));
        };
        validate_path(path)?;
        let kind = match kind.as_str() {
            "text" => ChangeKind::Text(EditScript::from_json(
                value.get("edit").expect("checked by exact_fields"),
            )?),
            "put" => {
                let Some(Json::Str(content)) = value.get("content") else {
                    return Err(error::invalid_json("put requires string content"));
                };
                ChangeKind::Put(b64::decode(content)?.into())
            }
            _ => ChangeKind::Delete,
        };
        Ok(Self {
            path: path.clone(),
            kind,
        })
    }

    fn to_json(&self) -> Json {
        let mut fields = vec![
            (
                "type".to_string(),
                Json::Str(
                    match self.kind {
                        ChangeKind::Text(_) => "text",
                        ChangeKind::Put(_) => "put",
                        ChangeKind::Delete => "delete",
                    }
                    .to_string(),
                ),
            ),
            ("path".to_string(), Json::Str(self.path.clone())),
        ];
        match &self.kind {
            ChangeKind::Text(edit) => fields.push(("edit".into(), edit.to_json())),
            ChangeKind::Put(content) => {
                fields.push(("content".into(), Json::Str(b64::encode(content))));
            }
            ChangeKind::Delete => {}
        }
        Json::Obj(fields)
    }
}

// -- Patches (SPEC §4.2) ---------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub author: String,
    pub revision: u64,
    pub base: Version,
    pub message: String,
    pub changes: Vec<Change>,
}

impl Patch {
    /// The `(author, revision)` dot that this patch uniquely owns.
    #[must_use]
    pub fn dot(&self) -> (&str, u64) {
        (&self.author, self.revision)
    }

    /// SPEC §4.2: `result = base with result[author] = revision`.
    ///
    /// Infallible. `Patch` has public fields, so a caller can hand-build one
    /// with a malformed author; that yields a version carrying the malformed
    /// author rather than a panic, and such a patch cannot reach disk because
    /// `Patch::from_json` and `cli::validate` both reject it.
    #[must_use]
    pub fn result(&self) -> Version {
        let mut result = self.base.clone();
        result.set_unchecked(&self.author, self.revision);
        result
    }

    fn from_json(value: &Json) -> Result<Self> {
        value.exact_fields_with(
            &["author", "revision", "base", "message", "changes"],
            &|k| error::patch_unknown_field(k),
        )?;
        let Some(Json::Str(author)) = value.get("author") else {
            return Err(error::invalid_json("patch requires a string author"));
        };
        validate_contributor_id(author)?;
        let Some(Json::Int(revision)) = value.get("revision") else {
            return Err(error::invalid_json("patch requires an integer revision"));
        };
        let revision = to_u64(*revision)?;
        if revision > MAX_REVISION {
            return Err(error::not_positive_safe_integer("revision"));
        }
        let base = version_from_json(value.get("base").expect("checked"))?;
        let Some(Json::Str(message)) = value.get("message") else {
            return Err(error::invalid_json("patch requires a string message"));
        };
        validate_message(message)?;
        let Some(Json::Arr(items)) = value.get("changes") else {
            return Err(error::invalid_json("patch requires a changes array"));
        };
        if items.is_empty() {
            return Err(error::changes_is_empty());
        }
        let changes = items
            .iter()
            .map(Change::from_json)
            .collect::<Result<Vec<_>>>()?;
        // SPEC §4.2: sorted by path, at most one change per path. Rejected
        // rather than normalized, so the on-disk form is already canonical.
        if changes.windows(2).any(|w| w[0].path >= w[1].path) {
            return Err(error::not_canonical("change order"));
        }
        Ok(Self {
            author: author.clone(),
            revision,
            base,
            message: message.clone(),
            changes,
        })
    }

    fn to_json(&self) -> Json {
        Json::Obj(vec![
            ("author".into(), Json::Str(self.author.clone())),
            ("revision".into(), Json::Int(to_i64(self.revision))),
            ("base".into(), version_to_json(&self.base)),
            ("message".into(), Json::Str(self.message.clone())),
            (
                "changes".into(),
                Json::Arr(self.changes.iter().map(Change::to_json).collect()),
            ),
        ])
    }
}

// -- Versions in repository JSON (SPEC §3.2) ------------------------------

fn version_from_json(value: &Json) -> Result<Version> {
    let Json::Arr(items) = value else {
        return Err(error::invalid_json("version must be an array"));
    };
    let mut pairs = Vec::with_capacity(items.len());
    let mut previous: Option<&str> = None;
    for item in items {
        let Json::Arr(pair) = item else {
            return Err(error::invalid_json("version entry must be an array"));
        };
        let [Json::Str(id), Json::Int(revision)] = &pair[..] else {
            return Err(error::invalid_json("version entry must be [id, revision]"));
        };
        // The JSON form is an *ordered* array, so ordering is validated here
        // rather than repaired by the sort inside `Version::from_pairs`.
        if previous.is_some_and(|p| p >= id.as_str()) {
            return Err(error::not_canonical("version entry order"));
        }
        previous = Some(id);
        if *revision <= 0 {
            return Err(error::not_positive_safe_integer("revision"));
        }
        pairs.push((id.clone(), to_u64(*revision)?));
    }
    Version::from_pairs(pairs)
}

fn version_to_json(version: &Version) -> Json {
    Json::Arr(
        version
            .iter()
            .map(|(id, revision)| {
                Json::Arr(vec![Json::Str(id.to_string()), Json::Int(to_i64(revision))])
            })
            .collect(),
    )
}

// -- Repository (SPEC §4.1) ------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Repository {
    pub frontier: Version,
    /// Sorted by author bytes, then numeric revision (SPEC §4.1).
    pub patches: Vec<Patch>,
}

impl Repository {
    pub fn from_json_str(text: &str) -> Result<Self> {
        Self::from_json(&json::parse(text)?)
    }

    pub fn from_json(value: &Json) -> Result<Self> {
        value.exact_fields_with(&["format", "frontier", "patches"], &|k| {
            error::repository_unknown_field(k)
        })?;
        match value.get("format") {
            Some(Json::Int(1)) => {}
            _ => return Err(error::invalid_json("unsupported repository format")),
        }
        let frontier = version_from_json(value.get("frontier").expect("checked"))?;
        let Some(Json::Arr(items)) = value.get("patches") else {
            return Err(error::invalid_json("patches must be an array"));
        };
        let patches = items
            .iter()
            .map(Patch::from_json)
            .collect::<Result<Vec<_>>>()?;
        if patches
            .windows(2)
            .any(|w| (w[0].author.as_str(), w[0].revision) >= (w[1].author.as_str(), w[1].revision))
        {
            return Err(error::not_canonical("patch order"));
        }
        Ok(Self { frontier, patches })
    }

    #[must_use]
    pub fn to_json(&self) -> Json {
        Json::Obj(vec![
            ("format".into(), Json::Int(1)),
            ("frontier".into(), version_to_json(&self.frontier)),
            (
                "patches".into(),
                Json::Arr(self.patches.iter().map(Patch::to_json).collect()),
            ),
        ])
    }

    /// Canonical serialization: two-space indent, trailing LF, fixed key order.
    #[must_use]
    pub fn to_canonical_string(&self) -> String {
        json::to_canonical_string(&self.to_json())
    }

    /// Every `(author, revision)` dot currently present.
    #[must_use]
    pub fn dots(&self) -> std::collections::HashSet<(&str, u64)> {
        self.patches.iter().map(Patch::dot).collect()
    }

    /// Restore the sort order `patches` must satisfy. Used when building a
    /// repository in memory; the reader rejects unsorted input instead.
    ///
    /// Sort patches into canonical order by (author, revision).
    ///
    /// Callers must ensure no duplicate dots exist before calling — SPEC §4.2
    /// gives each dot exactly one patch, and a duplicate means the caller built
    /// the union wrongly.
    pub fn sort_patches(&mut self) {
        self.patches.sort_by(|a, b| {
            a.author
                .cmp(&b.author)
                .then_with(|| a.revision.cmp(&b.revision))
        });
    }

    #[must_use]
    pub fn find(&self, author: &str, revision: u64) -> Option<&Patch> {
        self.patches
            .binary_search_by(|p| (p.author.as_str(), p.revision).cmp(&(author, revision)))
            .ok()
            .map(|i| &self.patches[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_EXAMPLE: &str = r#"{
      "format": 1,
      "frontier": [["alice@example.com",1]],
      "patches": [
        {
          "author": "alice@example.com",
          "revision": 1,
          "base": [],
          "message": "add greeting",
          "changes": [
            {"type":"text","path":"hello.txt","edit":[{"insert":["hello\n"]}]}
          ]
        }
      ]
    }"#;

    // -- SPEC §2 paths -----------------------------------------------------

    #[test]
    fn accepts_valid_tracked_paths() {
        for path in [
            "a",
            "a/b",
            "src/main.rs",
            "a.snap",
            ".snapshot",
            "dir/.snap",
        ] {
            assert!(validate_path(path).is_ok(), "{path} should be valid");
        }
    }

    #[test]
    fn rejects_paths_violating_spec_2() {
        for path in [
            "",        // empty
            "/a",      // empty first segment
            "a/",      // empty last segment
            "a//b",    // empty middle segment
            ".",       // dot segment
            "..",      // dotdot segment
            "a/../b",  // dotdot segment
            "a/./b",   // dot segment
            ".snap",   // reserved first segment
            ".snap/x", // reserved first segment
            "a\\b",    // backslash
            "a\tb",    // control character
            "a\u{0}b", // control character
        ] {
            assert!(validate_path(path).is_err(), "{path:?} should be rejected");
        }
    }

    #[test]
    fn prefix_free_check_catches_file_directory_collisions() {
        assert!(check_prefix_free(["a", "b"].into_iter()).is_ok());
        assert!(check_prefix_free(["a/b", "a/c"].into_iter()).is_ok());
        assert!(
            check_prefix_free(["ab", "a/b"].into_iter()).is_ok(),
            "not a prefix segment"
        );
        assert!(check_prefix_free(["a", "a/b"].into_iter()).is_err());
        assert!(check_prefix_free(["a/b/c", "a/b"].into_iter()).is_err());
    }

    /// Obviously-correct reference: for every path, test every ancestor prefix
    /// for membership. O(n * depth) with a set lookup, no ordering assumptions.
    fn prefix_free_reference(paths: &[&str]) -> bool {
        let all: std::collections::HashSet<&str> = paths.iter().copied().collect();
        for path in paths {
            for (index, byte) in path.bytes().enumerate() {
                if byte == b'/' && all.contains(&path[..index]) {
                    return false;
                }
            }
        }
        true
    }

    #[test]
    fn prefix_free_check_tolerates_duplicate_paths() {
        // A public function must not panic on input a caller can construct.
        assert!(check_prefix_free(["a", "a"].into_iter()).is_ok());
        assert!(check_prefix_free(["a/b", "a/b", "a/b"].into_iter()).is_ok());
        assert!(check_prefix_free(["a", "a", "a/b"].into_iter()).is_err());
    }

    #[test]
    fn prefix_free_stack_scan_matches_the_reference_exhaustively() {
        // The stack scan relies on sorted order putting every ancestor before
        // its descendants, and on popping leaving the nearest candidate on top.
        // That reasoning is subtle enough to be worth proving rather than
        // asserting: this sweeps every subset of an alphabet chosen to include
        // the separators that sort below `/` and the near-miss prefixes.
        const ALPHABET: [&str; 10] = [
            "x", "x/z", "x/z/w", "x-y", "x.y", "xy", "y", "y/x", "a", "a/b",
        ];
        let mut checked = 0usize;
        for mask in 0u32..(1 << ALPHABET.len()) {
            let subset: Vec<&str> = (0..ALPHABET.len())
                .filter(|i| mask & (1 << i) != 0)
                .map(|i| ALPHABET[i])
                .collect();
            let expected = prefix_free_reference(&subset);
            let actual = check_prefix_free(subset.iter().copied()).is_ok();
            assert_eq!(
                actual, expected,
                "disagreement on {subset:?}: stack scan said ok={actual}"
            );
            checked += 1;
        }
        assert_eq!(checked, 1 << ALPHABET.len());
    }

    #[test]
    fn prefix_free_check_sees_past_an_intervening_sibling() {
        // Regression: an adjacent-pair scan misses this. Every byte below `/`
        // (0x2F) sorts between a path and its descendants, so "x-y" lands
        // between "x" and "x/z" and hides the conflict from a windows(2) scan.
        let mut order = vec!["x", "x-y", "x/z"];
        order.sort_unstable();
        assert_eq!(
            order,
            ["x", "x-y", "x/z"],
            "the sibling really does separate them"
        );

        assert!(check_prefix_free(["x", "x/z"].into_iter()).is_err());
        assert!(
            check_prefix_free(["x", "x-y", "x/z"].into_iter()).is_err(),
            "the same conflict must still be caught when separated"
        );
        // Every byte below '/' behaves the same way.
        for sep in [
            "x!y", "x#y", "x$y", "x%y", "x&y", "x(y", "x+y", "x,y", "x.y",
        ] {
            assert!(
                check_prefix_free(["x", sep, "x/z"].into_iter()).is_err(),
                "separator {sep:?} must not hide the conflict"
            );
        }
        // A deeper ancestor must be found too.
        assert!(check_prefix_free(["a", "a-x", "a/b/c"].into_iter()).is_err());
    }

    // -- SPEC §4.2 messages ------------------------------------------------

    #[test]
    fn message_rules_allow_tab_and_lf_only() {
        assert!(validate_message("ok").is_ok());
        assert!(validate_message("two\nlines\there").is_ok());
        assert!(validate_message("").is_err());
        assert!(validate_message("bell\u{7}").is_err());
        assert!(validate_message("cr\r").is_err());
    }

    // -- SPEC §4.4 edit scripts --------------------------------------------

    #[test]
    fn edit_scripts_reject_adjacent_operations_of_one_kind() {
        assert!(EditScript::new(vec![EditOp::Retain(1), EditOp::Retain(1)]).is_err());
        assert!(EditScript::new(vec![
            EditOp::Insert(vec!["a\n".into()]),
            EditOp::Insert(vec!["b\n".into()])
        ])
        .is_err());
        assert!(EditScript::new(vec![EditOp::Delete(1), EditOp::Delete(1)]).is_err());
        assert!(EditScript::new(vec![EditOp::Retain(1), EditOp::Delete(1)]).is_ok());
        assert!(EditScript::new(vec![EditOp::Delete(1), EditOp::Retain(1)]).is_ok());
        assert!(
            EditScript::new(vec![EditOp::Delete(1), EditOp::Insert(vec!["x\n".into()])]).is_ok()
        );
    }

    #[test]
    fn edit_scripts_reject_zero_counts_and_empty_inserts() {
        assert!(EditScript::new(vec![EditOp::Retain(0)]).is_err());
        assert!(EditScript::new(vec![EditOp::Delete(0)]).is_err());
        assert!(EditScript::new(vec![EditOp::Insert(vec![])]).is_err());
        assert!(EditScript::new(vec![EditOp::Insert(vec![String::new()])]).is_err());
        assert!(
            EditScript::new(vec![EditOp::Retain(MAX_REVISION + 1)]).is_err(),
            "counts above MAX_REVISION must be rejected"
        );
        assert!(
            EditScript::new(vec![EditOp::Delete(MAX_REVISION + 1)]).is_err(),
            "counts above MAX_REVISION must be rejected"
        );
    }

    #[test]
    fn consumed_counts_retains_and_deletes_only() {
        let script = EditScript::new(vec![
            EditOp::Retain(2),
            EditOp::Insert(vec!["x\n".into()]),
            EditOp::Delete(3),
        ])
        .unwrap();
        assert_eq!(script.consumed(), 5);
    }

    // -- SPEC §4.1 repository ----------------------------------------------

    #[test]
    fn parses_the_spec_example() {
        let repo = Repository::from_json_str(SPEC_EXAMPLE).expect("valid");
        assert_eq!(repo.frontier.to_string(), "(alice@example.com->1)");
        assert_eq!(repo.patches.len(), 1);
        let patch = &repo.patches[0];
        assert_eq!(patch.dot(), ("alice@example.com", 1));
        assert_eq!(patch.result().to_string(), "(alice@example.com->1)");
        assert_eq!(patch.changes[0].path, "hello.txt");
    }

    #[test]
    fn serialization_is_byte_stable_across_round_trips() {
        // The property `trees_equal` depends on: identical typed value must
        // produce identical bytes, no matter how the value was reached.
        let repo = Repository::from_json_str(SPEC_EXAMPLE).unwrap();
        let once = repo.to_canonical_string();
        let twice = Repository::from_json_str(&once)
            .unwrap()
            .to_canonical_string();
        assert_eq!(once, twice);
        assert!(once.ends_with("}\n"), "trailing LF required by SPEC §4.1");
        assert!(once.contains("\n  \"format\": 1,"), "two-space indentation");
    }

    #[test]
    fn empty_repository_matches_what_init_must_write() {
        let text = Repository::default().to_canonical_string();
        assert_eq!(
            text,
            "{\n  \"format\": 1,\n  \"frontier\": [],\n  \"patches\": []\n}\n"
        );
    }

    #[test]
    fn rejects_unknown_and_missing_repository_fields() {
        assert!(Repository::from_json_str(r#"{"format":1,"frontier":[]}"#).is_err());
        assert!(
            Repository::from_json_str(r#"{"format":1,"frontier":[],"patches":[],"x":1}"#).is_err()
        );
        assert!(Repository::from_json_str(r#"{"format":2,"frontier":[],"patches":[]}"#).is_err());
    }

    #[test]
    fn rejects_unsorted_patches_and_changes() {
        let unsorted_changes = r#"{"format":1,"frontier":[],"patches":[
          {"author":"a@x","revision":1,"base":[],"message":"m","changes":[
            {"type":"delete","path":"b"},{"type":"delete","path":"a"}]}]}"#;
        assert!(Repository::from_json_str(unsorted_changes).is_err());

        let duplicate_paths = r#"{"format":1,"frontier":[],"patches":[
          {"author":"a@x","revision":1,"base":[],"message":"m","changes":[
            {"type":"delete","path":"a"},{"type":"delete","path":"a"}]}]}"#;
        assert!(Repository::from_json_str(duplicate_paths).is_err());
    }

    #[test]
    fn rejects_unsorted_or_duplicated_version_entries() {
        let unsorted = r#"{"format":1,"frontier":[["b@x",1],["a@x",1]],"patches":[]}"#;
        assert!(Repository::from_json_str(unsorted).is_err());
        let duplicated = r#"{"format":1,"frontier":[["a@x",1],["a@x",2]],"patches":[]}"#;
        assert!(Repository::from_json_str(duplicated).is_err());
        let zero = r#"{"format":1,"frontier":[["a@x",0]],"patches":[]}"#;
        assert!(Repository::from_json_str(zero).is_err());
    }

    #[test]
    fn rejects_empty_change_lists_and_bad_change_shapes() {
        let empty = r#"{"format":1,"frontier":[],"patches":[
          {"author":"a@x","revision":1,"base":[],"message":"m","changes":[]}]}"#;
        assert!(Repository::from_json_str(empty).is_err());

        let extra_field = r#"{"format":1,"frontier":[],"patches":[
          {"author":"a@x","revision":1,"base":[],"message":"m","changes":[
            {"type":"delete","path":"a","content":"AA=="}]}]}"#;
        assert!(Repository::from_json_str(extra_field).is_err());
    }

    #[test]
    fn rejects_non_canonical_base64_content() {
        let bad = r#"{"format":1,"frontier":[],"patches":[
          {"author":"a@x","revision":1,"base":[],"message":"m","changes":[
            {"type":"put","path":"a","content":"Zh=="}]}]}"#;
        assert!(Repository::from_json_str(bad).is_err());
    }

    #[test]
    fn put_content_survives_arbitrary_bytes() {
        let bytes: Vec<u8> = (0..=255u8).collect();
        let repo = Repository {
            frontier: Version::parse("(a@x->1)").unwrap(),
            patches: vec![Patch {
                author: "a@x".into(),
                revision: 1,
                base: Version::empty(),
                message: "binary".into(),
                changes: vec![Change {
                    path: "blob.bin".into(),
                    kind: ChangeKind::Put(bytes.clone().into()),
                }],
            }],
        };
        let reparsed = Repository::from_json_str(&repo.to_canonical_string()).unwrap();
        assert_eq!(reparsed, repo);
    }

    #[test]
    fn result_does_not_panic_on_a_hand_built_invalid_patch() {
        // `Patch` has public fields, so a library caller can construct one the
        // parser would have rejected. `result()` is infallible by signature, so
        // it must degrade rather than abort the process (exit code 2).
        let patch = Patch {
            author: "not-an-email".to_string(),
            revision: 1,
            base: Version::empty(),
            message: "hand built".to_string(),
            changes: vec![Change {
                path: "f".into(),
                kind: ChangeKind::Delete,
            }],
        };
        assert_eq!(patch.result().get("not-an-email"), 1);

        // And such a patch still cannot round-trip through the reader.
        let repo = Repository {
            frontier: patch.result(),
            patches: vec![patch],
        };
        assert!(
            Repository::from_json_str(&repo.to_canonical_string()).is_err(),
            "a malformed author must not survive serialization"
        );
    }

    #[test]
    fn find_locates_patches_by_dot() {
        let repo = Repository::from_json_str(SPEC_EXAMPLE).unwrap();
        assert!(repo.find("alice@example.com", 1).is_some());
        assert!(repo.find("alice@example.com", 2).is_none());
        assert!(repo.find("bob@example.com", 1).is_none());
    }
}
