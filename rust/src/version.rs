//! Vector-clock versions (SPEC §3).
//!
//! A version is a map from contributor id to revision, holding only nonzero
//! entries, kept sorted by unsigned UTF-8 bytes of the id. Rust's `Ord` for
//! `str` is already byte-wise, so the canonical ordering needs no custom
//! comparator.

use crate::error::{self, Result};
use std::cmp::Ordering;
use std::fmt;

/// SPEC §3.1: revisions are positive and no greater than 2^53 - 1.
pub const MAX_REVISION: u64 = 9_007_199_254_740_991;

/// SPEC §3.1: contributor ids are at most 254 bytes.
pub const MAX_ID_BYTES: usize = 254;

/// Validate a contributor id per SPEC §3.1.
///
/// Exactly one `@` with nonempty text on both sides; no control character,
/// whitespace, `,`, `(`, `)`, or the substring `->`; at most 254 bytes; ASCII.
pub fn validate_contributor_id(id: &str) -> Result<()> {
    let bad = || error::invalid_contributor_id(id);
    if id.is_empty() || id.len() > MAX_ID_BYTES || !id.is_ascii() {
        return Err(bad());
    }
    if id
        .bytes()
        .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return Err(bad());
    }
    if id.bytes().any(|b| matches!(b, b',' | b'(' | b')')) {
        return Err(bad());
    }
    if id.contains("->") {
        return Err(bad());
    }
    let mut parts = id.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(bad());
    };
    if local.is_empty() || domain.is_empty() {
        return Err(bad());
    }
    Ok(())
}

/// A causal frontier: a finite map of contributor id to nonzero revision.
///
/// Invariant: `entries` is sorted strictly ascending by id bytes and every
/// revision is in `1..=MAX_REVISION`. All constructors uphold it, so ordered
/// iteration and canonical serialization are just a walk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Version {
    entries: Vec<(Box<str>, u64)>,
}

impl Version {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build from unordered pairs, rejecting zero revisions.
    ///
    /// Rejects duplicate ids, invalid ids, and out-of-range revisions.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, u64)>) -> Result<Self> {
        let mut entries: Vec<(Box<str>, u64)> = Vec::new();
        for (id, revision) in pairs {
            validate_contributor_id(&id)?;
            if revision == 0 {
                return Err(error::invalid_version(&id));
            }
            if revision > MAX_REVISION {
                return Err(error::invalid_version(&id));
            }
            entries.push((id.into_boxed_str(), revision));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        if entries.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(error::invalid_version("duplicate contributor"));
        }
        Ok(Self { entries })
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Revision for `id`; absent components are zero (SPEC §3.3).
    #[must_use]
    pub fn get(&self, id: &str) -> u64 {
        self.entries
            .binary_search_by(|probe| probe.0.as_ref().cmp(id))
            .map_or(0, |i| self.entries[i].1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, u64)> {
        self.entries.iter().map(|(id, n)| (id.as_ref(), *n))
    }

    /// Set a component, inserting or removing to preserve the invariant.
    pub fn set(&mut self, id: &str, revision: u64) -> Result<()> {
        validate_contributor_id(id)?;
        if revision > MAX_REVISION {
            return Err(error::invalid_version(id));
        }
        match self
            .entries
            .binary_search_by(|probe| probe.0.as_ref().cmp(id))
        {
            Ok(i) => {
                if revision == 0 {
                    self.entries.remove(i);
                } else {
                    self.entries[i].1 = revision;
                }
            }
            Err(i) => {
                if revision != 0 {
                    self.entries.insert(i, (id.into(), revision));
                }
            }
        }
        Ok(())
    }

    /// Componentwise maximum (SPEC §3.3).
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        let mut entries = Vec::with_capacity(self.entries.len().max(other.entries.len()));
        let (mut i, mut j) = (0, 0);
        while i < self.entries.len() && j < other.entries.len() {
            let (a, b) = (&self.entries[i], &other.entries[j]);
            match a.0.cmp(&b.0) {
                Ordering::Less => {
                    entries.push(a.clone());
                    i += 1;
                }
                Ordering::Greater => {
                    entries.push(b.clone());
                    j += 1;
                }
                Ordering::Equal => {
                    entries.push((a.0.clone(), a.1.max(b.1)));
                    i += 1;
                    j += 1;
                }
            }
        }
        entries.extend_from_slice(&self.entries[i..]);
        entries.extend_from_slice(&other.entries[j..]);
        Self { entries }
    }

