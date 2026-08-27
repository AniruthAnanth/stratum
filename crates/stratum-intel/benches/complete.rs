//! **Placeholder — not yet written.**
//!
//! W20's acceptance asks for a criterion bench of `complete()` at the A11 cap
//! (2 048 variables, 512 of everything else). The unit was cut off before it
//! landed, and `[[bench]] harness = false` means `cargo bench` runs this `main`
//! and reports nothing, so leaving it undocumented would read as a passing
//! benchmark rather than a missing one.
//!
//! The ADR-017 gate itself is NOT missing. Performance here is asserted by a
//! counter, never a stopwatch, and
//! `stratum_intel::complete` carries that assertion as a unit test
//! (`the_candidate_count_is_bounded_at_the_a11_cap`). What is absent is the
//! wall-clock number recorded alongside it.
fn main() {}
