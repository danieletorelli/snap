//! Shared scaffolding for integration tests.
//!
//! Every integration binary compiles this module, so items used by only some
//! of them would otherwise trip `dead_code`.
#![allow(dead_code)]

use snap::cli::Env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temporary directory removed on drop. Hand-rolled rather than pulled in as
/// a dependency: the whole need is one unique path and a recursive delete.
pub struct Sandbox {
    pub root: PathBuf,
}

impl Sandbox {
    pub fn new() -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("snap-it-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).expect("create sandbox");
        Self { root }
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    pub fn write(&self, relative: &str, contents: &str) {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, contents).expect("write file");
    }

    pub fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.path(relative)).expect("read file")
    }

    pub fn env(&self, cwd: &str) -> Env {
        Env {
            cwd: self.path(cwd),
            home: Some(self.path("home")),
            snap_color: None,
            // Match the acceptance harness, which sets NO_COLOR in its base
            // environment, so integration output is plain by default.
            no_color: true,
            stdout_tty: false,
            stderr_tty: false,
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The result of one in-process command.
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run one command in-process. No spawning, so failures surface as ordinary
/// test output and the whole library stays reachable from a debugger.
pub fn run(env: &Env, args: &[&str]) -> Output {
    let owned: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = snap::cli::run(&owned, env, &mut stdout, &mut stderr);
    Output {
        code,
        stdout: String::from_utf8(stdout).expect("UTF-8 stdout"),
        stderr: String::from_utf8(stderr).expect("UTF-8 stderr"),
    }
}

/// Run a command that must succeed, returning its stdout.
pub fn ok(env: &Env, args: &[&str]) -> String {
    let out = run(env, args);
    assert_eq!(
        out.code,
        0,
        "`snap {}` failed: {}",
        args.join(" "),
        out.stderr
    );
    out.stdout
}

pub fn exists(path: &Path) -> bool {
    path.exists()
}
