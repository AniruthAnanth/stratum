//! The Stata differential-test harness (plan W23, spec §32).
//!
//! # The shape of the problem
//!
//! Stata is the oracle for this product, and the oracle is not always
//! available: it needs a licence, and the licence on the capture machine has
//! expired. The harness is therefore built around one comparator fed by two
//! sources of Stata's behaviour:
//!
//! * **The committed corpus** — `tests/golden/stata18/*.log`, captured from a
//!   real licensed StataMP 18.5 before the licence lapsed. Irreplaceable, and
//!   immutable: the banner's licence lines are `[redacted]` and the normalizer
//!   here tolerates that. The corpus phase re-runs everywhere, on every push,
//!   with **no Stata installed**.
//! * **A live Stata** — wherever one exists (a future machine, a colleague's),
//!   `tests/difftest/cases/**` run through `scripts/run-stata.sh` +
//!   `tests/difftest/ado/stratum_capture.ado` and are compared fresh. Absent a
//!   usable Stata this half exits [`EXIT_SKIP`] (77), which CI reports as
//!   neutral — visible as "did not run", never as green.
//!
//! Our side is **regenerated on every run** and never committed, so a
//! regression cannot be blessed into the repository by re-capturing it.
//! Until W09's engine edge lands in `stratum-cli`, "our runtime" for the
//! covered surface is `stratum-stats` called directly ([`corpus::regen`]);
//! the swap to `stratum run --capture` is confined to that one function.
//!
//! # The two channels
//!
//! 1. **Classic text** — byte-exact at `linesize 80` (05 §17.3 row 1: a
//!    formatting bug and a numerics bug both fail it).
//! 2. **Captured results** — `r()`/`e()` as [`stratum_proto::capture::
//!    CaptureRecord`]: numerics travel as `%21.17g` strings, are parsed to
//!    f64 **only at compare time** ([`capture::Value`]), and match under the
//!    per-class tolerances of [`tolerance::Class`]. Missing values match **by
//!    code, never by tolerance** — `.a` and `.b` are one ulp apart and any
//!    tolerance would call them equal.
//!
//! # What is asserted (ADR-017)
//!
//! Counters, not durations: cases compared, blocks byte-checked, scalars /
//! macros / matrices / functions compared, mismatches found. The integration
//! tests pin these to exact values so a case that silently drops out turns
//! the suite red.

pub mod capture;
pub mod compare;
pub mod corpus;
pub mod fixture;
pub mod lint;
pub mod log;
pub mod norm;
pub mod stata;
pub mod tolerance;

/// Exit code for "the differential could not run because no usable Stata
/// exists" — the classic automake SKIP code. `stata-diff.yml` maps it to a
/// neutral outcome; anything else red is red.
pub const EXIT_SKIP: i32 = 77;

/// Exit code for a comparison that ran and found differences.
pub const EXIT_DIFF: i32 = 1;

/// Exit code for an environment or usage error (missing corpus, bad flags).
pub const EXIT_ERR: i32 = 2;

use camino::{Utf8Path, Utf8PathBuf};

/// The repository root, derived from this crate's manifest directory so the
/// binary behaves identically from any working directory (the same rule
/// `xtask` follows).
#[must_use]
pub fn repo_root() -> Utf8PathBuf {
    let manifest = Utf8Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Utf8Path::parent)
        .expect("crates/stratum-difftest has a grandparent")
        .to_path_buf()
}
