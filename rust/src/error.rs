//! Every user-visible failure, in one place.
//!
//! The acceptance suite pins error text exactly (18 distinct `stderr_equals`
//! strings) or by substring (18 more), so wording is part of the contract and
//! belongs in one reviewable module rather than scattered across call sites.
//!
//! Per SPEC §10 a plain-mode error is exactly one line, `snap: <detail>`.
//! `Error` carries only the `<detail>`; `present` adds the prefix and any
//! terminal styling.

use std::fmt;

/// An expected failure. Exits 1 (SPEC §10); genuine internal faults panic and
/// are mapped to exit 2 by `main`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    detail: String,
}

impl Error {
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Shorthand for constructing an [`Error`].
macro_rules! err {
    ($($arg:tt)*) => { $crate::error::Error::new(format!($($arg)*)) };
}

// -- Fixed wording pinned by `stderr_equals` ------------------------------

pub fn invalid_command() -> Error {
    Error::new("invalid command or arguments")
}
pub fn not_a_repository() -> Error {
    Error::new("not a Snap repository")
}
pub fn working_tree_clean() -> Error {
    Error::new("working tree is clean")
}
pub fn working_tree_dirty() -> Error {
    Error::new("working tree is dirty")
}
pub fn already_current() -> Error {
    Error::new("target tree is already current")
}
pub fn contributor_required() -> Error {
    Error::new("contributor.id is required; configure it locally or globally")
}
pub fn invalid_commit_message() -> Error {
    Error::new("invalid commit message")
}
pub fn bad_color_mode() -> Error {
    Error::new("SNAP_COLOR must be auto, always, or never")
}
pub fn invalid_port(raw: &str) -> Error {
    err!("invalid port: {raw}")
}
pub fn unsupported_entry(path: &str) -> Error {
    err!("unsupported working tree entry: {path}")
}
pub fn unknown_version(version: &str) -> Error {
    err!("unknown version: {version}")
}

// -- Wording pinned by `stderr_contains` ----------------------------------

pub fn repository_exists() -> Error {
    Error::new("repository already exists")
}
pub fn nested_repository() -> Error {
    Error::new("cannot initialize inside repository")
}
pub fn invalid_json(detail: &str) -> Error {
    err!("invalid JSON: {detail}")
}
pub fn duplicate_json_key(key: &str) -> Error {
    // No colon: `25-config-version-path-boundaries` matches
    // /^snap: duplicate JSON key .+\n$/.
    err!("duplicate JSON key {key}")
}
pub fn invalid_contributor_id(raw: &str) -> Error {
    err!("invalid contributor id: {raw}")
}
pub fn invalid_version(raw: &str) -> Error {
    err!("invalid version: {raw}")
}
pub fn invalid_path(raw: &str) -> Error {
    err!("path is invalid: {raw}")
}
pub fn not_canonical_base64() -> Error {
    Error::new("content is not canonical base64")
}
/// The script leaves old tokens unconsumed (SPEC §4.4: there is no implicit
/// trailing retain).
pub fn edit_does_not_consume() -> Error {
    Error::new("edit does not consume old content")
}

/// The script runs past the end of the old token sequence.
pub fn edit_consumes_beyond() -> Error {
    Error::new("edit consumes beyond old content")
}
pub fn adjacent_insert() -> Error {
    Error::new("edit has adjacent insert operations")
}
pub fn adjacent_same_kind(kind: &str) -> Error {
    err!("edit has adjacent {kind} operations")
}
pub fn tree_paths_conflict(path: &str) -> Error {
    err!("tree paths conflict at {path}")
}
pub fn cyclic_history() -> Error {
    Error::new("cyclic or incomplete patch history")
}
pub fn depth_limit_reached() -> Error {
    Error::new("base reconstruction depth limit exceeded")
}
pub fn missing_base(dot: &str) -> Error {
    err!("cyclic or incomplete patch history: missing {dot}")
}
pub fn no_op_change(path: &str) -> Error {
    err!("no-op change at {path}")
}

pub fn delete_of_absent_path(path: &str) -> Error {
    err!("delete of absent path: {path}")
}

pub fn repository_unknown_field(key: &str) -> Error {
    err!("repository has unknown field: {key}")
}

pub fn change_unknown_field(key: &str) -> Error {
    err!("change has unknown field: {key}")
}

pub fn patch_unknown_field(key: &str) -> Error {
    err!("patch has unknown field: {key}")
}

pub fn not_canonical(detail: &str) -> Error {
    err!("{detail} is not canonical")
}

pub fn not_positive_safe_integer(what: &str) -> Error {
    err!("{what} must be a positive safe integer")
}

pub fn unreachable_patch(author: &str, revision: u64) -> Error {
    err!("unreachable patch: {author}@{revision}")
}

pub fn message_is_empty() -> Error {
    Error::new("patch message is empty")
}

pub fn changes_is_empty() -> Error {
    Error::new("patch changes is empty")
}

pub fn edit_must_have_one_operation() -> Error {
    Error::new("edit entry must have one operation")
}

pub fn insert_is_empty() -> Error {
    Error::new("edit insert is empty")
}
pub fn patch_collision(author: &str, revision: u64) -> Error {
    // §3.5 makes this unrepairable, so name the cause as well as the dot.
    // `16-dot-collision` matches by substring, which leaves room to be useful.
    err!(
        "patch collision: {author} revision {revision}; \
         the same contributor id authored different patches in two repositories"
    )
}
pub fn http_status(status: u16) -> Error {
    err!("HTTP {status}")
}
pub fn usage_diff() -> Error {
    Error::new("usage: snap diff [<old> <new> [--repo <repository>]]")
}
