//! The section-move proof — audit item **A15**, spec §3.
//!
//! # Why this gate exists
//!
//! ARCHITECTURE §6.3 listed `section.rename` and `section.move*` among the
//! permitted `.do` writers. They had no command, no owner and no gate — while
//! `section.move` **reorders executable statements**. A rename is provably a
//! comment-only edit (the title lives in a `// %%` comment) and is gated by
//! [`crate::assert_comment_only`]. A move is not: it changes which statement
//! runs first, which is a semantic change *by design*. So it needs its own,
//! different proof.
//!
//! # What is proven
//!
//! [`assert_statement_partition_preserved`] returns `Ok` iff the edit
//! **permuted whole statements and did nothing else**:
//!
//! 1. the multiset of per-statement canonical token streams is identical;
//! 2. every statement's bytes are unchanged;
//! 3. the multiset of comment texts is unchanged.
//!
//! A15 states (1) and (2) and then glosses them as "the edit permuted whole
//! statements and did nothing else". (3) is that gloss taken literally: without
//! it, a "move" that silently dropped every comment in the section would pass a
//! gate whose entire purpose is to prove nothing but order changed. It can only
//! ever cause a **refusal**, never an acceptance, so it cannot weaken the
//! guarantee — and a section move that legitimately alters a comment is a
//! rename, which has its own gate.
//!
//! # What is deliberately NOT proven
//!
//! That the reordered file computes the same numbers. It does not, in general —
//! that is what moving a section means. `section_move` reports which blocks it
//! re-staled (`restaled: BlockId[]` in CONTRACTS §11's table) and the execution
//! ledger takes it from there. This gate's job is narrower and absolute: prove
//! that the *only* thing that happened was a permutation.

use core::fmt;

use stratum_parse::{DerivedText, LogicalLine, Segmentation};

/// A refusal from the section-move gate.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PartitionDrift {
    /// A different number of statements: something was added, removed, split or
    /// joined.
    StatementCount {
        /// Statements before.
        before: usize,
        /// Statements after.
        after: usize,
    },
    /// A statement that exists in one buffer and not the other. `text` is the
    /// statement that lost its counterpart.
    StatementChanged {
        /// The statement text, as it appears in the buffer named by `side`.
        text: String,
        /// `"before"` or `"after"`.
        side: &'static str,
    },
    /// The comment content changed. A move carries comments with their
    /// statements; anything else is an edit, not a move.
    CommentChanged {
        /// The comment that lost its counterpart.
        text: String,
        /// `"before"` or `"after"`.
        side: &'static str,
    },
}

impl fmt::Display for PartitionDrift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PartitionDrift::StatementCount { before, after } => write!(
                f,
                "the file went from {before} statement(s) to {after}; a move cannot add or remove one"
            ),
            PartitionDrift::StatementChanged { text, side } => write!(
                f,
                "this statement appears only in the {side} buffer: {text:?}"
            ),
            PartitionDrift::CommentChanged { text, side } => write!(
                f,
                "this comment appears only in the {side} buffer: {text:?}"
            ),
        }
    }
}

impl core::error::Error for PartitionDrift {}

