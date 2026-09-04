//! Plain-mode rendering of a diff body (SPEC §7.6).
//!
//! Kept out of `cli` so that command code computes values and never formats
//! them. `present` applies the terminal styling of SPEC §7.11 on top of the
//! plain bytes this produces.

use crate::model::{Content, EditOp, Tree};
use crate::text;
use std::fmt::Write as _;

/// Render the plain unified diff of SPEC §7.6.
pub fn unified_diff(old: &Tree, new: &Tree) -> String {
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
                EditOp::Retain(n) => {
                    for token in &old_tokens[cursor..cursor + *n as usize] {
                        push_diff_line(&mut out, ' ', token);
                    }
                    cursor += *n as usize;
                }
                EditOp::Delete(n) => {
                    for token in &old_tokens[cursor..cursor + *n as usize] {
                        push_diff_line(&mut out, '-', token);
                    }
                    cursor += *n as usize;
                }
                EditOp::Insert(tokens) => {
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
