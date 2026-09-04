//! Plain and terminal presentation (SPEC §7.11).
//!
//! Two presentations over one set of values. Selecting a presentation MUST NOT
//! change execution, repository or filesystem effects, warning selection or
//! order, or exit status — so commands here only ever receive a finished value
//! and render it.
//!
//! TTY-ness arrives as a parameter rather than being probed inside this
//! module. SPEC §11 requires unit tests for `auto` selection on stdout and
//! stderr independently, and the acceptance harness pipes both streams, so
//! injection is the only way that requirement can be met.

use crate::error::{self, Result};
use crate::replay::Reason;
use crate::version::Version;
use std::fmt::Write as _;

/// SGR codes used by SPEC §7.11.
mod sgr {
    pub const BOLD: u8 = 1;
    pub const DIM: u8 = 2;
    pub const RED: u8 = 31;
    pub const GREEN: u8 = 32;
    pub const YELLOW: u8 = 33;
    pub const MAGENTA: u8 = 35;
    pub const CYAN: u8 = 36;
}

/// SPEC §7.11's `S(n, text)`: `ESC[`, the decimal code, `m`, text, `ESC[0m`.
#[must_use]
pub fn s(code: u8, text: &str) -> String {
    format!("\u{1b}[{code}m{text}\u{1b}[0m")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Plain,
    Terminal,
}

/// The presentation selected for each stream. They are independent: SPEC §7.11
/// says `auto` enables terminal mode "independently on stdout or stderr when
/// that stream is a TTY".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Presentation {
    pub stdout: Mode,
    pub stderr: Mode,
}

impl Presentation {
    #[must_use]
    pub const fn plain() -> Self {
        Self {
            stdout: Mode::Plain,
            stderr: Mode::Plain,
        }
    }
}

/// Resolve presentation from the environment and per-stream TTY-ness.
///
/// SPEC §7.11's table, plus its conservative `NO_COLOR` rule: the presence of
/// `NO_COLOR`, *including an empty value*, selects the complete plain
/// presentation in `auto` mode rather than merely suppressing color.
/// `SNAP_COLOR=always` is the explicit override and beats `NO_COLOR`.
pub fn resolve(
    snap_color: Option<&str>,
    no_color: bool,
    stdout_tty: bool,
    stderr_tty: bool,
) -> Result<Presentation> {
    let mode = |tty: bool| if tty { Mode::Terminal } else { Mode::Plain };
    match snap_color {
        None | Some("auto") => {
            if no_color {
                Ok(Presentation::plain())
            } else {
                Ok(Presentation {
                    stdout: mode(stdout_tty),
                    stderr: mode(stderr_tty),
                })
            }
        }
        Some("always") => Ok(Presentation {
            stdout: Mode::Terminal,
            stderr: Mode::Terminal,
        }),
        Some("never") => Ok(Presentation::plain()),
        // Rejected before command execution, and reported plainly because no
        // valid presentation was selected.
        Some(_) => Err(error::bad_color_mode()),
    }
}

/// The label attached to a successful mutating command (SPEC §7.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Success {
    Initialized,
    Committed,
    Reverted,
    Merged,
}

impl Success {
    fn label(self) -> &'static str {
        match self {
            Success::Initialized => "Initialized repository",
            Success::Committed => "Committed",
            Success::Reverted => "Reverted",
            Success::Merged => "Merged",
        }
    }
}

/// `init`, `commit`, `revert`, `merge` all print a version on success.
#[must_use]
pub fn success(mode: Mode, kind: Success, version: &Version) -> String {
    match mode {
        Mode::Plain => format!("{version}\n"),
        Mode::Terminal => format!(
            "{} {} {}\n",
            s(sgr::GREEN, "✓"),
            s(sgr::BOLD, kind.label()),
            s(sgr::CYAN, &version.to_string())
        ),
    }
}

/// One working-tree change (SPEC §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StatusCode {
    Added,
    Deleted,
    Modified,
}

impl StatusCode {
    #[must_use]
    pub fn letter(self) -> char {
        match self {
            StatusCode::Added => 'A',
            StatusCode::Deleted => 'D',
            StatusCode::Modified => 'M',
        }
    }

