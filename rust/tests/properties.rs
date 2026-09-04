//! Property tests over randomly generated causal histories.
//!
//! SPEC §11: "Property tests SHOULD generate valid causal patch graphs and
//! verify that import permutations produce the same joined frontier, patch
//! set, warnings, and tree."
//!
//! Randomness is a small LCG seeded deterministically, so a failure is
//! reproducible from the seed printed in the assertion message rather than
//! being a flake.

mod support;

use snap::model::Repository;
use snap::replay;
use snap::version::Version;
use std::fmt::Write as _;
use support::{ok, run, Sandbox};

/// Deterministic linear congruential generator. A dependency-free source of
/// reproducible pseudo-randomness; the constants are Numerical Recipes'.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0 >> 16
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Make a few random edits in `replica` and commit them.
fn random_commit(sandbox: &Sandbox, replica: &str, rng: &mut Rng, round: usize) {
    let files = ["alpha.txt", "beta.txt", "nested/gamma.txt"];
    let mut changed = false;
    for file in files {
        match rng.below(4) {
            0 => {
                // Rewrite with a few lines drawn from a small alphabet, so
                // concurrent edits genuinely overlap.
                let mut lines = String::new();
                for i in 0..=rng.below(4) {
                    let letter = "abcde".as_bytes()[rng.below(5)] as char;
                    let _ = writeln!(lines, "{letter}-{i}");
                }
                sandbox.write(&format!("{replica}/{file}"), &lines);
                changed = true;
            }
            1 => {
                let path = sandbox.path(&format!("{replica}/{file}"));
                if path.exists() {
                    std::fs::remove_file(path).unwrap();
                    changed = true;
                }
            }
            _ => {}
        }
    }
    if !changed {
        sandbox.write(&format!("{replica}/alpha.txt"), &format!("round {round}\n"));
    }
    let env = sandbox.env(replica);
    let out = run(&env, &["commit", &format!("round {round}")]);
    // A commit can legitimately find nothing to do; anything else is a bug.
    assert!(
        out.code == 0 || out.stderr == "snap: working tree is clean\n",
        "unexpected commit failure: {}",
        out.stderr
    );
}

/// Read a repository straight off disk, bypassing the CLI.
fn repository_at(sandbox: &Sandbox, replica: &str) -> Repository {
    let text = sandbox.read(&format!("{replica}/.snap/repository.json"));
    Repository::from_json_str(&text).expect("valid repository")
}

#[test]
fn import_permutations_converge() {
    // Three replicas diverge, then are merged in every association order. All
    // orders must agree on the frontier, the patch set, the warnings, and the
    // bytes (SPEC §6.5).
    for seed in 0..12u64 {
        let mut rng = Rng(seed.wrapping_mul(2_654_435_761).wrapping_add(1));
        let sandbox = Sandbox::new();
        let root = sandbox.env(".");
        ok(&root, &["init", "seed"]);
        let seed_env = sandbox.env("seed");
        ok(&seed_env, &["config", "contributor.id", "seed@x"]);
        sandbox.write("seed/alpha.txt", "a\nb\nc\n");
        ok(&seed_env, &["commit", "base"]);

        let replicas = ["r0", "r1", "r2"];
        for (index, replica) in replicas.iter().enumerate() {
            copy_tree(&sandbox.path("seed"), &sandbox.path(replica));
            let env = sandbox.env(replica);
            ok(&env, &["config", "contributor.id", &format!("c{index}@x")]);
            for round in 0..2 + rng.below(2) {
                random_commit(&sandbox, replica, &mut rng, round);
            }
        }

        // Two association orders: (r0 <- r1) <- r2, and r0 <- (r1 <- r2).
        copy_tree(&sandbox.path("r0"), &sandbox.path("left"));
        copy_tree(&sandbox.path("r1"), &sandbox.path("mid"));
        copy_tree(&sandbox.path("r0"), &sandbox.path("right"));

        let left = sandbox.env("left");
        ok(&left, &["merge", "../r1"]);
        ok(&left, &["merge", "../r2"]);

        let mid = sandbox.env("mid");
        ok(&mid, &["merge", "../r2"]);
        let right = sandbox.env("right");
        ok(&right, &["merge", "../mid"]);

        let a = repository_at(&sandbox, "left");
        let b = repository_at(&sandbox, "right");
        assert_eq!(
            a.frontier.to_string(),
            b.frontier.to_string(),
            "seed {seed}: frontier"
        );
        assert_eq!(a.patches, b.patches, "seed {seed}: patch set");

        let (tree_a, warn_a) = replay::materialize(&a, &a.frontier).unwrap();
        let (tree_b, warn_b) = replay::materialize(&b, &b.frontier).unwrap();
        assert_eq!(tree_a, tree_b, "seed {seed}: materialized tree");
        assert_eq!(warn_a, warn_b, "seed {seed}: warning set");
        assert_eq!(
            a.to_canonical_string(),
            b.to_canonical_string(),
            "seed {seed}: converged repositories must serialize identically"
        );
    }
}

