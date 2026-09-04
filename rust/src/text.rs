//! Text detection, tokenization, and the canonical diff (SPEC §4.4, §5).

use crate::error::{self, Result};
use crate::model::{EditOp, EditScript};

/// SPEC §4.4: a file is text when its bytes are valid UTF-8 and contain no NUL.
#[must_use]
pub fn is_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

/// Split immediately after every LF, retaining the LF in its token
/// (SPEC §4.4). The empty file has no tokens.
#[must_use]
pub fn tokenize(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        if byte == b'\n' {
            tokens.push(&text[start..=i]);
            start = i + 1;
        }
    }
    if start < text.len() {
        tokens.push(&text[start..]);
    }
    tokens
}

/// SPEC §4.4: a canonical token sequence has LF at the end of every token
/// except possibly the last, and no LF anywhere else.
pub fn check_canonical_tokens(tokens: &[String]) -> Result<()> {
    for (i, token) in tokens.iter().enumerate() {
        if token.is_empty() {
            return Err(error::invalid_json("token must be nonempty"));
        }
        let interior_lf = token[..token.len() - 1].contains('\n');
        let ends_with_lf = token.ends_with('\n');
        let is_last = i + 1 == tokens.len();
        if interior_lf || (!ends_with_lf && !is_last) {
            return Err(error::invalid_json("non-canonical token sequence"));
        }
    }
    Ok(())
}

/// Apply an edit script to a token sequence (SPEC §4.4).
///
/// The script MUST consume the complete old sequence — there is no implicit
/// trailing retain — and the result MUST be canonical.
pub fn apply(script: &EditScript, old: &[&str]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    for op in script.ops() {
        match op {
            EditOp::Retain(n) => {
                let end = cursor + *n as usize;
                let slice = old
                    .get(cursor..end)
                    .ok_or_else(error::edit_does_not_consume)?;
                out.extend(slice.iter().map(|s| (*s).to_string()));
                cursor = end;
            }
            EditOp::Delete(n) => {
                let end = cursor + *n as usize;
                if end > old.len() {
                    return Err(error::edit_consumes_beyond());
                }
                cursor = end;
            }
            EditOp::Insert(tokens) => out.extend(tokens.iter().cloned()),
        }
    }
    if cursor != old.len() {
        return Err(error::edit_does_not_consume());
    }
    check_canonical_tokens(&out)?;
    Ok(out)
}

/// The canonical token diff of SPEC §5.
///
/// `D(i, j)` is the minimum number of inserts and deletes transforming
/// `old[i..]` into `new[j..]`. The walk from `(0, 0)` retains on equal tokens,
/// and otherwise prefers `delete` when `D(i+1, j) <= D(i, j+1)`. That `<=` is
/// load-bearing: it is what makes repeated lines diff identically everywhere.
///
/// Cost is `O(n * m)` time and memory. SPEC §5 permits Myers or Hirschberg
/// "only if it produces the same script", which is unverified for this exact
/// tie rule, so the literal recurrence is what ships. Only the common prefix
/// is trimmed first — see `trim_common_prefix` for why the suffix is not.
#[must_use]
pub fn diff(old: &[&str], new: &[&str]) -> EditScript {
    let shared = trim_common_prefix(old, new);
    let (a, b) = (&old[shared..], &new[shared..]);
    let (n, m) = (a.len(), b.len());

    // d[i * (m + 1) + j] = D(i, j), filled from the bottom-right corner.
    let mut d = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in 0..n {
        d[at(i, m)] = (n - i) as u32;
    }
    for j in 0..m {
        d[at(n, j)] = (m - j) as u32;
    }
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            d[at(i, j)] = if a[i] == b[j] {
                d[at(i + 1, j + 1)]
            } else {
                1 + d[at(i + 1, j)].min(d[at(i, j + 1)])
            };
        }
    }

    let mut ops: Vec<EditOp> = Vec::new();
    if shared > 0 {
        ops.push(EditOp::Retain(shared as u64));
    }
    let (mut i, mut j) = (0usize, 0usize);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            push_retain(&mut ops, 1);
            i += 1;
            j += 1;
        } else if j == m || (i < n && d[at(i + 1, j)] <= d[at(i, j + 1)]) {
            push_delete(&mut ops, 1);
            i += 1;
        } else {
            push_insert(&mut ops, b[j].to_string());
            j += 1;
        }
    }
    EditScript::new(ops).expect("diff produces a well-formed script")
}