    /// `(color, symbol, label)` for terminal mode (SPEC §7.11).
    fn decoration(self) -> (u8, &'static str, &'static str) {
        match self {
            StatusCode::Added => (sgr::GREEN, "+", "added"),
            // U+2212 MINUS SIGN, not an ASCII hyphen.
            StatusCode::Deleted => (sgr::RED, "−", "deleted"),
            StatusCode::Modified => (sgr::YELLOW, "~", "modified"),
        }
    }
}

#[must_use]
pub fn status(mode: Mode, version: &Version, rows: &[(String, StatusCode)]) -> String {
    let mut out = String::new();
    match mode {
        Mode::Plain => {
            let _ = writeln!(out, "version {version}");
            for (path, code) in rows {
                let _ = writeln!(out, "{} {path}", code.letter());
            }
        }
        Mode::Terminal => {
            let _ = write!(
                out,
                "{}  {}\n\n",
                s(sgr::BOLD, "Snap status"),
                s(sgr::CYAN, &version.to_string())
            );
            if rows.is_empty() {
                let _ = writeln!(out, "  {} Working tree clean", s(sgr::GREEN, "✓"));
            }
            for (path, code) in rows {
                let (color, symbol, label) = code.decoration();
                let _ = writeln!(
                    out,
                    "  {} {path} {}",
                    s(color, symbol),
                    s(sgr::DIM, &format!("({label})"))
                );
            }
        }
    }
    out
}

/// One history entry, already ordered by the caller (SPEC §7.4).
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub version: Version,
    pub author: String,
    /// The escaped one-line message; see [`escape_message`].
    pub message: String,
}

