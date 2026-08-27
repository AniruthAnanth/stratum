//! Runs `tests/e2e/scenario_d.rs` today.
//!
//! The canonical file is at the repository root, where W25's e2e harness will
//! pick it up (`docs/ownership.toml` gives W26 both `crates/stratum-workspace/**`
//! and `tests/e2e/scenario_d.rs`). W25 has not landed, so without this include
//! the acceptance scenario for spec §5 — the product's central promise — would
//! compile nowhere and run never, which is exactly the state the audit found it
//! in (A32).
//!
//! `#[path]` rather than a copy: two copies of an acceptance test drift, and the
//! one that drifts is always the one nobody is running.

#[path = "../../../tests/e2e/scenario_d.rs"]
mod scenario_d;
