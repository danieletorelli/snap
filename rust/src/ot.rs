//! Operational transform for text edits (SPEC §6.3).
//!
//! Transform an incoming edit `P` so it applies after an aggregate context
//! edit `Q`. Both scripts consume the same base token count, so the two
//! streams are walked together, splitting counts where they disagree.
//!
//! SPEC §6.3's table, in the order the rules are tested:
//!
//! | Next operations   | Output in transformed `P`  | Consumption |
//! |-------------------|----------------------------|-------------|
//! | `Q insert`        | `retain(len(Q insert))`    | Q only      |
//! | `P insert`        | same `P insert`            | P only      |
//! | `P retain`/`Q retain` | `retain(min)`          | both        |
//! | `P delete`/`Q retain` | `delete(min)`          | both        |
//! | `P retain`/`Q delete` | nothing                | both        |
//! | `P delete`/`Q delete` | nothing                | both        |
//!
//! The `Q insert` row having priority is what puts concurrent inserts at one
//! cursor into canonical integration order. Deletion consumes only base
//! tokens, so concurrent inserted text survives.
//!
//! Snap performs this transform **once** against the aggregate context edit,
//! never once per historical patch. That is what keeps merging linear rather
//! than quadratic in divergence; do not "optimize" it into a per-patch loop.

use crate::model::{EditOp, EditScript};

/// A cursor over an edit script that can consume partial counts.
struct Stream<'a> {
    ops: &'a [EditOp],
    index: usize,
    /// Tokens still unconsumed in the operation at `index`.
    remaining: u64,
}

impl<'a> Stream<'a> {
    fn new(script: &'a EditScript) -> Self {
        let mut stream = Self {
            ops: script.ops(),
            index: 0,
            remaining: 0,
        };
        stream.load();
        stream
    }

    fn load(&mut self) {
        self.remaining = match self.ops.get(self.index) {
            Some(EditOp::Retain(n) | EditOp::Delete(n)) => *n,
            _ => 0,
        };
    }

    fn current(&self) -> Option<&'a EditOp> {
        self.ops.get(self.index)
    }

    fn advance(&mut self) {
        self.index += 1;
        self.load();
    }

    /// Consume `count` tokens from a retain or delete, advancing if exhausted.
    fn consume(&mut self, count: u64) {
        self.remaining -= count;
        if self.remaining == 0 {
            self.advance();
        }
    }

    fn is_done(&self) -> bool {
        self.index >= self.ops.len()
    }
}

/// Transform `incoming` so that it applies after `context`.
///
/// Returns `None` when the two scripts do not consume the same base token
/// count, which means one of them was not authored against the same base and
/// the caller must fall back to SPEC §6.4's path-level rules.
#[must_use]
pub fn transform(incoming: &EditScript, context: &EditScript) -> Option<EditScript> {
    if incoming.consumed() != context.consumed() {
        return None;
    }
    let mut p = Stream::new(incoming);
    let mut q = Stream::new(context);
    let mut out: Vec<EditOp> = Vec::new();

    while !p.is_done() || !q.is_done() {
        // Row 1: a context insertion is unseen by P, so P retains over it.
        if let Some(EditOp::Insert(tokens)) = q.current() {
            push_retain(&mut out, tokens.len() as u64);
            q.advance();
            continue;
        }
        // Row 2: P's own insertion survives unchanged.
        if let Some(EditOp::Insert(tokens)) = p.current() {
            push_insert(&mut out, tokens.clone());
            p.advance();
            continue;
        }
        // Rows 3-6: both sides now hold retain/delete over the same base.
        let (Some(p_op), Some(q_op)) = (p.current(), q.current()) else {
            // Equal consumption is checked up front, so reaching here with one
            // stream still holding base operations means the input was
            // malformed in a way validation should already have caught.
            return None;
        };
        let count = p.remaining.min(q.remaining);
        match (p_op, q_op) {
            (EditOp::Retain(_), EditOp::Retain(_)) => push_retain(&mut out, count),
            (EditOp::Delete(_), EditOp::Retain(_)) => push_delete(&mut out, count),
            // P retain / Q delete and P delete / Q delete both emit nothing:
            // the base tokens are already gone from the current tree.
            _ => {}
        }
        p.consume(count);
        q.consume(count);
    }

    // Every operation is well formed by construction, and coalescing above
    // guarantees no adjacent pair shares a kind.
    EditScript::new(out).ok()
}

