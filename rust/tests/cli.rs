//! End-to-end command behaviour, driven in-process through `snap::cli::run`.
//!
//! These complement the language-neutral YAML suite rather than duplicating
//! it: the YAML suite proves conformance from outside, while these exercise
//! paths that are awkward to reach from a shell and keep failures debuggable.

mod support;

use support::{ok, run, Sandbox};

#[test]
fn init_creates_an_empty_repository() {
    let sandbox = Sandbox::new();
    let env = sandbox.env(".");
    assert_eq!(ok(&env, &["init", "repo"]), "()\n");
    assert_eq!(
        sandbox.read("repo/.snap/repository.json"),
        "{\n  \"format\": 1,\n  \"frontier\": [],\n  \"patches\": []\n}\n"
    );
}

#[test]
fn init_refuses_to_nest_or_reinitialize() {
    let sandbox = Sandbox::new();
    let env = sandbox.env(".");
    ok(&env, &["init", "repo"]);
    assert!(run(&env, &["init", "repo"])
        .stderr
        .contains("repository already exists"));

    let inner = sandbox.env("repo");
    assert!(run(&inner, &["init", "nested"])
        .stderr
        .contains("cannot initialize inside repository"));
}

#[test]
fn a_full_commit_status_log_cycle() {
    let sandbox = Sandbox::new();
    let env = sandbox.env("repo");
    ok(&sandbox.env("."), &["init", "repo"]);
    ok(&env, &["config", "contributor.id", "alice@example.com"]);

    assert_eq!(ok(&env, &["status"]), "version ()\n");

    sandbox.write("repo/hello.txt", "hello\n");
    assert_eq!(ok(&env, &["status"]), "version ()\nA hello.txt\n");
    assert_eq!(
        ok(&env, &["commit", "add greeting"]),
        "(alice@example.com->1)\n"
    );
    assert_eq!(ok(&env, &["status"]), "version (alice@example.com->1)\n");
    assert_eq!(
        ok(&env, &["log"]),
        "(alice@example.com->1)\talice@example.com\tadd greeting\n"
    );

    sandbox.write("repo/hello.txt", "hello\nworld\n");
    assert_eq!(
        ok(&env, &["status"]),
        "version (alice@example.com->1)\nM hello.txt\n"
    );
    std::fs::remove_file(sandbox.path("repo/hello.txt")).unwrap();
    assert_eq!(
        ok(&env, &["status"]),
        "version (alice@example.com->1)\nD hello.txt\n"
    );
}

#[test]
fn commit_refuses_a_clean_tree() {
    let sandbox = Sandbox::new();
    let env = sandbox.env("repo");
    ok(&sandbox.env("."), &["init", "repo"]);
    ok(&env, &["config", "contributor.id", "a@x"]);
    let out = run(&env, &["commit", "nothing"]);
    assert_eq!(out.code, 1);
    assert_eq!(out.stderr, "snap: working tree is clean\n");
}

#[test]
fn commit_requires_an_identity() {
    let sandbox = Sandbox::new();
    let env = sandbox.env("repo");
    ok(&sandbox.env("."), &["init", "repo"]);
    sandbox.write("repo/f.txt", "x\n");
    let out = run(&env, &["commit", "no identity"]);
    assert_eq!(out.code, 1);
    assert_eq!(
        out.stderr,
        "snap: contributor.id is required; configure it locally or globally\n"
    );
}

#[test]
fn local_configuration_beats_global() {
    let sandbox = Sandbox::new();
    let env = sandbox.env("repo");
    ok(&sandbox.env("."), &["init", "repo"]);
    ok(
        &sandbox.env("."),
        &["config", "--global", "contributor.id", "global@x"],
    );
    ok(&env, &["config", "contributor.id", "local@x"]);
    sandbox.write("repo/f.txt", "x\n");
    assert_eq!(ok(&env, &["commit", "which identity"]), "(local@x->1)\n");
}

#[test]
fn global_configuration_is_used_when_no_local_exists() {
    let sandbox = Sandbox::new();
    let env = sandbox.env("repo");
    ok(&sandbox.env("."), &["init", "repo"]);
    ok(
        &sandbox.env("."),
        &["config", "--global", "contributor.id", "global@x"],
    );
    sandbox.write("repo/f.txt", "x\n");
    assert_eq!(ok(&env, &["commit", "global identity"]), "(global@x->1)\n");
}