    /// The four-way causal comparison of SPEC §3.3.
    ///
    /// Deliberately not `PartialOrd`: that collapses `Concurrent` into `None`
    /// and invites `!(a < b)` to be read as "after", which is wrong here.
    #[must_use]
    pub fn causal_cmp(&self, other: &Self) -> Causal {
        // Linear merge walk rather than per-id lookup: replay compares versions
        // inside its ready-set loop, so this is a hot path.
        let mut saw_less = false;
        let mut saw_greater = false;
        let (mut i, mut j) = (0, 0);
        while i < self.entries.len() && j < other.entries.len() {
            let (a, b) = (&self.entries[i], &other.entries[j]);
            match a.0.cmp(&b.0) {
                // Present on one side only: the other component is zero, and
                // stored revisions are always nonzero.
                Ordering::Less => {
                    saw_greater = true;
                    i += 1;
                }
                Ordering::Greater => {
                    saw_less = true;
                    j += 1;
                }
                Ordering::Equal => {
                    match a.1.cmp(&b.1) {
                        Ordering::Less => saw_less = true,
                        Ordering::Greater => saw_greater = true,
                        Ordering::Equal => {}
                    }
                    i += 1;
                    j += 1;
                }
            }
            if saw_less && saw_greater {
                return Causal::Concurrent;
            }
        }
        saw_greater |= i < self.entries.len();
        saw_less |= j < other.entries.len();
        match (saw_less, saw_greater) {
            (false, false) => Causal::Equal,
            (true, false) => Causal::Before,
            (false, true) => Causal::After,
            (true, true) => Causal::Concurrent,
        }
    }

    #[must_use]
    pub fn is_before_or_equal(&self, other: &Self) -> bool {
        matches!(self.causal_cmp(other), Causal::Before | Causal::Equal)
    }

    /// Snap order (SPEC §3.4): a total order extending causal order.
    ///
    /// Walk the sorted union of contributor ids and compare the counter at
    /// each; the first unequal counter decides. Its ordering of concurrent
    /// versions carries no chronological meaning.
    #[must_use]
    pub fn snap_cmp(&self, other: &Self) -> Ordering {
        let (mut i, mut j) = (0, 0);
        while i < self.entries.len() && j < other.entries.len() {
            let (a, b) = (&self.entries[i], &other.entries[j]);
            let ord = match a.0.cmp(&b.0) {
                // An id present on only one side has counter 0 on the other.
                Ordering::Less => {
                    i += 1;
                    a.1.cmp(&0)
                }
                Ordering::Greater => {
                    j += 1;
                    0.cmp(&b.1)
                }
                Ordering::Equal => {
                    i += 1;
                    j += 1;
                    a.1.cmp(&b.1)
                }
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        if i < self.entries.len() {
            return Ordering::Greater;
        }
        if j < other.entries.len() {
            return Ordering::Less;
        }
        Ordering::Equal
    }

    /// Parse the canonical CLI form of SPEC §3.2.
    ///
    /// Strict: rejects whitespace, explicit zeroes, leading zeroes, signs,
    /// duplicate ids, overflow, and noncanonical ordering.
    pub fn parse(raw: &str) -> Result<Self> {
        let bad = || error::invalid_version(raw);
        let body = raw
            .strip_prefix('(')
            .and_then(|r| r.strip_suffix(')'))
            .ok_or_else(bad)?;
        if body.is_empty() {
            return Ok(Self::empty());
        }
        let mut entries: Vec<(Box<str>, u64)> = Vec::new();
        for field in body.split(',') {
            let (id, revision) = field.split_once("->").ok_or_else(bad)?;
            // Ids cannot contain `->`, so a second occurrence is malformed.
            if revision.contains("->") {
                return Err(bad());
            }
            validate_contributor_id(id).map_err(|_| bad())?;
            entries.push((id.into(), parse_revision(revision).ok_or_else(bad)?));
        }
        if entries.windows(2).any(|w| w[0].0 >= w[1].0) {
            return Err(bad());
        }
        Ok(Self { entries })
    }
}

/// Strict positive-integer parse: digits only, no leading zero, within range.
fn parse_revision(raw: &str) -> Option<u64> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if raw.len() > 1 && raw.starts_with('0') {
        return None;
    }
    let value: u64 = raw.parse().ok()?;
    (1..=MAX_REVISION).contains(&value).then_some(value)
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("(")?;
        for (i, (id, revision)) in self.entries.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            write!(f, "{id}->{revision}")?;
        }
        f.write_str(")")
    }
}

