//! Argument grammar, repository discovery, and the eight commands (SPEC §7).
//!
//! [`run`] takes its whole world as parameters — arguments, working directory,
//! environment, TTY-ness, and both output streams — so every command is
//! testable in-process without spawning anything or touching global state.

use crate::error::{self, Error, Result};
use crate::model::{
    self, Change, ChangeKind, Content, Patch, Repository, Tree, MAX_COMMIT_MESSAGE_BYTES,
};
use crate::present::{self, LogEntry, Mode, Presentation, Success};
use crate::replay::{self, Warnings};
use crate::text;
use crate::version::{validate_contributor_id, Version};
use crate::{config, http, worktree};
use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const SEMVER: &str = env!("CARGO_PKG_VERSION");

/// Everything the process learns from outside itself.
#[derive(Debug, Clone)]
pub struct Env {
    pub cwd: PathBuf,
    pub home: Option<PathBuf>,
    pub snap_color: Option<String>,
    /// SPEC §7.11: presence alone counts, including an empty value.
    pub no_color: bool,
    pub stdout_tty: bool,
    pub stderr_tty: bool,
}

/// Run one command. Returns the process exit code (SPEC §10).
pub fn run(args: &[String], env: &Env, stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    // SPEC §7.11: an invalid SNAP_COLOR is an error *before* command
    // execution, reported plainly because no presentation was selected.
    let presentation = match present::resolve(
        env.snap_color.as_deref(),
        env.no_color,
        env.stdout_tty,
        env.stderr_tty,
    ) {
        Ok(p) => p,
        Err(e) => {
            let _ = stderr.write_all(present::error_line(Mode::Plain, e.detail()).as_bytes());
            return 1;
        }
    };

    match dispatch(args, env, presentation, stdout, stderr) {
        Ok(()) => 0,
        Err(e) => {
            let _ =
                stderr.write_all(present::error_line(presentation.stderr, e.detail()).as_bytes());
            1
        }
    }
}

fn dispatch(
    args: &[String],
    env: &Env,
    presentation: Presentation,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["--version"] => write_out(stdout, &present::version_line(presentation.stdout, SEMVER)),
        ["--serve"] => cmd_serve(env, None, stdout),
        ["--serve", port] => cmd_serve(env, Some(port), stdout),
        ["init"] => cmd_init(env, ".", presentation, stdout),
        ["init", path] => cmd_init(env, operand(path)?, presentation, stdout),
        ["config", "contributor.id", id] => cmd_config(env, id, false),
        ["config", "--global", "contributor.id", id] => cmd_config(env, id, true),
        ["status"] => cmd_status(env, presentation, stdout),
        ["log"] => cmd_log(env, presentation, stdout),
        ["commit", message] => cmd_commit(env, message, presentation, stdout),
        ["diff"] => cmd_diff_working(env, presentation, stdout),
        ["diff", old, new] => cmd_diff_versions(env, old, new, None, presentation, stdout),
        ["diff", old, new, "--repo", repo] => {
            cmd_diff_versions(env, old, new, Some(repo), presentation, stdout)
        }
        // A `diff` shape that is close but wrong gets the usage hint; anything
        // else is the generic grammar error.
        ["diff", ..] => Err(error::usage_diff()),
        ["revert", version] => cmd_revert(env, version, presentation, stdout),
        ["merge", repository] => cmd_merge(env, operand(repository)?, presentation, stdout, stderr),
        _ => Err(error::invalid_command()),
    }
}

/// Reject an operand that looks like an option.
///
/// Without this, `snap init --unknown` would create a directory called
/// `--unknown`; `24-cli-grammar-matrix` asserts that path does not exist.
fn operand(value: &str) -> Result<&str> {
    if value.starts_with('-') {
        return Err(error::invalid_command());
    }
    Ok(value)
}

fn write_out(stdout: &mut dyn Write, text: &str) -> Result<()> {
    stdout
        .write_all(text.as_bytes())
        .map_err(|e| Error::new(format!("cannot write output: {e}")))
}

// -- Repository loading ----------------------------------------------------

struct Session {
    root: PathBuf,
    repository: Repository,
}

/// Locate and fully validate the nearest repository (SPEC §4.5, §7).
fn open(env: &Env) -> Result<Session> {
    let root = worktree::discover(&env.cwd).ok_or_else(error::not_a_repository)?;
    let text = std::fs::read_to_string(worktree::repository_path(&root))
        .map_err(|e| Error::new(format!("cannot read repository: {e}")))?;
    let repository = load(&text)?;
    Ok(Session { root, repository })
}