#[test]
fn revert_is_additive_and_prints_the_new_version() {
    let sandbox = Sandbox::new();
    let env = sandbox.env("repo");
    ok(&sandbox.env("."), &["init", "repo"]);
    ok(&env, &["config", "contributor.id", "a@x"]);
    sandbox.write("repo/f.txt", "one\n");
    ok(&env, &["commit", "first"]);
    sandbox.write("repo/f.txt", "two\n");
    ok(&env, &["commit", "second"]);

    // Reverting moves the frontier *forward* (SPEC §7.7).
    assert_eq!(ok(&env, &["revert", "(a@x->1)"]), "(a@x->3)\n");
    assert_eq!(sandbox.read("repo/f.txt"), "one\n");
    assert_eq!(
        ok(&env, &["log"]).lines().count(),
        3,
        "history keeps every patch"
    );
}

#[test]
fn revert_to_the_current_tree_is_an_error() {
    let sandbox = Sandbox::new();
    let env = sandbox.env("repo");
    ok(&sandbox.env("."), &["init", "repo"]);
    ok(&env, &["config", "contributor.id", "a@x"]);
    sandbox.write("repo/f.txt", "one\n");
    ok(&env, &["commit", "first"]);
    let out = run(&env, &["revert", "(a@x->1)"]);
    assert_eq!(out.code, 1);
    assert_eq!(out.stderr, "snap: target tree is already current\n");
}

#[test]
fn merge_converges_and_is_idempotent() {
    let sandbox = Sandbox::new();
    let root = sandbox.env(".");
    ok(&root, &["init", "seed"]);
    let seed = sandbox.env("seed");
    ok(&seed, &["config", "contributor.id", "seed@x"]);
    sandbox.write("seed/f.txt", "a\nb\n");
    ok(&seed, &["commit", "seed"]);

    copy_tree(&sandbox.path("seed"), &sandbox.path("left"));
    copy_tree(&sandbox.path("seed"), &sandbox.path("right"));

    let left = sandbox.env("left");
    let right = sandbox.env("right");
    ok(&left, &["config", "contributor.id", "left@x"]);
    ok(&right, &["config", "contributor.id", "right@x"]);
    sandbox.write("left/f.txt", "A\nb\n");
    sandbox.write("right/f.txt", "a\nB\n");
    ok(&left, &["commit", "left"]);
    ok(&right, &["commit", "right"]);

    let merged = ok(&left, &["merge", "../right"]);
    assert_eq!(merged, "(left@x->1,right@x->1,seed@x->1)\n");
    assert_eq!(
        sandbox.read("left/f.txt"),
        "A\nB\n",
        "both concurrent edits survive"
    );

    // SPEC §6.5: re-merging the same history is a no-op.
    let again = ok(&left, &["merge", "../right"]);
    assert_eq!(again, merged);
    assert_eq!(sandbox.read("left/f.txt"), "A\nB\n");
}

#[test]
fn merge_direction_does_not_change_the_result() {
    let sandbox = Sandbox::new();
    let root = sandbox.env(".");
    ok(&root, &["init", "seed"]);
    let seed = sandbox.env("seed");
    ok(&seed, &["config", "contributor.id", "seed@x"]);
    sandbox.write("seed/f.txt", "a\nb\nc\n");
    ok(&seed, &["commit", "seed"]);
    copy_tree(&sandbox.path("seed"), &sandbox.path("left"));
    copy_tree(&sandbox.path("seed"), &sandbox.path("right"));

    let left = sandbox.env("left");
    let right = sandbox.env("right");
    ok(&left, &["config", "contributor.id", "left@x"]);
    ok(&right, &["config", "contributor.id", "right@x"]);
    sandbox.write("left/f.txt", "A\nb\nc\n");
    sandbox.write("right/f.txt", "a\nb\nC\n");
    ok(&left, &["commit", "left"]);
    ok(&right, &["commit", "right"]);

    let from_left = ok(&left, &["merge", "../right"]);
    let from_right = ok(&right, &["merge", "../left"]);
    assert_eq!(
        from_left, from_right,
        "joined version is direction-independent"
    );
    assert_eq!(
        sandbox.read("left/f.txt"),
        sandbox.read("right/f.txt"),
        "SPEC §6.5: merge direction cannot change the joined result"
    );
    assert_eq!(
        sandbox.read("left/.snap/repository.json"),
        sandbox.read("right/.snap/repository.json"),
        "converged repositories must serialize to identical bytes"
    );
}