/// SPEC §7.4: in messages, backslash, tab and LF are escaped as `\\`, `\t` and
/// `\n`, *in that order* — backslash first, or the escapes would double up.
#[must_use]
pub fn escape_message(message: &str) -> String {
    message
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

#[must_use]
pub fn log(mode: Mode, entries: &[LogEntry]) -> String {
    match mode {
        Mode::Plain => entries.iter().fold(String::new(), |mut out, e| {
            let _ = writeln!(out, "{}\t{}\t{}", e.version, e.author, e.message);
            out
        }),
        Mode::Terminal => entries
            .iter()
            .map(|e| {
                format!(
                    "{} {}\n  {} {} {}\n",
                    s(sgr::CYAN, "●"),
                    s(sgr::BOLD, &e.message),
                    s(sgr::CYAN, &e.version.to_string()),
                    s(sgr::DIM, "by"),
                    s(sgr::MAGENTA, &e.author)
                )
            })
            .collect::<Vec<_>>()
            // "Entries have one additional LF between them."
            .join("\n"),
    }
}

/// SPEC §7.11: diff keeps every plain byte, styling only the whole text of a
/// matching line by the *first* applicable rule.
#[must_use]
pub fn diff(mode: Mode, plain: &str) -> String {
    if mode == Mode::Plain {
        return plain.to_string();
    }
    let mut out = String::new();
    for line in plain.split_inclusive('\n') {
        let (body, newline) = line
            .strip_suffix('\n')
            .map_or((line, ""), |stripped| (stripped, "\n"));
        let code = if body.starts_with("--- ") || body.starts_with("+++ ") {
            Some(sgr::BOLD)
        } else if body.starts_with("@@ ") {
            Some(sgr::CYAN)
        } else if body.starts_with('-') {
            Some(sgr::RED)
        } else if body.starts_with('+') {
            Some(sgr::GREEN)
        } else if body.starts_with("\\ ") {
            Some(sgr::DIM)
        } else if body.starts_with("Binary files ") {
            Some(sgr::YELLOW)
        } else {
            None
        };
        match code {
            Some(code) => out.push_str(&s(code, body)),
            None => out.push_str(body),
        }
        out.push_str(newline);
    }
    out
}

#[must_use]
pub fn version_line(mode: Mode, semver: &str) -> String {
    let text = format!("snap {semver}");
    match mode {
        Mode::Plain => format!("{text}\n"),
        Mode::Terminal => format!("{}\n", s(sgr::BOLD, &text)),
    }
}

/// A merge warning (SPEC §6.4, §7.11).
#[must_use]
pub fn warning(mode: Mode, path: &str, reason: Reason) -> String {
    let detail = format!("auto-resolved {path}: {reason}");
    match mode {
        Mode::Plain => format!("warning: {detail}\n"),
        Mode::Terminal => format!("{} {}\n", s(sgr::YELLOW, "⚠"), s(sgr::YELLOW, &detail)),
    }
}

/// A one-line error (SPEC §10, §7.11).
#[must_use]
pub fn error_line(mode: Mode, detail: &str) -> String {
    match mode {
        Mode::Plain => format!("snap: {detail}\n"),
        Mode::Terminal => format!("{}\n", s(sgr::RED, &format!("✗ snap: {detail}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(raw: &str) -> Version {
        Version::parse(raw).unwrap()
    }

    // -- SPEC §7.11 selection. The acceptance harness pipes both streams and
    // sets NO_COLOR=1, so `auto` with a TTY can only be covered here (§11).

    #[test]
    fn auto_selects_each_stream_independently() {
        let p = resolve(None, false, true, false).unwrap();
        assert_eq!(p.stdout, Mode::Terminal);
        assert_eq!(p.stderr, Mode::Plain);

        let p = resolve(Some("auto"), false, false, true).unwrap();
        assert_eq!(p.stdout, Mode::Plain);
        assert_eq!(p.stderr, Mode::Terminal);

        let p = resolve(None, false, true, true).unwrap();
        assert_eq!((p.stdout, p.stderr), (Mode::Terminal, Mode::Terminal));

        let p = resolve(None, false, false, false).unwrap();
        assert_eq!((p.stdout, p.stderr), (Mode::Plain, Mode::Plain));
    }

    #[test]
    fn no_color_selects_the_complete_plain_presentation_in_auto() {
        // SPEC §7.11 treats NO_COLOR conservatively: presence alone, even
        // empty, means plain — not merely "color suppressed".
        let p = resolve(None, true, true, true).unwrap();
        assert_eq!((p.stdout, p.stderr), (Mode::Plain, Mode::Plain));
    }

    #[test]
    fn always_overrides_no_color_and_never_overrides_ttys() {
        let p = resolve(Some("always"), true, false, false).unwrap();
        assert_eq!((p.stdout, p.stderr), (Mode::Terminal, Mode::Terminal));

        let p = resolve(Some("never"), false, true, true).unwrap();
        assert_eq!((p.stdout, p.stderr), (Mode::Plain, Mode::Plain));
    }

    #[test]
    fn an_invalid_snap_color_is_an_error() {
        assert!(resolve(Some("yes"), false, false, false).is_err());
        assert!(resolve(Some(""), false, false, false).is_err());
        assert!(
            resolve(Some("AUTO"), false, false, false).is_err(),
            "case sensitive"
        );
    }

    // -- Rendering ---------------------------------------------------------

    #[test]
    fn success_lines_match_the_spec_layout() {
        assert_eq!(
            success(Mode::Plain, Success::Committed, &v("(a@x->1)")),
            "(a@x->1)\n"
        );
        assert_eq!(
            success(Mode::Terminal, Success::Initialized, &v("()")),
            "\u{1b}[32m✓\u{1b}[0m \u{1b}[1mInitialized repository\u{1b}[0m \u{1b}[36m()\u{1b}[0m\n"
        );
    }

    #[test]
    fn status_renders_both_presentations() {
        let rows = vec![("added.txt".to_string(), StatusCode::Added)];
        assert_eq!(
            status(Mode::Plain, &v("(a@x->1)"), &rows),
            "version (a@x->1)\nA added.txt\n"
        );
        assert_eq!(
            status(Mode::Terminal, &v("(a@x->1)"), &rows),
            "\u{1b}[1mSnap status\u{1b}[0m  \u{1b}[36m(a@x->1)\u{1b}[0m\n\n  \u{1b}[32m+\u{1b}[0m added.txt \u{1b}[2m(added)\u{1b}[0m\n"
        );
    }

    #[test]
    fn a_clean_tree_prints_only_the_version_in_plain_mode() {
        assert_eq!(status(Mode::Plain, &v("()"), &[]), "version ()\n");
        assert_eq!(
            status(Mode::Terminal, &v("()"), &[]),
            "\u{1b}[1mSnap status\u{1b}[0m  \u{1b}[36m()\u{1b}[0m\n\n  \u{1b}[32m✓\u{1b}[0m Working tree clean\n"
        );
    }

    #[test]
    fn deleted_uses_the_minus_sign_not_a_hyphen() {
        let rows = vec![("gone.txt".to_string(), StatusCode::Deleted)];
        let rendered = status(Mode::Terminal, &v("()"), &rows);
        assert!(rendered.contains('\u{2212}'), "SPEC §7.11 specifies U+2212");
        assert!(
            !rendered.contains("[31m-"),
            "an ASCII hyphen would be wrong"
        );
    }

    #[test]
    fn message_escaping_handles_backslash_first() {
        // SPEC §7.4 fixes the order: backslash, then tab, then LF. Escaping
        // tab first would turn a literal `\t` in the message into `\\t`.
        assert_eq!(escape_message("a\\tb"), "a\\\\tb");
        assert_eq!(escape_message("a\tb"), "a\\tb");
        assert_eq!(escape_message("a\nb"), "a\\nb");
        assert_eq!(escape_message("\\\t\n"), "\\\\\\t\\n");
    }

    #[test]
    fn log_separates_terminal_entries_with_one_extra_lf() {
        let entries = vec![
            LogEntry {
                version: v("(a@x->2)"),
                author: "a@x".into(),
                message: "second".into(),
            },
            LogEntry {
                version: v("(a@x->1)"),
                author: "a@x".into(),
                message: "first".into(),
            },
        ];
        assert_eq!(
            log(Mode::Plain, &entries),
            "(a@x->2)\ta@x\tsecond\n(a@x->1)\ta@x\tfirst\n"
        );
        let terminal = log(Mode::Terminal, &entries);
        assert!(
            terminal.contains("\u{1b}[0m\n\n\u{1b}[36m●"),
            "blank line between entries"
        );
        assert!(!terminal.ends_with("\n\n"), "no trailing blank line");
    }

    #[test]
    fn diff_styles_by_first_applicable_rule() {
        // `--- ` must win over the bare `-` rule, and `+++ ` over `+`.
        let plain = "--- a/x\n+++ b/x\n@@ -1,1 +1,1 @@\n keep\n-gone\n+new\n\\ No newline at end of file\nBinary files a/y and b/y differ\n";
        let styled = diff(Mode::Terminal, plain);
        assert!(
            styled.starts_with("\u{1b}[1m--- a/x\u{1b}[0m\n"),
            "header is bold, not red"
        );
        assert!(
            styled.contains("\u{1b}[1m+++ b/x\u{1b}[0m\n"),
            "header is bold, not green"
        );
        assert!(styled.contains("\u{1b}[36m@@ -1,1 +1,1 @@\u{1b}[0m\n"));
        assert!(styled.contains("\n keep\n"), "context lines are untouched");
        assert!(styled.contains("\u{1b}[31m-gone\u{1b}[0m\n"));
        assert!(styled.contains("\u{1b}[32m+new\u{1b}[0m\n"));
        assert!(styled.contains("\u{1b}[2m\\ No newline at end of file\u{1b}[0m\n"));
        assert!(styled.contains("\u{1b}[33mBinary files a/y and b/y differ\u{1b}[0m\n"));
    }

    #[test]
    fn plain_diff_is_passed_through_byte_for_byte() {
        let plain = "--- a/x\n+++ b/x\n@@ -1,1 +1,1 @@\n-a\n+b\n";
        assert_eq!(diff(Mode::Plain, plain), plain);
    }

    #[test]
    fn warnings_and_errors_render_per_spec() {
        assert_eq!(
            warning(Mode::Plain, "a/b", Reason::NamespaceWins),
            "warning: auto-resolved a/b: namespace-wins\n"
        );
        assert_eq!(
            warning(Mode::Terminal, "same", Reason::LaterCreateWins),
            "\u{1b}[33m⚠\u{1b}[0m \u{1b}[33mauto-resolved same: later-create-wins\u{1b}[0m\n"
        );
        assert_eq!(
            error_line(Mode::Plain, "invalid command or arguments"),
            "snap: invalid command or arguments\n"
        );
        assert_eq!(
            error_line(Mode::Terminal, "invalid command or arguments"),
            "\u{1b}[31m✗ snap: invalid command or arguments\u{1b}[0m\n"
        );
    }

    #[test]
    fn version_line_matches_the_golden() {
        assert_eq!(version_line(Mode::Plain, "1.0.0"), "snap 1.0.0\n");
        assert_eq!(
            version_line(Mode::Terminal, "1.0.0"),
            "\u{1b}[1msnap 1.0.0\u{1b}[0m\n"
        );
    }

    #[test]
    fn empty_plain_output_stays_empty() {
        assert_eq!(diff(Mode::Plain, ""), "");
        assert_eq!(log(Mode::Plain, &[]), "");
        assert_eq!(log(Mode::Terminal, &[]), "");
    }
}
