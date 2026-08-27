//! The host half of the bridge, **compiled**.
//!
//! `apps/desktop/src-tauri/src/e2e_cmds.rs` is W25's file (`docs/ownership.toml`,
//! `[[unit]] W25`) living inside W17's crate. Declaring it needs `mod e2e_cmds;`
//! in `apps/desktop/src-tauri/src/main.rs`, which is W17's file, so R0 forbids
//! W25 from writing it — and through three repair rounds nobody did. The result
//! was 683 lines and four tests that **no compiler had ever seen**: not built by
//! `cargo build --workspace`, not type-checked by `clippy --workspace
//! --all-targets --all-features -- -D warnings`, not run by `cargo test
//! --workspace`. That is the W07 defect class, and this unit had two instances
//! of it (this one and `tests/e2e/scenario_b.rs`) while reporting a third.
//!
//! One `#[path]` from W25's own crate fixes what R0 allows W25 to fix. It is the
//! same move `fence.rs` made — the gate moved out of `xtask/src/e2e.rs` and into
//! this crate for the identical reason — and the same mechanism `lib.rs` already
//! uses at its foot to compile `tests/e2e/*.rs`.
//!
//! # What this does and does not settle
//!
//! **Does:** the non-Tauri ~460 lines of `e2e_cmds.rs` — `Control`, the
//! correlation table, the framing, the deadlines — and their four tests are now
//! built and run by every `cargo test --workspace` on all three OSes. The
//! command-name constants are now checked by the *compiler* against
//! [`crate::fence::FENCED_COMMANDS`] rather than only by the source-text scrape
//! in `tests/e2e/harness.rs`. And `stratum-e2e-host-probe` is a real linked
//! executable that carries those constants, which is what ADR-011's fence needs
//! as a positive control (`fence --require-present`) and did not have.
//!
//! **Does not:** prove the `e2e` *cargo feature* gate holds on `stratum-desktop`.
//! That claim is a differential over two builds of that crate and it needs
//! `[features] e2e = []` in `apps/desktop/src-tauri/Cargo.toml` plus `mod
//! e2e_cmds;` in its `main.rs` — both W17's, both still owed, both spelled out
//! verbatim in the header of `e2e_cmds.rs` and in `.github/workflows/e2e.yml`.
//! `tauri_surface` is `#[cfg(feature = "e2e")]` and this crate declares no such
//! feature, so it stays uncompiled here and no Tauri edge is created: `cargo
//! tree -p stratum-e2e --all-features | grep -c tauri` is 0. Nothing below may
//! be read as "the fence is a differential now" — the workflow says which half
//! it ran, every time.

/// W25's host-side Tauri commands, compiled from where W17's crate will declare
/// them. Re-exported rather than copied so there is exactly one of it.
#[path = "../../../apps/desktop/src-tauri/src/e2e_cmds.rs"]
pub mod e2e_cmds;

#[cfg(test)]
mod tests {
    use super::e2e_cmds;
    use crate::fence;

    /// The drift check, done by the **compiler** rather than by scraping the
    /// source text.
    ///
    /// `tests/e2e/harness.rs` also checks this by reading `e2e_cmds.rs` as a
    /// string and pulling `pub const E2E_*` out of it. That test earns its keep
    /// — it catches a *third* name being added to the host and not to the
    /// fence, which no equality assertion can see — but it is a parser over
    /// someone's formatting. This one cannot be fooled by formatting: if the
    /// two constants ever differ, `stratum-e2e` stops compiling green.
    #[test]
    fn the_fence_greps_for_exactly_what_the_host_registers() {
        assert_eq!(e2e_cmds::E2E_DISPATCH, fence::E2E_DISPATCH);
        assert_eq!(e2e_cmds::E2E_SNAPSHOT, fence::E2E_SNAPSHOT);
        assert!(fence::FENCED_COMMANDS.contains(&e2e_cmds::E2E_DISPATCH));
        assert!(fence::FENCED_COMMANDS.contains(&e2e_cmds::E2E_SNAPSHOT));
    }

    /// The names the fence must NOT look for.
    ///
    /// `PORT_ENV`, `HOST_ENV` and `REQUEST_EVENT` are configuration and an event
    /// topic, not Tauri commands. Fencing them would make the gate fail on a
    /// shipped build for a string that is not a backdoor, and a gate that cries
    /// wolf is a gate that gets an `|| true` appended to it.
    #[test]
    fn configuration_names_are_not_fenced() {
        for name in [
            e2e_cmds::PORT_ENV,
            e2e_cmds::HOST_ENV,
            e2e_cmds::REQUEST_EVENT,
        ] {
            assert!(
                !fence::FENCED_COMMANDS.contains(&name),
                "{name} is not a Tauri command and must not be fenced"
            );
        }
    }
}