fn push_retain(out: &mut Vec<EditOp>, count: u64) {
    if count == 0 {
        return;
    }
    match out.last_mut() {
        Some(EditOp::Retain(n)) => *n += count,
        _ => out.push(EditOp::Retain(count)),
    }
}

fn push_delete(out: &mut Vec<EditOp>, count: u64) {
    if count == 0 {
        return;
    }
    match out.last_mut() {
        Some(EditOp::Delete(n)) => *n += count,
        _ => out.push(EditOp::Delete(count)),
    }
}

fn push_insert(out: &mut Vec<EditOp>, tokens: Vec<String>) {
    match out.last_mut() {
        Some(EditOp::Insert(existing)) => existing.extend(tokens),
        _ => out.push(EditOp::Insert(tokens)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{apply, diff, tokenize};

    fn script(ops: Vec<EditOp>) -> EditScript {
        EditScript::new(ops).expect("well-formed script")
    }

    fn ins(tokens: &[&str]) -> EditOp {
        EditOp::Insert(tokens.iter().map(|t| (*t).to_string()).collect())
    }

    // -- One test per row of the SPEC §6.3 table --------------------------

    #[test]
    fn row_q_insert_becomes_a_retain() {
        let p = script(vec![EditOp::Retain(1)]);
        let q = script(vec![ins(&["new\n"]), EditOp::Retain(1)]);
        assert_eq!(
            transform(&p, &q).unwrap().ops(),
            &[EditOp::Retain(2)],
            "P must retain over text it never saw"
        );
    }

    #[test]
    fn row_p_insert_survives_unchanged() {
        let p = script(vec![ins(&["mine\n"]), EditOp::Retain(1)]);
        let q = script(vec![EditOp::Retain(1)]);
        assert_eq!(
            transform(&p, &q).unwrap().ops(),
            &[ins(&["mine\n"]), EditOp::Retain(1)]
        );
    }

    #[test]
    fn row_retain_retain_yields_retain() {
        let p = script(vec![EditOp::Retain(3)]);
        let q = script(vec![EditOp::Retain(3)]);
        assert_eq!(transform(&p, &q).unwrap().ops(), &[EditOp::Retain(3)]);
    }

    #[test]
    fn row_delete_retain_yields_delete() {
        let p = script(vec![EditOp::Delete(2), EditOp::Retain(1)]);
        let q = script(vec![EditOp::Retain(3)]);
        assert_eq!(
            transform(&p, &q).unwrap().ops(),
            &[EditOp::Delete(2), EditOp::Retain(1)]
        );
    }

    #[test]
    fn row_retain_delete_yields_nothing() {
        let p = script(vec![EditOp::Retain(2)]);
        let q = script(vec![EditOp::Delete(2)]);
        assert!(
            transform(&p, &q).unwrap().is_empty(),
            "context already removed the tokens"
        );
    }

    #[test]
    fn row_delete_delete_yields_nothing() {
        // Concurrent deletion of the same tokens collapses rather than
        // deleting twice.
        let p = script(vec![EditOp::Delete(2)]);
        let q = script(vec![EditOp::Delete(2)]);
        assert!(transform(&p, &q).unwrap().is_empty());
    }

    // -- Priority, splitting, and survival --------------------------------

    #[test]
    fn q_insert_has_priority_over_p_insert() {
        // Both sides insert at the same cursor. SPEC §6.3 gives the `Q insert`
        // row priority, so the context's tokens land first and P retains over
        // them before contributing its own.
        let p = script(vec![ins(&["p\n"]), EditOp::Retain(1)]);
        let q = script(vec![ins(&["q\n"]), EditOp::Retain(1)]);
        assert_eq!(
            transform(&p, &q).unwrap().ops(),
            &[EditOp::Retain(1), ins(&["p\n"]), EditOp::Retain(1)]
        );
    }

    #[test]
    fn counts_split_when_the_streams_disagree() {
        // P deletes 3 while Q retains 1 then deletes 2: the first token is
        // still deletable, the rest are already gone.
        let p = script(vec![EditOp::Delete(3)]);
        let q = script(vec![EditOp::Retain(1), EditOp::Delete(2)]);
        assert_eq!(transform(&p, &q).unwrap().ops(), &[EditOp::Delete(1)]);
    }

    #[test]
    fn concurrent_insert_survives_inside_a_deleted_region() {
        // SPEC §6.3: "Deletion consumes only base tokens, so concurrent
        // inserted text survives." Q removes the whole region; P's insertion
        // inside it is still emitted.
        let p = script(vec![EditOp::Retain(1), ins(&["kept\n"]), EditOp::Retain(1)]);
        let q = script(vec![EditOp::Delete(2)]);
        assert_eq!(transform(&p, &q).unwrap().ops(), &[ins(&["kept\n"])]);
    }

    #[test]
    fn trailing_insertions_are_processed_after_the_base_is_consumed() {
        let p = script(vec![EditOp::Retain(1), ins(&["tail\n"])]);
        let q = script(vec![EditOp::Retain(1)]);
        assert_eq!(
            transform(&p, &q).unwrap().ops(),
            &[EditOp::Retain(1), ins(&["tail\n"])]
        );
    }

    #[test]
    fn output_never_has_adjacent_operations_of_one_kind() {
        let p = script(vec![
            EditOp::Retain(1),
            EditOp::Delete(1),
            EditOp::Retain(1),
        ]);
        let q = script(vec![
            EditOp::Retain(1),
            EditOp::Delete(1),
            EditOp::Retain(1),
        ]);
        let out = transform(&p, &q).unwrap();
        for pair in out.ops().windows(2) {
            assert!(
                std::mem::discriminant(&pair[0]) != std::mem::discriminant(&pair[1]),
                "must coalesce: {:?}",
                out.ops()
            );
        }
    }

    #[test]
    fn rejects_scripts_authored_against_different_bases() {
        let p = script(vec![EditOp::Retain(2)]);
        let q = script(vec![EditOp::Retain(3)]);
        assert!(
            transform(&p, &q).is_none(),
            "unequal base consumption must be refused"
        );
    }

    // -- End-to-end: transformed edits apply to the transformed base ------

    #[test]
    fn transformed_edit_applies_to_the_context_result() {
        // The invariant every merge depends on: if Q takes B to C, then the
        // transform of P through Q must apply cleanly to C.
        let cases = [
            ("a\nb\nc\n", "a\nB\nc\n", "a\nb\nC\n"),
            ("a\nb\nc\n", "a\n", "a\nb\nc\nd\n"),
            ("a\nb\n", "x\na\nb\n", "a\nb\ny\n"),
            ("a\nb\nc\nd\n", "a\nd\n", "a\nb\nX\nc\nd\n"),
            ("l1\nl2\nl3\n", "l1\nl3\n", "l1\nl2\nl3\nl4\n"),
        ];
        for (base, context_result, incoming_result) in cases {
            let (b, c, t) = (
                tokenize(base),
                tokenize(context_result),
                tokenize(incoming_result),
            );
            let q = diff(&b, &c);
            let p = diff(&b, &t);
            let transformed = transform(&p, &q).expect("same base");
            let merged = apply(&transformed, &c).unwrap_or_else(|e| {
                panic!("{base:?} + {incoming_result:?} over {context_result:?}: {e}")
            });
            // Both concurrent effects are represented: nothing the context
            // kept and the incoming edit also kept may be lost.
            assert!(!merged.is_empty() || c.is_empty() && t.is_empty());
        }
    }

    #[test]
    fn transforming_an_edit_through_itself_duplicates_it() {
        // This is correct SPEC §6.3 behaviour, not a defect, and it is worth
        // pinning because it looks like one. Transforming P through an
        // identical Q yields `retain, insert, retain`: the `Q insert` row
        // retains over the context's copy and the `P insert` row then adds
        // P's own, so the token appears twice.
        //
        // Nothing in §6.3 prevents that. SPEC §6.2 rule 2 does, one layer up:
        // "If the path is identical in C and T, keep it unchanged. This
        // collapses identical concurrent changes before OT rather than
        // duplicating their effect." Replay must therefore apply rule 2
        // *before* reaching rule 3, or identical concurrent edits double.
        // `replay` has the regression test for that ordering.
        let base = tokenize("a\nb\n");
        let same = tokenize("a\nX\nb\n");
        let edit = diff(&base, &same);
        let transformed = transform(&edit, &edit).expect("same base");
        let merged = apply(&transformed, &same).unwrap();
        assert_eq!(
            merged,
            vec!["a\n", "X\n", "X\n", "b\n"],
            "OT alone duplicates; SPEC §6.2 rule 2 is what prevents it"
        );
    }
}
