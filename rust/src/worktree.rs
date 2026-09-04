//! Working-tree scanning and materialization (SPEC §2, §10).

use crate::error::{self, Result};
use crate::model::{self, Content, Tree};
use crate::present::StatusCode;
use std::path::{Path, PathBuf};

pub const SNAP_DIR: &str = ".snap";
pub const REPOSITORY_FILE: &str = "repository.json";

/// Walk from `start` to the filesystem root looking for a repository
/// (SPEC §7).
#[must_use]
pub fn discover(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        if dir.join(SNAP_DIR).join(REPOSITORY_FILE).is_file() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

#[must_use]
pub fn repository_path(root: &Path) -> PathBuf {
    root.join(SNAP_DIR).join(REPOSITORY_FILE)
}

/// Read every tracked file below `root` (SPEC §2).
///
/// `.snap/` is excluded. Symlinks and other non-regular entries are reported
/// rather than followed or ignored, which SPEC §10 requires of every command
/// that scans the working tree.
pub fn scan(root: &Path) -> Result<Tree> {
    let mut tree = Tree::new();
    scan_into(root, root, &mut tree)?;
    Ok(tree)
}

fn scan_into(root: &Path, dir: &Path, tree: &mut Tree) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| error::Error::new(format!("cannot read {}: {e}", dir.display())))?;
    // Sorted so failures are deterministic: the same unsupported entry is
    // reported first on every platform, whatever order the OS enumerates in.
    let mut names: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|e| error::Error::new(format!("cannot read {}: {e}", dir.display())))?;
        names.push(entry.path());
    }
    names.sort();

    for path in names {
        let relative = relative_path(root, &path)?;
        if dir == root && relative == SNAP_DIR {
            continue;
        }
        // `symlink_metadata` does not follow, so a symlink is seen as one.
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| error::Error::new(format!("cannot stat {}: {e}", path.display())))?;
        let file_type = meta.file_type();
        if file_type.is_dir() {
            scan_into(root, &path, tree)?;
        } else if file_type.is_file() {
            model::validate_path(&relative)?;
            let bytes = std::fs::read(&path)
                .map_err(|e| error::Error::new(format!("cannot read {}: {e}", path.display())))?;
            tree.insert(relative, Content::from(bytes));
        } else {
            // Symlinks, FIFOs, sockets, devices.
            return Err(error::unsupported_entry(&relative));
        }
    }
    Ok(())
}

fn relative_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| error::invalid_path(&path.display().to_string()))?;
    let mut out = String::new();
    for component in relative.components() {
        let std::path::Component::Normal(part) = component else {
            return Err(error::invalid_path(&relative.display().to_string()));
        };
        let part = part
            .to_str()
            .ok_or_else(|| error::invalid_path("non-UTF-8 path"))?;
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(part);
    }
    Ok(out)
}