/// Prove that `after` is a **reordering** of `before` and nothing more.
pub fn assert_statement_partition_preserved(
    before: &str,
    after: &str,
) -> Result<(), PartitionDrift> {
    let a = stratum_parse::segment(before);
    let b = stratum_parse::segment(after);

    let mut ka = statement_keys(&a);
    let mut kb = statement_keys(&b);
    if ka.len() != kb.len() {
        return Err(PartitionDrift::StatementCount {
            before: ka.len(),
            after: kb.len(),
        });
    }
    // Multiset equality by sorting. Sorting the KEYS, not the statements, is
    // what makes this a permutation test rather than an order test: two
    // identical statements in different places are indistinguishable, which is
    // correct — moving one past the other is a no-op.
    ka.sort();
    kb.sort();
    if let Some(pos) = ka.iter().zip(kb.iter()).position(|(x, y)| x != y) {
        // Report the first mismatch from whichever side has the statement the
        // other lacks. `ka` and `kb` are the same length and `pos` is inside
        // both, so both lookups succeed.
        let (side, key) = match (ka.get(pos), kb.get(pos)) {
            (Some(x), Some(y)) if x < y => ("before", x),
            (_, Some(y)) => ("after", y),
            (Some(x), None) => ("before", x),
            _ => return Ok(()),
        };
        return Err(PartitionDrift::StatementChanged {
            text: key.1.clone(),
            side,
        });
    }

    let mut ca = comment_texts(before, &a);
    let mut cb = comment_texts(after, &b);
    ca.sort();
    cb.sort();
    if ca != cb {
        let (side, text) = first_difference(&ca, &cb);
        return Err(PartitionDrift::CommentChanged { text, side });
    }
    Ok(())
}

/// Per-statement `(canonical token stream, exact bytes)`.
///
/// The canonical stream is the one `CodeHash` is built from (CONTRACTS §1.2),
/// which already normalises `///` continuations and carries a per-statement
/// delimiter discriminant. So moving a statement into or out of a `#delimit ;`
/// stretch changes its key, and the gate refuses, which is exactly right: the
/// same bytes in the other delimiter mode are a different program.
///
/// # Why the stream is held as its §1.2 rule-6 encoding and not as `Vec<CanonToken>`
///
/// The multiset test below is "sort both sides and compare", which needs a TOTAL
/// ORDER on the key. `CanonToken` has none, and CONTRACTS §1.2 pins its derive
/// list normatively, so growing one there would put proto out of step with the
/// contract to buy an ordering with no meaning in this domain (`Ident` before
/// `Number` says nothing about Stata). Rule 6's encoding is already an exact,
/// INJECTIVE serialisation of the stream — the length prefix is precisely what
/// stops `["ab", "c"]` and `["a", "bc"]` from colliding — so byte order over it
/// is a total order that separates exactly the streams that differ.
///
/// It is deliberately the pre-image and not `code_hash` of it. This is a safety
/// gate on a path that rewrites the user's `.do` file; comparing the bytes
/// themselves costs less than hashing them and keeps a collision assumption out
/// of a proof that does not need one.
fn statement_keys(seg: &Segmentation<'_>) -> Vec<(Vec<u8>, String)> {
    let mut out = Vec::with_capacity(seg.lines.len());
    for (i, line) in seg.lines.iter().enumerate() {
        let d: Option<&stratum_parse::Derived> = seg.derived.get(i).and_then(|x| x.as_deref());
        let code = line.code(seg.src, d);
        if code.trim().is_empty() {
            continue;
        }
        let lines: [LogicalLine; 1] = [*line];
        let derived: [DerivedText; 1] = [seg.derived.get(i).and_then(Clone::clone)];
        let mut canon = Vec::with_capacity(code.len() + 16);
        stratum_parse::canon::for_each_canon_token(seg.src, &lines, &derived, |kind, text| {
            // CONTRACTS §1.2 rule 6, in its order: kind byte, little-endian u32
            // length, then the bytes.
            canon.push(kind as u8);
            canon.extend_from_slice(&(text.len() as u32).to_le_bytes());
            canon.extend_from_slice(text);
        });
        out.push((
            canon,
            seg.src
                .get(line.code_span.start as usize..line.code_span.end as usize)
                .unwrap_or("")
                .to_owned(),
        ));
    }
    out
}

