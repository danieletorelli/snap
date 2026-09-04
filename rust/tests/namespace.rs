//! Namespace-conflict replay (SPEC §6.2).
//!
//! The property tests in `properties.rs` only ever write regular files at three
//! fixed paths, so they never produce a file/directory collision — the most
//! intricate branch in `integrate`. These cases exercise it directly, and each
//! one checks three things: the result is prefix-free per SPEC §2, it matches
//! the naive spec-literal replay, and the warning set matches too.
use snap::model::{Change, ChangeKind, Content, Patch, Repository, Tree};
use snap::version::Version;
use snap::{replay, text};

fn content(b: &str) -> Content {
    Content::from(b.as_bytes().to_vec())
}
fn put(p: &str, b: &str) -> Change {
    Change {
        path: p.into(),
        kind: ChangeKind::Put(content(b)),
    }
}
fn del(p: &str) -> Change {
    Change {
        path: p.into(),
        kind: ChangeKind::Delete,
    }
}
fn txt(p: &str, from: &str, to: &str) -> Change {
    let (a, b) = (text::tokenize(from), text::tokenize(to));
    Change {
        path: p.into(),
        kind: ChangeKind::Text(text::diff(&a, &b)),
    }
}
fn patch(a: &str, r: u64, base: &str, mut ch: Vec<Change>) -> Patch {
    ch.sort_by(|x, y| x.path.cmp(&y.path));
    Patch {
        author: a.into(),
        revision: r,
        base: Version::parse(base).unwrap(),
        message: "m".into(),
        changes: ch,
    }
}
fn repo(patches: Vec<Patch>) -> Repository {
    let frontier = patches
        .iter()
        .fold(Version::empty(), |acc, p| acc.join(&p.result()));
    let mut r = Repository { frontier, patches };
    r.sort_patches();
    r
}
/// Is every path in the tree free of an ancestor also in the tree? (SPEC §2)
fn prefix_free(t: &Tree) -> bool {
    for p in t.keys() {
        for (i, b) in p.bytes().enumerate() {
            if b == b'/' && t.contains_key(&p[..i]) {
                return false;
            }
        }
    }
    true
}

#[test]
fn concurrent_file_and_directory_at_same_name() {
    // seed: nothing. alice creates file "a". bob concurrently creates "a/b".
    let seed = patch("seed@x", 1, "()", vec![put("keep.txt", "k\n")]);
    let s = "(seed@x->1)";
    let alice = patch("alice@x", 1, s, vec![put("a", "file\n")]);
    let bob = patch("bob@x", 1, s, vec![put("a/b", "nested\n")]);
    let r = repo(vec![seed, alice, bob]);
    let (tree, warns) = replay::materialize(&r, &r.frontier).expect("replays");
    assert!(
        prefix_free(&tree),
        "SPEC §2 violated: {:?}",
        tree.keys().collect::<Vec<_>>()
    );
    let (naive, nw) = replay::naive_materialize(&r, &r.frontier).unwrap();
    assert_eq!(tree, naive, "optimized != naive");
    assert_eq!(warns, nw, "warnings differ");
}

#[test]
fn deep_namespace_conflict() {
    // "a/b/c" exists; concurrently someone makes "a" a file.
    let seed = patch("seed@x", 1, "()", vec![put("a/b/c", "deep\n")]);
    let s = "(seed@x->1)";
    let alice = patch("alice@x", 1, s, vec![del("a/b/c"), put("a", "file\n")]);
    let bob = patch("bob@x", 1, s, vec![put("a/b/d", "sibling\n")]);
    let r = repo(vec![seed, alice, bob]);
    let (tree, warns) = replay::materialize(&r, &r.frontier).expect("replays");
    assert!(
        prefix_free(&tree),
        "SPEC §2 violated: {:?}",
        tree.keys().collect::<Vec<_>>()
    );
    let (naive, nw) = replay::naive_materialize(&r, &r.frontier).unwrap();
    assert_eq!(tree, naive);
    assert_eq!(warns, nw);
}

#[test]
fn three_way_namespace_pileup() {
    let seed = patch("seed@x", 1, "()", vec![put("z.txt", "z\n")]);
    let s = "(seed@x->1)";
    let a = patch("a@x", 1, s, vec![put("n", "file\n")]);
    let b = patch("b@x", 1, s, vec![put("n/x", "x\n")]);
    let c = patch("c@x", 1, s, vec![put("n/y", "y\n")]);
    let r = repo(vec![seed, a, b, c]);
    let (tree, warns) = replay::materialize(&r, &r.frontier).expect("replays");
    assert!(
        prefix_free(&tree),
        "SPEC §2 violated: {:?}",
        tree.keys().collect::<Vec<_>>()
    );
    let (naive, nw) = replay::naive_materialize(&r, &r.frontier).unwrap();
    assert_eq!(tree, naive);
    assert_eq!(warns, nw);
}

#[test]
fn text_edit_versus_namespace_takeover() {
    let seed = patch("seed@x", 1, "()", vec![put("a/b", "one\n")]);
    let s = "(seed@x->1)";
    let a = patch("a@x", 1, s, vec![txt("a/b", "one\n", "two\n")]);
    let b = patch("b@x", 1, s, vec![del("a/b"), put("a", "now a file\n")]);
    let r = repo(vec![seed, a, b]);
    let (tree, warns) = replay::materialize(&r, &r.frontier).expect("replays");
    assert!(
        prefix_free(&tree),
        "SPEC §2 violated: {:?}",
        tree.keys().collect::<Vec<_>>()
    );
    let (naive, nw) = replay::naive_materialize(&r, &r.frontier).unwrap();
    assert_eq!(tree, naive);
    assert_eq!(warns, nw);
}