/// Compare the current tree with the working tree (SPEC §7.3), sorted by path.
#[must_use]
pub fn status(current: &Tree, working: &Tree) -> Vec<(String, StatusCode)> {
    let mut rows = Vec::new();
    for (path, bytes) in working {
        match current.get(path) {
            None => rows.push((path.clone(), StatusCode::Added)),
            Some(old) if old.as_ref() != bytes.as_ref() => {
                rows.push((path.clone(), StatusCode::Modified));
            }
            Some(_) => {}
        }
    }
    for path in current.keys() {
        if !working.contains_key(path) {
            rows.push((path.clone(), StatusCode::Deleted));
        }
    }
    // `String` compares by bytes, which is SPEC §2's path order.
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

/// Write `target` into the working tree (SPEC §6.2).
///
/// Removes files that block required directories, creates directories, writes
/// changed files, and removes newly empty directories, so the filesystem
/// represents exactly the target path/byte map. Files whose bytes already
/// match are left alone — the end state is identical and SPEC §2 tracks no
/// timestamps, so rewriting them would be pure I/O.
pub fn materialize(root: &Path, current: &Tree, target: &Tree) -> Result<()> {
    for path in current.keys() {
        if !target.contains_key(path) {
            let full = root.join(path);
            if full.exists() {
                std::fs::remove_file(&full).map_err(|e| {
                    error::Error::new(format!("cannot remove {}: {e}", full.display()))
                })?;
            }
        }
    }
    for (path, bytes) in target {
        if current
            .get(path)
            .is_some_and(|old| old.as_ref() == bytes.as_ref())
        {
            continue;
        }
        let full = root.join(path);
        // A directory may occupy a path that must now hold a file. Everything
        // under it that belonged to the old tree has already been removed
        // above, and prefix-freeness (SPEC §2) means no target path lives
        // inside it, so removing it cannot destroy wanted content.
        if full.is_dir() {
            std::fs::remove_dir_all(&full)
                .map_err(|e| error::Error::new(format!("cannot remove {}: {e}", full.display())))?;
        }
        if let Some(parent) = full.parent() {
            // A file may occupy a path a directory now needs.
            clear_blocking_files(parent)?;
            std::fs::create_dir_all(parent).map_err(|e| {
                error::Error::new(format!("cannot create {}: {e}", parent.display()))
            })?;
        }
        std::fs::write(&full, bytes.as_ref())
            .map_err(|e| error::Error::new(format!("cannot write {}: {e}", full.display())))?;
    }
    remove_empty_directories(root, root)?;
    Ok(())
}

fn clear_blocking_files(dir: &Path) -> Result<()> {
    let mut ancestor = Some(dir);
    while let Some(path) = ancestor {
        if path.is_file() {
            std::fs::remove_file(path)
                .map_err(|e| error::Error::new(format!("cannot remove {}: {e}", path.display())))?;
        }
        ancestor = path.parent();
    }
    Ok(())
}

/// Depth-first removal of directories left empty by materialization. The
/// repository root and `.snap/` are never removed.
fn remove_empty_directories(root: &Path, dir: &Path) -> Result<bool> {
    let mut empty = true;
    let entries = std::fs::read_dir(dir)
        .map_err(|e| error::Error::new(format!("cannot read {}: {e}", dir.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| error::Error::new(format!("cannot read {}: {e}", dir.display())))?;
        let path = entry.path();
        if dir == root && path.file_name().is_some_and(|n| n == SNAP_DIR) {
            empty = false;
            continue;
        }
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| error::Error::new(format!("cannot stat {}: {e}", path.display())))?;
        if meta.file_type().is_dir() {
            if remove_empty_directories(root, &path)? {
                std::fs::remove_dir(&path).map_err(|e| {
                    error::Error::new(format!("cannot remove {}: {e}", path.display()))
                })?;
            } else {
                empty = false;
            }
        } else {
            empty = false;
        }
    }
    Ok(empty && dir != root)
}

/// Replace a file atomically through a same-directory temporary (SPEC §10).
///
/// On POSIX, `rename()` is atomic when source and destination are on the same
/// filesystem.  On Windows, `rename()` fails when the destination already
/// exists, so we remove first — non-atomic but correct.  SPEC §12 puts
/// crash recovery out of scope, so the brief window between remove and rename
/// is acceptable.
pub fn replace_file(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| error::Error::new("invalid path"))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| error::Error::new(format!("cannot create {}: {e}", parent.display())))?;
    let temporary = parent.join(format!(".{}.tmp", std::process::id()));
    std::fs::write(&temporary, contents)
        .map_err(|e| error::Error::new(format!("cannot write {}: {e}", temporary.display())))?;
    if std::fs::rename(&temporary, path).is_err() {
        // Windows: rename fails when the destination exists.  Remove and retry.
        let _ = std::fs::remove_file(path);
        std::fs::rename(&temporary, path)
            .map_err(|e| error::Error::new(format!("cannot replace {}: {e}", path.display())))?;
    }
    Ok(())
}
