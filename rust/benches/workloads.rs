//! Benchmark workloads for the Snap Rust implementation.
//!
//! Run with `cargo bench`. Deliberately a plain binary rather than criterion:
//! the point is a repeatable number to justify or reject an optimization, not
//! statistical rigour, and the project carries one runtime dependency by
//! design.
//!
//! Workloads:
//! * linear        — 1,000 sequential patches, 1 file (rule-1 fast path + memo)
//! * divergent     — 2 branches × 250 patches, distinct files (non-prefix memo)
//! * wide-tree     — 5,000 files, 2 patches (tree scan cost)
//! * large-tree    — 1,000 patches × 1,000 files (`BTreeMap` cloning at scale)
//! * text-ot       — 2 branches × 250 edits, same file, overlapping (OT stress)
//! * deep-linear   — 10,000 / 100,000 patches, 1 file (depth + memo scaling)
//! * diff          — 400 × 400 tokens (SPEC §5 DP)

use snap::model::{Change, ChangeKind, Content, Patch, Repository};
use snap::replay;
use snap::text;
use snap::version::Version;
use std::fmt::Write as _;
use std::time::Instant;

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
        kind: ChangeKind::Put(Content::from(bytes.as_bytes().to_vec())),
    }
}

/// One contributor, `count` patches, each rewriting the same file.
fn linear_history(count: u64) -> Repository {
    let mut repository = Repository::default();
    let mut frontier = Version::empty();
    for revision in 1..=count {
        let from = if revision == 1 {
            String::new()
        } else {
            format!("line {}\n", revision - 1)
        };
        let to = format!("line {revision}\n");
        let patch = Patch {
            author: "a@x".to_string(),
            revision,
            base: frontier.clone(),
            message: format!("r{revision}"),
            changes: vec![text_change("f.txt", &from, &to)],
        };
        frontier = patch.result();
        repository.patches.push(patch);
    }
    repository.sort_patches();
    repository.frontier = frontier;
    repository
}

/// A shared root, then two contributors editing distinct files concurrently.
fn divergent_history(per_branch: u64) -> Repository {
    let mut repository = Repository::default();
    let root = Patch {
        author: "root@x".to_string(),
        revision: 1,
        base: Version::empty(),
        message: "root".to_string(),
        changes: vec![text_change("shared.txt", "", "a\nb\nc\n")],
    };
    let root_version = root.result();
    repository.patches.push(root);

    for author in ["left@x", "right@x"] {
        let mut base = root_version.clone();
        for revision in 1..=per_branch {
            let file = format!("{author}-{revision}.txt");
            let patch = Patch {
                author: author.to_string(),
                revision,
                base: base.clone(),
                message: format!("{author} r{revision}"),
                changes: vec![put(&file, "content\n")],
            };
            base = patch.result();
            repository.patches.push(patch);
        }
    }
    repository.sort_patches();
    repository.frontier = repository
        .patches
        .iter()
        .fold(Version::empty(), |acc, patch| acc.join(&patch.result()));
    repository
}

/// One patch creating `count` files, then one small follow-up patch.
fn wide_tree(count: usize) -> Repository {
    let mut repository = Repository::default();
    let mut changes: Vec<Change> = (0..count)
        .map(|i| put(&format!("dir{:03}/file{:05}.txt", i % 100, i), "content\n"))
        .collect();
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    let first = Patch {
        author: "a@x".to_string(),
        revision: 1,
        base: Version::empty(),
        message: "wide".to_string(),
        changes,
    };
    let after_first = first.result();
    let second = Patch {
        author: "a@x".to_string(),
        revision: 2,
        base: after_first,
        message: "touch".to_string(),
        changes: vec![put("dir000/file00000.txt", "changed\n")],
    };
    let frontier = second.result();
    repository.patches.push(first);
    repository.patches.push(second);
    repository.sort_patches();
    repository.frontier = frontier;
    repository
}

/// `patches` sequential patches over `files` files; each patch rewrites one
/// file. Measures `BTreeMap` structural cloning cost at scale.
fn large_tree(patches: u64, files: usize) -> Repository {
    let mut repository = Repository::default();
    // Seed the tree with `files` files.
    let mut changes: Vec<Change> = (0..files)
        .map(|i| put(&format!("f{i:05}.txt"), "init\n"))
        .collect();
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    let seed = Patch {
        author: "a@x".to_string(),
        revision: 1,
        base: Version::empty(),
        message: "seed".to_string(),
        changes,
    };
    let mut frontier = seed.result();
    repository.patches.push(seed);

    // Each subsequent patch touches one file, cycling through the file set.
    for rev in 2..=patches {
        let idx = ((rev - 2) as usize) % files;
        let patch = Patch {
            author: "a@x".to_string(),
            revision: rev,
            base: frontier.clone(),
            message: format!("r{rev}"),
            changes: vec![put(&format!("f{idx:05}.txt"), &format!("r{rev}\n"))],
        };
        frontier = patch.result();
        repository.patches.push(patch);
    }
    repository.sort_patches();
    repository.frontier = frontier;
    repository
}