/// Number of leading tokens shared by both sides.
///
/// Trimming the common *prefix* is safe: the walk starts at `(0, 0)` and
/// SPEC §5 rule 1 retains unconditionally on equal tokens, so those retains
/// are forced regardless of the `D` values.
///
/// Trimming the common *suffix* is **not** safe — not even applied after the
/// prefix trim, which is the form an optimizer would actually reach for. An
/// exhaustive sweep of all 132,496 sequence pairs up to length 5 over a
/// 3-symbol alphabet found 17,232 disagreements with the literal recurrence.
///
/// The smallest is `old = [a]`, `new = [b, a, a]`: the literal walk gives
/// `insert [b], retain 1, insert [a]`, while trimming the shared trailing `a`
/// first leaves `[] -> [b, a]` and yields `insert [b, a], retain 1`. Same
/// input, different script. See the regression test.
fn trim_common_prefix(old: &[&str], new: &[&str]) -> usize {
    old.iter().zip(new).take_while(|(a, b)| a == b).count()
}

fn push_retain(ops: &mut Vec<EditOp>, count: u64) {
    match ops.last_mut() {
        Some(EditOp::Retain(n)) => *n += count,
        _ => ops.push(EditOp::Retain(count)),
    }
}

fn push_delete(ops: &mut Vec<EditOp>, count: u64) {
    match ops.last_mut() {
        Some(EditOp::Delete(n)) => *n += count,
        _ => ops.push(EditOp::Delete(count)),
    }
}

