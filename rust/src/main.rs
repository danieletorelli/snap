//! Thin binary shell: arguments in, exit code out (SPEC §10).
//!
//! All logic lives in the library so it stays reachable from unit tests,
//! integration tests, and benchmarks. This file owns exactly three things the
//! library deliberately does not: reading the real environment, probing TTYs,
//! and turning a panic into exit status 2.

use snap::cli::{self, Env};
use std::io::IsTerminal;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let env = Env {
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        home: std::env::var_os("HOME").map(PathBuf::from),
        snap_color: std::env::var("SNAP_COLOR").ok(),
        // SPEC §7.11: presence counts, including an empty value, so this must
        // test for the variable rather than for a truthy value.
        no_color: std::env::var_os("NO_COLOR").is_some(),
        stdout_tty: std::io::stdout().is_terminal(),
        stderr_tty: std::io::stderr().is_terminal(),
    };

    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();

    // SPEC §10: expected errors exit 1; an unexpected internal failure exits 2.
    let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cli::run(&args, &env, &mut stdout, &mut stderr)
    }))
    .unwrap_or(2);
    std::process::exit(code);
}
