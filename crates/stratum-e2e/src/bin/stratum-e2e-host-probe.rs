//! **The fence's positive control, as an artifact.** ADR-011, plan W25.
//!
//! `stratum-e2e-gate fence <binary>` asserts a shipped build carries none of
//! [`FENCED_COMMANDS`]. That is a *negative* assertion, and a negative assertion
//! over a grep is worth exactly what its positive control is worth: a fence
//! looking for a name no build ever emits passes on every binary forever, which
//! is ADR-011's failure mode wearing a green tick.
//!
//! The control the acceptance bullet asks for is `stratum-desktop --features
//! e2e`, and it cannot be built: `apps/desktop/src-tauri/Cargo.toml` has no
//! `[features]` table and its `main.rs` has no `mod e2e_cmds;`, both W17's files
//! and both still owed (R0). Until they land the positive half had nothing to
//! run against, so CI ran the negative half alone — which is precisely the
//! vacuous-capable shape the bullet exists to forbid.
//!
//! This binary is what W25 can build without reaching across: a real, linked,
//! platform-native executable — Mach-O here, ELF and PE on the other two
//! runners — whose command-name strings come from
//! `e2e_cmds` itself, through [`stratum_e2e::host`]. `stratum-e2e-gate fence
//! --require-present` runs against it on every push, so the gate's positive
//! branch is a live CI path rather than a branch only unit tests with byte
//! literals have ever taken.
//!
//! # Read the claim exactly
//!
//! This proves the scanner finds `e2e_cmds`'s own constants in a real binary.
//! It does **not** prove the `e2e` cargo feature gate holds on `stratum-desktop`
//! — that is a differential over two builds of *that* crate and it needs W17's
//! two lines. `.github/workflows/e2e.yml` prints which of the two claims it
//! made, every run, and does not conflate them.

use stratum_e2e::fence::FENCED_COMMANDS;
use stratum_e2e::host::e2e_cmds::{E2E_DISPATCH, E2E_SNAPSHOT, PORT_ENV, REQUEST_EVENT};

fn main() {
    // Referenced, not merely declared: a `const &str` nothing uses is a name
    // rustc is free to leave out of `.rodata`, and an artifact that does not
    // contain the needle is not a control. This is the same mechanism the
    // feature gate relies on in reverse — with `e2e` off, `tauri_surface` is
    // the only referent and it is gone, so the strings are gone.
    println!("stratum-e2e host probe");
    println!("  dispatch  {E2E_DISPATCH}");
    println!("  snapshot  {E2E_SNAPSHOT}");
    println!("  port env  {PORT_ENV}");
    println!("  event     {REQUEST_EVENT}");
    println!("  fenced    {FENCED_COMMANDS:?}");
}