#[test]
fn merging_is_idempotent() {
    for seed in 0..8u64 {
        let mut rng = Rng(seed.wrapping_mul(6_364_136_223).wrapping_add(7));
        let sandbox = Sandbox::new();
        ok(&sandbox.env("."), &["init", "seed"]);
        let seed_env = sandbox.env("seed");
        ok(&seed_env, &["config", "contributor.id", "seed@x"]);
        sandbox.write("seed/alpha.txt", "a\nb\n");
        ok(&seed_env, &["commit", "base"]);
        copy_tree(&sandbox.path("seed"), &sandbox.path("other"));

        let other = sandbox.env("other");
        ok(&other, &["config", "contributor.id", "other@x"]);
        for round in 0..=rng.below(3) {
            random_commit(&sandbox, "other", &mut rng, round);
        }

        let once = ok(&seed_env, &["merge", "../other"]);
        let snapshot = sandbox.read("seed/.snap/repository.json");
        let twice = ok(&seed_env, &["merge", "../other"]);
        assert_eq!(
            once, twice,
            "seed {seed}: re-merge must print the same version"
        );
        assert_eq!(
            snapshot,
            sandbox.read("seed/.snap/repository.json"),
            "seed {seed}: SPEC §6.5 makes re-merging a no-op"
        );
    }
}

#[test]
fn materializing_a_version_ignores_unrelated_patches() {
    // The claim the base-tree memoization rests on: a version's tree depends
    // only on its own causal closure, so a canonical prefix snapshot taken
    // during a larger replay is the same tree a standalone replay would build.
    for seed in 0..10u64 {
        let mut rng = Rng(seed.wrapping_mul(2_246_822_519).wrapping_add(13));
        let sandbox = Sandbox::new();
        ok(&sandbox.env("."), &["init", "seed"]);
        let seed_env = sandbox.env("seed");
        ok(&seed_env, &["config", "contributor.id", "seed@x"]);
        sandbox.write("seed/alpha.txt", "a\nb\n");
        ok(&seed_env, &["commit", "base"]);
        copy_tree(&sandbox.path("seed"), &sandbox.path("other"));

        let other = sandbox.env("other");
        ok(&other, &["config", "contributor.id", "other@x"]);
        for round in 0..=rng.below(3) {
            random_commit(&sandbox, "other", &mut rng, round);
        }
        for round in 0..=rng.below(3) {
            random_commit(&sandbox, "seed", &mut rng, round);
        }

        let small = repository_at(&sandbox, "seed");
        let small_frontier = small.frontier.clone();
        let small_tree = replay::materialize(&small, &small_frontier).unwrap().0;

        ok(&seed_env, &["merge", "../other"]);
        let large = repository_at(&sandbox, "seed");

        // The pre-merge frontier is still known in the merged repository, and
        // must materialize to exactly the same bytes.
        let large_tree = replay::materialize(&large, &small_frontier).unwrap().0;
        assert_eq!(
            small_tree, large_tree,
            "seed {seed}: version {small_frontier} must not depend on unrelated patches"
        );
    }
}

#[test]
fn version_join_is_a_semilattice_over_random_versions() {
    const IDS: [&str; 4] = ["a@x", "b@x", "c@x", "~@x"];

    // A free function rather than a closure: two closures cannot both hold a
    // unique borrow of the generator.
    fn random_version(rng: &mut Rng) -> Version {
        let mut pairs: Vec<(String, u64)> = Vec::new();
        for id in IDS {
            if rng.below(2) == 0 {
                pairs.push((id.to_string(), 1 + rng.below(5) as u64));
            }
        }
        Version::from_pairs(pairs).expect("valid version")
    }

    let mut rng = Rng(0x5eed);
    for _ in 0..400 {
        let a = random_version(&mut rng);
        let b = random_version(&mut rng);
        let c = random_version(&mut rng);
        assert_eq!(a.join(&a), a, "idempotent");
        assert_eq!(a.join(&b), b.join(&a), "commutative");
        assert_eq!(a.join(&b).join(&c), a.join(&b.join(&c)), "associative");
        assert!(a.is_before_or_equal(&a.join(&b)), "join is an upper bound");
        assert!(b.is_before_or_equal(&a.join(&b)), "join is an upper bound");
        // Snap order is total and antisymmetric over the same sample.
        assert_eq!(a.snap_cmp(&b), b.snap_cmp(&a).reverse());
    }
}