/// Parse and validate a repository value (SPEC §4.5).
fn load(text: &str) -> Result<Repository> {
    let repository = Repository::from_json_str(text)?;
    validate(&repository)?;
    Ok(repository)
}

/// SPEC §4.5's validation passes beyond what parsing already enforces:
/// contiguous contributor revisions, `revision = base[author] + 1`, complete
/// base closure, acyclicity, and a deterministic replay of the frontier.
fn validate(repository: &Repository) -> Result<()> {
    for patch in &repository.patches {
        let expected = patch
            .base
            .get(&patch.author)
            .checked_add(1)
            .ok_or_else(|| error::invalid_json("revision overflow"))?;
        if patch.revision != expected {
            return Err(error::invalid_json("revision must be base[author] + 1"));
        }
        for (id, revision) in patch.base.iter() {
            if repository.find(id, revision).is_none() {
                return Err(error::missing_base(&format!("{id}@{revision}")));
            }
        }
    }
    // Contiguity: revision n must follow n-1 for each contributor (SPEC §3.5).
    for window in repository.patches.windows(2) {
        let (a, b) = (&window[0], &window[1]);
        if a.author == b.author {
            let next = a
                .revision
                .checked_add(1)
                .ok_or_else(|| error::invalid_json("revision overflow"))?;
            if b.revision != next {
                return Err(error::cyclic_history());
            }
        }
    }
    for patch in &repository.patches {
        if patch.revision == 1 {
            continue;
        }
        if repository.find(&patch.author, patch.revision - 1).is_none() {
            return Err(error::cyclic_history());
        }
    }
    // `patches` must be exactly the causal closure of `frontier`, with nothing
    // unreachable (SPEC §4.1).
    for patch in &repository.patches {
        if patch.revision > repository.frontier.get(&patch.author) {
            // SPEC §4.1: `patches` is exactly the causal closure of
            // `frontier`, "with no unreachable patches".
            return Err(error::unreachable_patch(&patch.author, patch.revision));
        }
    }
    for (id, revision) in repository.frontier.iter() {
        if repository.find(id, revision).is_none() {
            return Err(error::missing_base(&format!("{id}@{revision}")));
        }
    }
    // Replay proves acyclicity and that every change applies to its base.
    replay::materialize(repository, &repository.frontier)?;
    Ok(())
}

fn current_tree(repository: &Repository) -> Result<Tree> {
    replay::materialize_tree(repository, &repository.frontier)
}

// -- init ------------------------------------------------------------------

fn cmd_init(
    env: &Env,
    path: &str,
    presentation: Presentation,
    stdout: &mut dyn Write,
) -> Result<()> {
    let target = env.cwd.join(path);
    if target
        .join(worktree::SNAP_DIR)
        .join(worktree::REPOSITORY_FILE)
        .is_file()
    {
        return Err(error::repository_exists());
    }
    // An existing repository anywhere above the target forbids nesting.
    let search_base = if target.exists() {
        target.clone()
    } else {
        target
            .parent()
            .map_or_else(|| target.clone(), Path::to_path_buf)
    };
    if let Some(existing) = worktree::discover(&search_base) {
        if existing != target {
            return Err(error::nested_repository());
        }
        return Err(error::repository_exists());
    }
    std::fs::create_dir_all(target.join(worktree::SNAP_DIR))
        .map_err(|e| Error::new(format!("cannot create repository: {e}")))?;
    let repository = Repository::default();
    worktree::replace_file(
        &worktree::repository_path(&target),
        &repository.to_canonical_string(),
    )?;
    write_out(
        stdout,
        &present::success(presentation.stdout, Success::Initialized, &Version::empty()),
    )
}

// -- config ----------------------------------------------------------------

fn cmd_config(env: &Env, id: &str, global: bool) -> Result<()> {
    // SPEC §7.2: validate before writing.
    validate_contributor_id(id)?;
    let path = if global {
        config::global_path(env.home.as_deref())
            .ok_or_else(|| Error::new("HOME is not set; global configuration is unavailable"))?
    } else {
        let root = worktree::discover(&env.cwd).ok_or_else(error::not_a_repository)?;
        config::local_path(&root)
    };
    worktree::replace_file(&path, &config::render(id))
}

// -- status ----------------------------------------------------------------

fn cmd_status(env: &Env, presentation: Presentation, stdout: &mut dyn Write) -> Result<()> {
    let session = open(env)?;
    let current = current_tree(&session.repository)?;
    let working = worktree::scan(&session.root)?;
    let rows = worktree::status(&current, &working);
    write_out(
        stdout,
        &present::status(presentation.stdout, &session.repository.frontier, &rows),
    )
}