#[test]
fn merge_refuses_a_dirty_tree_without_importing_anything() {
    let sandbox = Sandbox::new();
    let root = sandbox.env(".");
    ok(&root, &["init", "seed"]);
    let seed = sandbox.env("seed");
    ok(&seed, &["config", "contributor.id", "seed@x"]);
    sandbox.write("seed/f.txt", "a\n");
    ok(&seed, &["commit", "seed"]);
    copy_tree(&sandbox.path("seed"), &sandbox.path("other"));
    let other = sandbox.env("other");
    ok(&other, &["config", "contributor.id", "other@x"]);
    sandbox.write("other/g.txt", "b\n");
    ok(&other, &["commit", "other"]);

    sandbox.write("seed/dirty.txt", "uncommitted\n");
    let before = sandbox.read("seed/.snap/repository.json");
    let out = run(&seed, &["merge", "../other"]);
    assert_eq!(out.code, 1);
    assert_eq!(out.stderr, "snap: working tree is dirty\n");
    assert_eq!(
        sandbox.read("seed/.snap/repository.json"),
        before,
        "no mutation on refusal"
    );
}

#[test]
fn diff_renders_a_unified_block_and_marks_a_missing_newline() {
    let sandbox = Sandbox::new();
    let env = sandbox.env("repo");
    ok(&sandbox.env("."), &["init", "repo"]);
    ok(&env, &["config", "contributor.id", "a@x"]);
    sandbox.write("repo/f.txt", "context\nold\n");
    ok(&env, &["commit", "first"]);
    sandbox.write("repo/f.txt", "context\nnew");
    assert_eq!(
        ok(&env, &["diff"]),
        "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,2 @@\n context\n-old\n+new\n\\ No newline at end of file\n"
    );
}

#[test]
fn binary_content_round_trips_and_diffs_as_binary() {
    let sandbox = Sandbox::new();
    let env = sandbox.env("repo");
    ok(&sandbox.env("."), &["init", "repo"]);
    ok(&env, &["config", "contributor.id", "a@x"]);
    let bytes: Vec<u8> = (0..=255u8).collect();
    std::fs::write(sandbox.path("repo/blob.bin"), &bytes).unwrap();
    ok(&env, &["commit", "binary"]);
    assert_eq!(std::fs::read(sandbox.path("repo/blob.bin")).unwrap(), bytes);

    std::fs::write(sandbox.path("repo/blob.bin"), [0u8, 1, 2]).unwrap();
    assert_eq!(
        ok(&env, &["diff"]),
        "Binary files a/blob.bin and b/blob.bin differ\n"
    );
}

#[test]
fn a_symlink_is_reported_rather_than_followed() {
    let sandbox = Sandbox::new();
    let env = sandbox.env("repo");
    ok(&sandbox.env("."), &["init", "repo"]);
    sandbox.write("repo/real.txt", "x\n");
    #[cfg(unix)]
    std::os::unix::fs::symlink("real.txt", sandbox.path("repo/link")).unwrap();
    let out = run(&env, &["status"]);
    assert_eq!(out.code, 1);
    assert_eq!(out.stderr, "snap: unsupported working tree entry: link\n");
}

#[test]
fn unknown_commands_and_extra_operands_are_rejected() {
    let sandbox = Sandbox::new();
    let env = sandbox.env(".");
    for args in [
        &["bogus"][..],
        &["--version", "extra"],
        &["init", "a", "b"],
        &["status", "extra"],
        &["log", "--unknown"],
        &["commit"],
        &["merge"],
    ] {
        let out = run(&env, args);
        assert_eq!(out.code, 1, "{args:?} should fail");
        assert_eq!(
            out.stderr, "snap: invalid command or arguments\n",
            "{args:?}"
        );
    }
}

#[test]
fn init_does_not_create_a_directory_named_like_an_option() {
    let sandbox = Sandbox::new();
    let env = sandbox.env(".");
    let out = run(&env, &["init", "--unknown"]);
    assert_eq!(out.code, 1);
    assert!(!support::exists(&sandbox.path("--unknown")));
}

#[test]
fn an_invalid_snap_color_fails_before_the_command_runs() {
    let sandbox = Sandbox::new();
    let mut env = sandbox.env(".");
    env.snap_color = Some("sometimes".to_string());
    let out = run(&env, &["init", "repo"]);
    assert_eq!(out.code, 1);
    assert_eq!(
        out.stderr,
        "snap: SNAP_COLOR must be auto, always, or never\n"
    );
    assert!(
        !support::exists(&sandbox.path("repo")),
        "no repository was created"
    );
}

#[test]
fn commands_outside_a_repository_say_so() {
    let sandbox = Sandbox::new();
    let env = sandbox.env(".");
    for args in [&["status"][..], &["log"], &["commit", "m"], &["diff"]] {
        let out = run(&env, args);
        assert_eq!(out.code, 1, "{args:?}");
        assert_eq!(out.stderr, "snap: not a Snap repository\n", "{args:?}");
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
