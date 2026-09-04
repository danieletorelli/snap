//! The three workloads PLAN.md §5 commits to measuring.
//!
//! Run with `cargo bench`. Deliberately a plain binary rather than criterion:
//! the point is a repeatable number to justify or reject an optimization, not
//! statistical rigour, and the project carries one runtime dependency by
//! design.
//!
//! * linear history  — 1,000 sequential patches; measures the SPEC §6.2 rule-1
//!   fast path and base-tree memoization on a canonical prefix.
//! * divergent history — two branches of 250 patches merged; measures OT and
//!   the non-prefix base-tree fallback.
//! * wide tree — 5,000 files, one small patch; measures scan, replay and
//!   materialization cost against tree size rather than history length.

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