/// Two branches of `per_branch` patches editing the same file with overlapping
/// regions. Forces diff + OT transform + patch application on every merge.
fn text_ot(per_branch: u64) -> Repository {
    let mut repository = Repository::default();
    // Base: 100 lines.
    let base_text: String = (0..100).fold(String::new(), |mut s, i| {
        let _ = writeln!(s, "line {i}");
        s
    });
    let root = Patch {
        author: "root@x".to_string(),
        revision: 1,
        base: Version::empty(),
        message: "base".to_string(),
        changes: vec![put("doc.txt", &base_text)],
    };
    let root_version = root.result();
    repository.patches.push(root);

    // Alice edits lines 40..50, Bob edits lines 45..55 — overlapping region.
    for (author, start_line) in [("alice@x", 40u64), ("bob@x", 45u64)] {
        let mut base = root_version.clone();
        for rev in 1..=per_branch {
            let lines: String = (0..100)
                .map(|i| {
                    if i >= start_line && i < start_line + 10 {
                        format!("{author} r{rev} line {i}\n")
                    } else {
                        format!("line {i}\n")
                    }
                })
                .collect();
            let patch = Patch {
                author: author.to_string(),
                revision: rev,
                base: base.clone(),
                message: format!("{author} r{rev}"),
                changes: vec![text_change("doc.txt", &base_text, &lines)],
            };
            base = patch.result();
            repository.patches.push(patch);
        }
    }
    repository.sort_patches();
    repository.frontier = repository
        .patches
        .iter()
        .fold(Version::empty(), |acc, p| acc.join(&p.result()));
    repository
}

/// Linear chain of `count` patches, each rewriting one file. Stresses depth
/// and memoization scaling.
fn deep_linear(count: u64) -> Repository {
    let mut repository = Repository::default();
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
        repository.patches.push(patch);
    }
    repository.sort_patches();
    repository.frontier = frontier;
    repository
}

fn measure(name: &str, detail: &str, repository: &Repository) {
    // One untimed pass so the measurement is not dominated by first-touch
    // page faults and lazy allocation.
    let _ = replay::materialize(repository, &repository.frontier).expect("valid history");
    let started = Instant::now();
    let rounds = 5;
    let mut paths = 0;
    for _ in 0..rounds {
        let (tree, _) = replay::materialize(repository, &repository.frontier).expect("valid");
        paths = tree.len();
    }
    let each = started.elapsed() / rounds;
    println!("{name:<20} {detail:<34} {each:>10.2?}  ({paths} paths)");
}

fn main() {
    println!("{:<20} {:<34} {:>10}", "workload", "shape", "per replay");
    println!("{}", "-".repeat(74));

    let linear = linear_history(1_000);
    measure("linear", "1000 sequential patches, 1 file", &linear);

    let divergent = divergent_history(250);
    measure("divergent", "2 branches x 250 patches", &divergent);

    let wide = wide_tree(5_000);
    measure("wide-tree", "5000 files, 2 patches", &wide);

    let lt = large_tree(1_000, 1_000);
    measure("large-tree", "1000 patches x 1000 files", &lt);

    let ot = text_ot(250);
    measure("text-ot", "2 branches x 250 edits, same file", &ot);

    let deep_a = deep_linear(10_000);
    measure("deep-linear", "10000 patches, 1 file", &deep_a);

    let deep_b = deep_linear(100_000);
    measure("deep-linear", "100000 patches, 1 file", &deep_b);

    // Diff is the other hot path; SPEC §5 is O(n*m) by construction.
    let mut old_text = String::new();
    let mut new_text = String::new();
    for i in 0..400 {
        let _ = writeln!(old_text, "line {i}");
        let _ = writeln!(new_text, "line {}", i * 2 % 400);
    }
    let old_tokens = text::tokenize(&old_text);
    let new_tokens = text::tokenize(&new_text);
    let started = Instant::now();
    for _ in 0..20 {
        let _ = text::diff(&old_tokens, &new_tokens);
    }
    println!(
        "{:<20} {:<34} {:>10.2?}",
        "diff",
        "400 x 400 tokens",
        started.elapsed() / 20
    );
}