// -- log -------------------------------------------------------------------

fn cmd_log(env: &Env, presentation: Presentation, stdout: &mut dyn Write) -> Result<()> {
    let session = open(env)?;
    let ordered = replay::canonical_order(&session.repository, &session.repository.frontier)?;
    // SPEC §7.4: reverse canonical integration order.
    let entries: Vec<LogEntry> = ordered
        .iter()
        .rev()
        .map(|patch| LogEntry {
            version: patch.result(),
            author: patch.author.clone(),
            message: present::escape_message(&patch.message),
        })
        .collect();
    write_out(stdout, &present::log(presentation.stdout, &entries))
}

// -- commit ----------------------------------------------------------------

/// Build the change list taking `current` to `working` (SPEC §7.5).
fn changes_between(current: &Tree, working: &Tree) -> Vec<Change> {
    let mut changes = Vec::new();
    for (path, new_bytes) in working {
        let old = current.get(path);
        if old.is_some_and(|old| old.as_ref() == new_bytes.as_ref()) {
            continue;
        }
        // Text when the new content is text and the old path is absent or
        // text; otherwise an atomic replacement.
        let new_is_text = text::is_text(new_bytes);
        let old_is_text = old.is_none_or(|bytes| text::is_text(bytes));
        let kind = if new_is_text && old_is_text {
            let old_text = old.map_or(String::new(), |b| String::from_utf8_lossy(b).to_string());
            let new_text = String::from_utf8_lossy(new_bytes).to_string();
            ChangeKind::Text(text::diff(
                &text::tokenize(&old_text),
                &text::tokenize(&new_text),
            ))
        } else {
            ChangeKind::Put(new_bytes.clone())
        };
        changes.push(Change {
            path: path.clone(),
            kind,
        });
    }
    for path in current.keys() {
        if !working.contains_key(path) {
            changes.push(Change {
                path: path.clone(),
                kind: ChangeKind::Delete,
            });
        }
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    changes
}

fn cmd_commit(
    env: &Env,
    message: &str,
    presentation: Presentation,
    stdout: &mut dyn Write,
) -> Result<()> {
    let mut session = open(env)?;
    // SPEC §7.5: user-supplied messages are capped at 4096 bytes. Generated
    // revert messages are exempt (SPEC §4.2), so the cap lives here and not in
    // `validate_message`.
    // A user-supplied message reports as "invalid commit message" whatever is
    // wrong with it. `model::validate_message` speaks about *patches* and is
    // for reading repositories, where SPEC §4.2's wording differs.
    if message.len() > MAX_COMMIT_MESSAGE_BYTES || model::validate_message(message).is_err() {
        return Err(error::invalid_commit_message());
    }
    let author = config::resolve(Some(&session.root), env.home.as_deref())?
        .ok_or_else(error::contributor_required)?;

    let current = current_tree(&session.repository)?;
    let working = worktree::scan(&session.root)?;
    let changes = changes_between(&current, &working);
    if changes.is_empty() {
        return Err(error::working_tree_clean());
    }
    let version = author_patch(&mut session.repository, &author, message, changes)?;
    // Only the metadata needs replacing: the working files are already right.
    worktree::replace_file(
        &worktree::repository_path(&session.root),
        &session.repository.to_canonical_string(),
    )?;
    write_out(
        stdout,
        &present::success(presentation.stdout, Success::Committed, &version),
    )
}

/// Append one patch authored on the current frontier (SPEC §4.2).
fn author_patch(
    repository: &mut Repository,
    author: &str,
    message: &str,
    changes: Vec<Change>,
) -> Result<Version> {
    let base = repository.frontier.clone();
    let revision = base
        .get(author)
        .checked_add(1)
        .ok_or_else(|| error::invalid_json("revision overflow"))?;
    if repository.find(author, revision).is_some() {
        return Err(error::patch_collision(author, revision));
    }
    let patch = Patch {
        author: author.to_string(),
        revision,
        base,
        message: message.to_string(),
        changes,
    };
    let result = patch.result();
    repository.patches.push(patch);
    repository.sort_patches();
    repository.frontier = result.clone();
    Ok(result)
}

// -- diff ------------------------------------------------------------------

fn cmd_diff_working(env: &Env, presentation: Presentation, stdout: &mut dyn Write) -> Result<()> {
    let session = open(env)?;
    let current = current_tree(&session.repository)?;
    let working = worktree::scan(&session.root)?;
    let rendered = render_diff(&current, &working);
    write_out(stdout, &present::diff(presentation.stdout, &rendered))
}

fn cmd_diff_versions(
    env: &Env,
    old: &str,
    new: &str,
    repo: Option<&str>,
    presentation: Presentation,
    stdout: &mut dyn Write,
) -> Result<()> {
    let session = open(env)?;
    let old_version = Version::parse(old)?;
    let new_version = Version::parse(new)?;
    let old_tree = known_tree(&session.repository, &old_version)?;
    let new_tree = match repo {
        None => known_tree(&session.repository, &new_version)?,
        Some(operand) => {
            let other = load_operand(env, operand)?;
            // SPEC §7.6: a cross-repository diff must also compare every dot
            // present in both and fail as corrupt if the values differ.
            check_dot_collisions(&session.repository, &other)?;
            known_tree(&other, &new_version)?
        }
    };
    let rendered = render_diff(&old_tree, &new_tree);
    write_out(stdout, &present::diff(presentation.stdout, &rendered))
}

/// Materialize a version, rejecting one the repository does not know
/// (SPEC §4.1).
fn known_tree(repository: &Repository, version: &Version) -> Result<Tree> {
    for (id, revision) in version.iter() {
        if repository.find(id, revision).is_none() {
            return Err(error::unknown_version(&version.to_string()));
        }
    }
    replay::materialize_tree(repository, version)
        .map_err(|_| error::unknown_version(&version.to_string()))
}

/// Render the plain unified diff of SPEC §7.6.
fn render_diff(old: &Tree, new: &Tree) -> String {
    let mut paths: Vec<&String> = old.keys().chain(new.keys()).collect();
    paths.sort();
    paths.dedup();

    let mut out = String::new();
    for path in paths {
        let before = old.get(path);
        let after = new.get(path);
        if before.map(Content::as_ref) == after.map(Content::as_ref) {
            continue;
        }
        let both_text =
            before.is_none_or(|b| text::is_text(b)) && after.is_none_or(|b| text::is_text(b));
        let a_label = if before.is_some() {
            format!("a/{path}")
        } else {
            "/dev/null".to_string()
        };
        let b_label = if after.is_some() {
            format!("b/{path}")
        } else {
            "/dev/null".to_string()
        };
        if !both_text {
            let _ = writeln!(out, "Binary files {a_label} and {b_label} differ");
            continue;
        }
        let old_text = before.map_or(String::new(), |b| String::from_utf8_lossy(b).to_string());
        let new_text = after.map_or(String::new(), |b| String::from_utf8_lossy(b).to_string());
        let old_tokens = text::tokenize(&old_text);
        let new_tokens = text::tokenize(&new_text);
        let _ = write!(out, "--- {a_label}\n+++ {b_label}\n");
        let _ = writeln!(out, "@@ -1,{} +1,{} @@", old_tokens.len(), new_tokens.len());
        let script = text::diff(&old_tokens, &new_tokens);
        let mut cursor = 0usize;
        for op in script.ops() {
            match op {
                model::EditOp::Retain(n) => {
                    for token in &old_tokens[cursor..cursor + *n as usize] {
                        push_diff_line(&mut out, ' ', token);
                    }
                    cursor += *n as usize;
                }
                model::EditOp::Delete(n) => {
                    for token in &old_tokens[cursor..cursor + *n as usize] {
                        push_diff_line(&mut out, '-', token);
                    }
                    cursor += *n as usize;
                }
                model::EditOp::Insert(tokens) => {
                    for token in tokens {
                        push_diff_line(&mut out, '+', token);
                    }
                }
            }
        }
    }
    out
}

/// SPEC §7.6: a token without a final LF is followed by LF and the marker.
fn push_diff_line(out: &mut String, prefix: char, token: &str) {
    out.push(prefix);
    out.push_str(token);
    if !token.ends_with('\n') {
        out.push('\n');
        out.push_str("\\ No newline at end of file\n");
    }
}

// -- revert ----------------------------------------------------------------

fn cmd_revert(
    env: &Env,
    version: &str,
    presentation: Presentation,
    stdout: &mut dyn Write,
) -> Result<()> {
    let mut session = open(env)?;
    // The target version is checked before the identity: `14-cli-errors`
    // reverts to an unknown version with no contributor configured and expects
    // to hear about the version.
    let target_version = Version::parse(version)?;
    let target = known_tree(&session.repository, &target_version)?;
    let author = config::resolve(Some(&session.root), env.home.as_deref())?
        .ok_or_else(error::contributor_required)?;

    let current = current_tree(&session.repository)?;
    let working = worktree::scan(&session.root)?;
    if !worktree::status(&current, &working).is_empty() {
        return Err(error::working_tree_dirty());
    }
    let changes = changes_between(&current, &target);
    if changes.is_empty() {
        return Err(error::already_current());
    }
    // SPEC §7.7: the generated message may exceed the commit cap.
    let message = format!("revert to {target_version}");
    let new_version = author_patch(&mut session.repository, &author, &message, changes)?;

    // SPEC §10: working files first, then the metadata.
    worktree::materialize(&session.root, &current, &target)?;
    worktree::replace_file(
        &worktree::repository_path(&session.root),
        &session.repository.to_canonical_string(),
    )?;
    write_out(
        stdout,
        &present::success(presentation.stdout, Success::Reverted, &new_version),
    )
}

// -- merge -----------------------------------------------------------------

fn load_operand(env: &Env, operand: &str) -> Result<Repository> {
    if operand.starts_with("http://") || operand.starts_with("https://") {
        let body = http::fetch(operand)?;
        return load(&body);
    }
    let root = env.cwd.join(operand);
    let path = worktree::repository_path(&root);
    let text = std::fs::read_to_string(&path).map_err(|_| error::not_a_repository())?;
    load(&text)
}

/// SPEC §3.5: the same dot with structurally different patches is corruption.
fn check_dot_collisions(local: &Repository, other: &Repository) -> Result<()> {
    for patch in &other.patches {
        if let Some(mine) = local.find(&patch.author, patch.revision) {
            if mine != patch {
                return Err(error::patch_collision(&patch.author, patch.revision));
            }
        }
    }
    Ok(())
}

fn cmd_merge(
    env: &Env,
    operand: &str,
    presentation: Presentation,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let session = open(env)?;
    let current = current_tree(&session.repository)?;
    let working = worktree::scan(&session.root)?;
    if !worktree::status(&current, &working).is_empty() {
        return Err(error::working_tree_dirty());
    }
    let other = load_operand(env, operand)?;
    check_dot_collisions(&session.repository, &other)?;

    // Import is set union; the frontier is the join (SPEC §7.8).
    //
    // The membership test is taken against a snapshot of the dots we started
    // with, never against `merged.patches` while it is being appended to:
    // `Repository::find` binary-searches, so consulting a half-updated vec
    // silently misses existing dots and admits duplicates.
    let existing: std::collections::HashSet<(&str, u64)> =
        session.repository.patches.iter().map(Patch::dot).collect();
    let mut merged = session.repository.clone();
    for patch in &other.patches {
        if !existing.contains(&patch.dot()) {
            merged.patches.push(patch.clone());
        }
    }
    merged.sort_patches();
    merged.frontier = session.repository.frontier.join(&other.frontier);

    // SPEC §10: everything is validated and built before anything is written.
    validate(&merged)?;
    let (target, joined_warnings) = replay::materialize(&merged, &merged.frontier)?;
    let (_, local_warnings) =
        replay::materialize(&session.repository, &session.repository.frontier)?;

    worktree::materialize(&session.root, &current, &target)?;
    worktree::replace_file(
        &worktree::repository_path(&session.root),
        &merged.to_canonical_string(),
    )?;

    write_new_warnings(
        &joined_warnings,
        &local_warnings,
        presentation.stderr,
        stderr,
    )?;
    write_out(
        stdout,
        &present::success(presentation.stdout, Success::Merged, &merged.frontier),
    )
}

/// SPEC §6.4: merge prints only pairs present in the joined replay but absent
/// from the pre-merge local replay.
fn write_new_warnings(
    joined: &Warnings,
    local: &Warnings,
    mode: Mode,
    stderr: &mut dyn Write,
) -> Result<()> {
    for (path, reason) in joined.difference(local) {
        stderr
            .write_all(present::warning(mode, path, *reason).as_bytes())
            .map_err(|e| Error::new(format!("cannot write warning: {e}")))?;
    }
    Ok(())
}

// -- serve -----------------------------------------------------------------

fn cmd_serve(env: &Env, port: Option<&str>, stdout: &mut dyn Write) -> Result<()> {
    let port = http::parse_port(port)?;
    let session = open(env)?;
    // SPEC §7.9: validate and snapshot at startup, then serve that snapshot.
    let snapshot = session.repository.to_canonical_string();
    http::serve(port, &snapshot, stdout)
}