/// The four outcomes of SPEC §3.3. `Concurrent` is a first-class result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Causal {
    Equal,
    Before,
    After,
    Concurrent,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(raw: &str) -> Version {
        Version::parse(raw).expect("valid version")
    }

    // -- SPEC §3.1 contributor ids ---------------------------------------

    #[test]
    fn accepts_email_shaped_ids() {
        for id in [
            "a@x",
            "alice@example.com",
            "a+laptop@example.com",
            "a@x.y.z",
        ] {
            assert!(validate_contributor_id(id).is_ok(), "{id} should be valid");
        }
    }

    #[test]
    fn rejects_ids_violating_spec_3_1() {
        for id in [
            "",         // empty
            "nope",     // no @
            "a@@x",     // two @
            "@x",       // empty local
            "a@",       // empty domain
            "a b@x",    // whitespace
            "a\t@x",    // whitespace
            "a\u{1}@x", // control
            "a,b@x",    // comma
            "a(b@x",    // paren
            "a)b@x",    // paren
            "a->b@x",   // arrow substring
            "\u{e9}@x", // non-ASCII
        ] {
            assert!(
                validate_contributor_id(id).is_err(),
                "{id:?} should be rejected"
            );
        }
    }

    #[test]
    fn enforces_254_byte_id_limit() {
        let local = "a".repeat(MAX_ID_BYTES - 2);
        let ok = format!("{local}@x");
        assert_eq!(ok.len(), MAX_ID_BYTES);
        assert!(validate_contributor_id(&ok).is_ok());
        assert!(validate_contributor_id(&format!("a{ok}")).is_err());
    }

    // -- SPEC §3.2 canonical syntax --------------------------------------

    #[test]
    fn parses_and_renders_the_empty_version() {
        assert!(v("()").is_empty());
        assert_eq!(v("()").to_string(), "()");
    }

    #[test]
    fn round_trips_canonical_forms() {
        for raw in [
            "()",
            "(a@x->1)",
            "(a@x->1,b@x->2)",
            "(jdegoes@example.com->2323,vigoo@example.com->239)",
        ] {
            assert_eq!(v(raw).to_string(), raw);
        }
    }

    #[test]
    fn rejects_noncanonical_syntax() {
        for raw in [
            "",                        // not delimited
            "(",                       // unterminated
            "a@x->1",                  // missing parens
            "(a@x->0)",                // explicit zero
            "(a@x->01)",               // leading zero
            "(a@x->+1)",               // sign
            "(a@x->-1)",               // sign
            "(a@x->1,a@x->2)",         // duplicate id
            "(b@x->1,a@x->2)",         // misordered
            "(a@x->1, b@x->2)",        // whitespace
            "( a@x->1)",               // whitespace
            "(a@x->1,)",               // trailing separator
            "(a@x)",                   // missing revision
            "(a@x->)",                 // empty revision
            "(a@x->9007199254740992)", // above MAX_REVISION
            "(a@x->1->2)",             // double arrow
        ] {
            assert!(Version::parse(raw).is_err(), "{raw:?} should be rejected");
        }
    }

    #[test]
    fn accepts_the_maximum_revision() {
        let raw = format!("(a@x->{MAX_REVISION})");
        assert_eq!(v(&raw).get("a@x"), MAX_REVISION);
    }

    #[test]
    fn orders_contributors_by_unsigned_byte_order() {
        // '~' (0x7E) sorts after 'a' (0x61); a naive locale sort might not.
        let parsed = Version::from_pairs([("~@x".into(), 1), ("a@x".into(), 2)]).unwrap();
        assert_eq!(parsed.to_string(), "(a@x->2,~@x->1)");
    }

    // -- SPEC §3.3 causal comparison and join ----------------------------

    #[test]
    fn preserves_all_four_comparison_outcomes() {
        assert_eq!(v("(a@x->1)").causal_cmp(&v("(a@x->1)")), Causal::Equal);
        assert_eq!(v("(a@x->1)").causal_cmp(&v("(a@x->2)")), Causal::Before);
        assert_eq!(v("(a@x->2)").causal_cmp(&v("(a@x->1)")), Causal::After);
        assert_eq!(v("(a@x->1)").causal_cmp(&v("(b@x->1)")), Causal::Concurrent);
        assert_eq!(v("()").causal_cmp(&v("(a@x->1)")), Causal::Before);
        assert_eq!(v("()").causal_cmp(&v("()")), Causal::Equal);
    }

    #[test]
    fn concurrency_is_not_the_negation_of_before() {
        // The trap `PartialOrd` sets: neither before nor after, yet not equal.
        let (a, b) = (v("(a@x->1,b@x->2)"), v("(a@x->2,b@x->1)"));
        assert_eq!(a.causal_cmp(&b), Causal::Concurrent);
        assert_eq!(b.causal_cmp(&a), Causal::Concurrent);
        assert!(!a.is_before_or_equal(&b));
        assert!(!b.is_before_or_equal(&a));
    }

    #[test]
    fn join_takes_the_componentwise_maximum() {
        let joined = v("(a@x->1,b@x->5)").join(&v("(b@x->2,c@x->7)"));
        assert_eq!(joined.to_string(), "(a@x->1,b@x->5,c@x->7)");
    }

    #[test]
    fn join_is_a_semilattice() {
        let vs = [v("()"), v("(a@x->1)"), v("(b@x->3)"), v("(a@x->2,c@x->9)")];
        for a in &vs {
            assert_eq!(a.join(a), *a, "idempotent");
            for b in &vs {
                assert_eq!(a.join(b), b.join(a), "commutative");
                for c in &vs {
                    assert_eq!(a.join(b).join(c), a.join(&b.join(c)), "associative");
                }
            }
        }
    }

    #[test]
    fn join_is_the_least_upper_bound() {
        let (a, b) = (v("(a@x->1,b@x->2)"), v("(a@x->2,b@x->1)"));
        let j = a.join(&b);
        assert!(a.is_before_or_equal(&j) && b.is_before_or_equal(&j));
    }

    // -- SPEC §3.4 Snap order --------------------------------------------

    #[test]
    fn snap_order_extends_causal_order() {
        let vs = [
            v("()"),
            v("(a@x->1)"),
            v("(a@x->2)"),
            v("(b@x->1)"),
            v("(a@x->1,b@x->1)"),
            v("(a@x->2,b@x->3)"),
        ];
        for a in &vs {
            for b in &vs {
                match a.causal_cmp(b) {
                    Causal::Before => assert_eq!(a.snap_cmp(b), Ordering::Less),
                    Causal::After => assert_eq!(a.snap_cmp(b), Ordering::Greater),
                    Causal::Equal => assert_eq!(a.snap_cmp(b), Ordering::Equal),
                    Causal::Concurrent => assert_ne!(a.snap_cmp(b), Ordering::Equal),
                }
            }
        }
    }

    #[test]
    fn snap_order_is_a_strict_total_order() {
        let vs = [
            v("()"),
            v("(a@x->1)"),
            v("(a@x->1,b@x->2)"),
            v("(a@x->2,b@x->1)"),
            v("(b@x->1)"),
            v("(~@x->1)"),
        ];
        for a in &vs {
            for b in &vs {
                assert_eq!(a.snap_cmp(b), b.snap_cmp(a).reverse(), "antisymmetric");
                for c in &vs {
                    if a.snap_cmp(b) == Ordering::Less && b.snap_cmp(c) == Ordering::Less {
                        assert_eq!(a.snap_cmp(c), Ordering::Less, "transitive");
                    }
                }
            }
        }
    }

    #[test]
    fn snap_order_compares_the_first_unequal_counter() {
        // Sorted union is a@x, b@x. Left has a@x->1, right a@x->2: a decides,
        // even though b@x->9 on the left is larger.
        assert_eq!(
            v("(a@x->1,b@x->9)").snap_cmp(&v("(a@x->2)")),
            Ordering::Less
        );
    }

    // -- Accessors --------------------------------------------------------

    #[test]
    fn absent_components_read_as_zero() {
        assert_eq!(v("(a@x->3)").get("b@x"), 0);
        assert_eq!(v("()").get("a@x"), 0);
    }

    #[test]
    fn set_maintains_the_sorted_nonzero_invariant() {
        let mut ver = Version::empty();
        ver.set("b@x", 2).unwrap();
        ver.set("a@x", 1).unwrap();
        assert_eq!(ver.to_string(), "(a@x->1,b@x->2)");
        ver.set("a@x", 0).unwrap(); // zero removes
        assert_eq!(ver.to_string(), "(b@x->2)");
        ver.set("c@x", 0).unwrap(); // absent stays absent
        assert_eq!(ver.to_string(), "(b@x->2)");
    }

    #[test]
    fn set_rejects_invalid_ids_and_oversized_revisions() {
        let mut ver = Version::empty();
        assert!(ver.set("bad", 1).is_err());
        assert!(ver.set("a@x", MAX_REVISION + 1).is_err());
        assert!(ver.set("a@x", 1).is_ok());
    }

    #[test]
    fn from_pairs_rejects_duplicates_and_out_of_range() {
        assert!(Version::from_pairs([("a@x".into(), 1), ("a@x".into(), 2)]).is_err());
        assert!(Version::from_pairs([("a@x".into(), 0)]).is_err());
        assert!(Version::from_pairs([("a@x".into(), MAX_REVISION + 1)]).is_err());
        assert!(Version::from_pairs([("bad".into(), 1)]).is_err());
    }
}
