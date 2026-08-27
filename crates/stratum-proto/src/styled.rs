//! CONTRACTS.md §5.2 (A12) — the single flattening function for styled output.
//!
//! `stratum_stats::*::classic_text` returns `Vec<StyledRun>` rather than a
//! `String` because style cannot be recovered from plain text after the fact.
//! Everything that needs the bytes instead of the styling — the CLI's text mode,
//! the log file writer, `log_copy`, and the byte-exactness goldens — goes
//! through [`to_plain`], so a change to styling can never move a golden.

use crate::result::StyledRun;

/// Concatenate the runs' text, discarding style. The ONLY sanctioned way to turn
/// styled output back into bytes.
///
/// This is the whole of `stratum-proto`'s logic budget, along with
/// [`crate::BlockId::is_real`]: a two-line fold that must live beside the type it
/// flattens, because a second copy in the CLI and a third in the log writer is
/// exactly how the goldens and the log drift apart.
#[must_use]
pub fn to_plain(runs: &[StyledRun]) -> String {
    let mut out = String::with_capacity(runs.iter().map(|r| r.text.len()).sum());
    for run in runs {
        out.push_str(&run.text);
    }
    out
}