/// Every comment's text, trimmed, in document order.
fn comment_texts(src: &str, seg: &Segmentation<'_>) -> Vec<String> {
    let mut out = Vec::new();
    for (i, line) in seg.lines.iter().enumerate() {
        let d: Option<&stratum_parse::Derived> = seg.derived.get(i).and_then(|x| x.as_deref());
        let map = line.map(d);
        let mut runs: Vec<stratum_proto::Span> = if map.dst_len() > 0 {
            map.span_to_source(stratum_proto::Span {
                start: 0,
                end: map.dst_len(),
            })
            .to_vec()
        } else {
            Vec::new()
        };
        runs.sort_by_key(|s| s.start);
        let mut cursor = line.span.start;
        let mut push = |from: u32, to: u32| {
            if to > from {
                let t = src.get(from as usize..to as usize).unwrap_or("").trim();
                if !t.is_empty() {
                    out.push(t.to_owned());
                }
            }
        };
        for r in &runs {
            push(cursor, r.start);
            cursor = cursor.max(r.end);
        }
        push(cursor, line.span.end);
    }
    out
}

fn first_difference(a: &[String], b: &[String]) -> (&'static str, String) {
    for (x, y) in a.iter().zip(b.iter()) {
        if x != y {
            return if x < y {
                ("before", x.clone())
            } else {
                ("after", y.clone())
            };
        }
    }
    if a.len() > b.len() {
        ("before", a.get(b.len()).cloned().unwrap_or_default())
    } else {
        ("after", b.get(a.len()).cloned().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    const A: &str = "\
// %% Load
sysuse auto, clear

// %% Model
regress price mpg weight
predict yhat
";

    const MOVED: &str = "\
// %% Model
regress price mpg weight
predict yhat

// %% Load
sysuse auto, clear
";

    #[test]
    fn a_pure_section_move_passes() {
        assert_eq!(assert_statement_partition_preserved(A, MOVED), Ok(()));
    }

    #[test]
    fn dropping_a_statement_is_refused() {
        let dropped = MOVED.replace("predict yhat\n", "");
        assert!(matches!(
            assert_statement_partition_preserved(A, &dropped),
            Err(PartitionDrift::StatementCount { .. })
        ));
    }

    #[test]
    fn altering_one_byte_inside_a_moved_statement_is_refused() {
        let tweaked = MOVED.replace("price mpg weight", "price mpg weigth");
        assert!(matches!(
            assert_statement_partition_preserved(A, &tweaked),
            Err(PartitionDrift::StatementChanged { .. })
        ));
    }

    #[test]
    fn splitting_a_continuation_chain_across_the_boundary_is_refused() {
        let before = "regress price ///\n    mpg weight\nsummarize price\n";
        // The "move" left the continuation behind: two statements became three.
        let after = "    mpg weight\nsummarize price\nregress price ///\n";
        assert!(assert_statement_partition_preserved(before, after).is_err());
    }

    #[test]
    fn dropping_a_comment_is_refused() {
        let stripped = MOVED.replace("// %% Load\n", "");
        assert!(matches!(
            assert_statement_partition_preserved(A, &stripped),
            Err(PartitionDrift::CommentChanged { .. })
        ));
    }

    #[test]
    fn moving_a_statement_into_a_delimit_semi_stretch_is_refused() {
        let before = "di 1\n#delimit ;\ndi 2;\n#delimit cr\n";
        // `di 1` now runs in `;` mode: same bytes, different program.
        let after = "#delimit ;\ndi 1\ndi 2;\n#delimit cr\n";
        assert!(assert_statement_partition_preserved(before, after).is_err());
    }

    #[test]
    fn reordering_two_identical_statements_is_a_no_op() {
        let before = "summarize price\ndi 1\nsummarize price\n";
        let after = "summarize price\nsummarize price\ndi 1\n";
        // The multiset is the same and the bytes are the same, so this is a
        // legal permutation — and it is, because the two `summarize` statements
        // are indistinguishable.
        assert_eq!(assert_statement_partition_preserved(before, after), Ok(()));
    }

    #[test]
    fn an_added_statement_is_refused() {
        let added = format!("{MOVED}drop _all\n");
        assert!(assert_statement_partition_preserved(A, &added).is_err());
    }
}