fn push_insert(ops: &mut Vec<EditOp>, token: String) {
    match ops.last_mut() {
        Some(EditOp::Insert(tokens)) => tokens.push(token),
        _ => ops.push(EditOp::Insert(vec![token])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SPEC §5 recurrence with no trimming at all: the oracle that the
    /// shipped `diff` is differentially tested against.
    fn diff_reference(a: &[&str], b: &[&str]) -> Vec<EditOp> {
        let (n, m) = (a.len(), b.len());
        let mut d = vec![0u32; (n + 1) * (m + 1)];
        let at = |i: usize, j: usize| i * (m + 1) + j;
        for i in 0..n {
            d[at(i, m)] = (n - i) as u32;
        }
        for j in 0..m {
            d[at(n, j)] = (m - j) as u32;
        }
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                d[at(i, j)] = if a[i] == b[j] {
                    d[at(i + 1, j + 1)]
                } else {
                    1 + d[at(i + 1, j)].min(d[at(i, j + 1)])
                };
            }
        }
        let mut ops: Vec<EditOp> = Vec::new();
        let (mut i, mut j) = (0usize, 0usize);
        while i < n || j < m {
            if i < n && j < m && a[i] == b[j] {
                super::push_retain(&mut ops, 1);
                i += 1;
                j += 1;
            } else if j == m || (i < n && d[at(i + 1, j)] <= d[at(i, j + 1)]) {
                super::push_delete(&mut ops, 1);
                i += 1;
            } else {
                super::push_insert(&mut ops, b[j].to_string());
                j += 1;
            }
        }
        ops
    }

    fn toks(text: &str) -> Vec<&str> {
        tokenize(text)
    }

    // -- SPEC §4.4 text detection and tokenization -------------------------

    #[test]
    fn text_requires_valid_utf8_without_nul() {
        assert!(is_text(b"hello\n"));
        assert!(is_text(b""));
        assert!(is_text("h\u{e9}\n".as_bytes()));
        assert!(!is_text(b"has\0nul"));
        assert!(!is_text(&[0xff, 0xfe]));
    }

    #[test]
    fn tokenization_splits_after_lf_and_keeps_it() {
        assert_eq!(toks(""), Vec::<&str>::new(), "empty file has no tokens");
        assert_eq!(toks("a\n"), ["a\n"]);
        assert_eq!(toks("a"), ["a"], "no final newline");
        assert_eq!(toks("a\nb"), ["a\n", "b"]);
        assert_eq!(toks("a\nb\n"), ["a\n", "b\n"]);
        assert_eq!(toks("\n"), ["\n"], "lone newline");
        assert_eq!(toks("\n\n"), ["\n", "\n"]);
        // SPEC §4.4's own example: CR belongs to the token, LF ends it.
        assert_eq!(toks("a\r\nb"), ["a\r\n", "b"]);
    }

    #[test]
    fn canonical_token_check_matches_tokenizer_output() {
        for text in ["", "a\n", "a", "a\nb", "a\r\nb\n"] {
            let owned: Vec<String> = toks(text).into_iter().map(String::from).collect();
            assert!(check_canonical_tokens(&owned).is_ok(), "{text:?}");
        }
        assert!(
            check_canonical_tokens(&["a\nb\n".to_string()]).is_err(),
            "interior LF"
        );
        assert!(
            check_canonical_tokens(&["a".to_string(), "b".to_string()]).is_err(),
            "non-final token without LF"
        );
        assert!(
            check_canonical_tokens(&[String::new()]).is_err(),
            "empty token"
        );
    }

    // -- SPEC §5 canonical diff -------------------------------------------

    #[test]
    fn diff_then_apply_round_trips() {
        let cases = [
            ("", ""),
            ("", "a\n"),
            ("a\n", ""),
            ("a\n", "a\n"),
            ("a\nb\n", "a\nc\n"),
            ("a\nb\nc\n", "c\nb\na\n"),
            ("x\n", "a\nb\nc\n"),
            ("a\nb\nc\n", "x\n"),
            ("no newline", "still none"),
        ];
        for (old, new) in cases {
            let (a, b) = (toks(old), toks(new));
            let script = diff(&a, &b);
            let result = apply(&script, &a).expect("script applies");
            assert_eq!(
                result, b,
                "diff({old:?}, {new:?}) must reproduce the new side"
            );
        }
    }

    #[test]
    fn empty_to_empty_produces_an_empty_script() {
        assert!(diff(&[], &[]).is_empty());
    }

    #[test]
    fn suffix_trimming_would_be_wrong() {
        // Guards `trim_common_prefix`'s doc comment. `[a] -> [b, a, a]` has no
        // common prefix and a one-token common suffix, so it isolates suffix
        // trimming applied *after* the prefix trim — the variant that actually
        // tempts, and the smallest case where it diverges.
        let script = diff(&["a\n"], &["b\n", "a\n", "a\n"]);
        assert_eq!(
            script.ops(),
            &[
                EditOp::Insert(vec!["b\n".to_string()]),
                EditOp::Retain(1),
                EditOp::Insert(vec!["a\n".to_string()]),
            ],
            "the literal SPEC §5 walk splits the insertion around the retain"
        );
        // What suffix trimming would have produced instead.
        assert_ne!(
            script.ops(),
            &[
                EditOp::Insert(vec!["b\n".to_string(), "a\n".to_string()]),
                EditOp::Retain(1),
            ]
        );

        // The original prefix-order case still holds.
        let script = diff(&["a\n", "a\n"], &["a\n"]);
        assert_eq!(script.ops(), &[EditOp::Retain(1), EditOp::Delete(1)]);
    }

    #[test]
    fn deletion_wins_ties() {
        // SPEC §5 rule 2: choose delete when D(i+1,j) <= D(i,j+1). With one
        // token replaced by another the two costs are equal, so delete leads.
        let script = diff(&["a\n"], &["b\n"]);
        assert_eq!(
            script.ops(),
            &[EditOp::Delete(1), EditOp::Insert(vec!["b\n".to_string()])]
        );
    }

    #[test]
    fn repeated_lines_diff_deterministically() {
        let old = toks("x\nx\nx\n");
        let new = toks("x\nx\n");
        let script = diff(&old, &new);
        assert_eq!(apply(&script, &old).unwrap(), new);
        assert_eq!(script.ops(), &[EditOp::Retain(2), EditOp::Delete(1)]);
    }

    #[test]
    fn adjacent_operations_are_coalesced() {
        let script = diff(&toks("a\nb\nc\n"), &toks("x\ny\n"));
        for pair in script.ops().windows(2) {
            let same = matches!(
                (&pair[0], &pair[1]),
                (EditOp::Retain(_), EditOp::Retain(_))
                    | (EditOp::Delete(_), EditOp::Delete(_))
                    | (EditOp::Insert(_), EditOp::Insert(_))
            );
            assert!(
                !same,
                "adjacent same-kind operations must coalesce: {:?}",
                script.ops()
            );
        }
    }

    #[test]
    fn script_always_consumes_the_whole_old_sequence() {
        // SPEC §4.4: "there is no implicit trailing retain".
        for (old, new) in [
            ("a\nb\nc\n", "a\n"),
            ("a\n", "a\nb\nc\n"),
            ("a\nb\n", "b\na\n"),
        ] {
            let (a, b) = (toks(old), toks(new));
            assert_eq!(
                diff(&a, &b).consumed() as usize,
                a.len(),
                "{old:?} -> {new:?}"
            );
        }
    }

    #[test]
    fn prefix_trimming_matches_the_untrimmed_recurrence() {
        // Exhaustive differential test over a small alphabet: the shipped
        // implementation must agree with the literal SPEC §5 recurrence on
        // every pair of sequences up to length 4.
        let alphabet = ["a\n", "b\n", "c\n"];
        let mut sequences: Vec<Vec<&str>> = vec![vec![]];
        for _ in 0..4 {
            let mut next = Vec::new();
            for seq in &sequences {
                for token in alphabet {
                    let mut extended = seq.clone();
                    extended.push(token);
                    next.push(extended);
                }
            }
            sequences.extend(next);
        }
        let mut checked = 0usize;
        for a in &sequences {
            for b in &sequences {
                assert_eq!(
                    diff(a, b).ops(),
                    diff_reference(a, b).as_slice(),
                    "diff({a:?}, {b:?}) diverged from the reference recurrence"
                );
                checked += 1;
            }
        }
        assert!(checked > 14_000, "expected a broad sweep, ran {checked}");
    }

    // -- SPEC §4.4 application --------------------------------------------

    #[test]
    fn apply_rejects_scripts_that_do_not_consume_the_old_side() {
        let short = EditScript::new(vec![EditOp::Retain(1)]).unwrap();
        assert!(
            apply(&short, &["a\n", "b\n"]).is_err(),
            "leaves a token unconsumed"
        );

        let long = EditScript::new(vec![EditOp::Retain(3)]).unwrap();
        assert!(apply(&long, &["a\n"]).is_err(), "consumes past the end");
    }

    #[test]
    fn apply_rejects_scripts_producing_non_canonical_output() {
        // Inserting a token with an interior LF cannot be a valid result.
        let script = EditScript::new(vec![EditOp::Insert(vec!["a\nb\n".to_string()])]).unwrap();
        assert!(apply(&script, &[]).is_err());
    }

    #[test]
    fn empty_script_creates_an_empty_file() {
        // SPEC §4.4: "An empty script is valid only when creating an empty
        // text file."
        let script = EditScript::default();
        assert_eq!(apply(&script, &[]).unwrap(), Vec::<String>::new());
    }
}
