//! `cargo xtask` — the repository's invariant checks and code generation.
//!
//! Every subcommand here backs a numbered CI invariant in `ARCHITECTURE.md` §8.
//! They are ordinary programs, not shell one-liners, for two reasons: a
//! contributor can run any of them locally with the same exit code CI will see,
//! and the graph-shaped ones (`layering`, `ownership`) need real graph and glob
//! semantics that `grep` cannot express without being wrong at the edges.
//!
//! Invariants owned by this binary:
//!
//! | §8 item | subcommand |
//! |---|---|
//! | 1, 2, 4, 5, 6 (crate half) | `layering` |
//! | 12 | `csp-check` |
//! | 13 | `ownership` |
//! | 14 (token half) | `tokens --check` |
//! | 9 | `normalize-ndjson`, `conformance` |
//!
//! §8.3, §8.6 (workflow half), §8.7, §8.11 and §8.14 are text scans over the
//! working tree and live in `scripts/check-topology.sh`.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Parser, Subcommand};

mod conformance;
mod csp_check;
// Registered by W23 (owner of `difftest.rs`), same shape and precedent as
// W25's `e2e` registration below: the module line, the variant and the
// dispatch arm are the three lines a subcommand needs from this file, and the
// crossing into W00's file is recorded in W23's return.
mod difftest;
// Registered by W22 (owner of `dist.rs`, `sign.rs`, `smoke.rs`, `goldens.rs`),
// under the same precedent as W23/W25/W11a below: module line, variant,
// dispatch arm — the three lines a subcommand needs from W00's file. Crossing
// recorded in W22's return.
mod dist;
mod e2e;
mod goldens;
mod layering;
mod normalize_ndjson;
mod ownership;
mod sdp1;
mod sign;
mod smoke;
mod tokens;
// Registered by W11a, which owns `wasm.rs` but not this file. A subcommand module
// is unreachable — not even compiled — until its crate root declares it, so every
// unit the plan hands an `xtask/src/*.rs` to (W11a here; W25's `e2e` above; W22
// for dist/sign/smoke/goldens) needs exactly these three lines from W00. Kept
// adjacent and minimal so the next registration is a one-line merge rather than a
// conflict.
mod wasm;

#[derive(Parser)]
#[command(
    name = "xtask",
    about = "Stratum repository invariants and code generation",
    version,
    max_term_width = 100
)]
struct Cli {
    /// Repository root. Defaults to the directory containing this crate's parent
    /// manifest, which is the workspace root by construction.
    #[arg(long, global = true, value_name = "DIR")]
    root: Option<Utf8PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// ARCHITECTURE §8.1/§8.2/§8.4/§8.5/§8.6 — crate dependency direction.
    Layering(layering::Cmd),
    /// ARCHITECTURE §8.13 — every tracked file owned by exactly one work unit.
    Ownership(ownership::Cmd),
    /// ARCHITECTURE §8.14 — generate (or verify) the two design-token artifacts.
    Tokens(tokens::Cmd),
    /// Generate or verify the SDP1 reference fixtures (CONTRACTS §8.1, ADR-007).
    Sdp1(sdp1::Cmd),
    /// CONTRACTS §7.2 — the `--deterministic` NDJSON normalizer (A8).
    NormalizeNdjson(normalize_ndjson::Cmd),
    /// ARCHITECTURE §8.9 — run `tests/conformance/**` and compare its
    /// `--deterministic` output across runs, thread counts and platforms.
    Conformance(conformance::Cmd),
    /// ARCHITECTURE §8.12 — the packaged app's CSP lists every fetchable scheme.
    CspCheck(csp_check::Cmd),
    /// IMPLEMENTATION_PLAN W23 / spec §32 — the Stata differential harness:
    /// corpus comparison without Stata, live differential with one (exit 77
    /// SKIP when absent).
    Difftest(difftest::Cmd),
    /// IMPLEMENTATION_PLAN W25 — the two-tier end-to-end harness (ADR-011, Q16).
    E2e(e2e::Cmd),
    /// IMPLEMENTATION_PLAN W22 — bundle staging and bundle-config invariants
    /// (never-steal file associations, entitlements policy, per-OS targets).
    Dist(dist::Cmd),
    /// IMPLEMENTATION_PLAN W22 / ADR-011 — signature verification and an
    /// honest report of which signing credentials this environment holds.
    Sign(sign::Cmd),
    /// IMPLEMENTATION_PLAN W22 / design 08 §8.5 — packaging smoke assertions,
    /// runnable locally against the packaged artifact.
    Smoke(smoke::Cmd),
    /// IMPLEMENTATION_PLAN W22 / design 08 §11.3 — golden hygiene: size cap
    /// and canonical order for the committed Stata oracle output.
    Goldens(goldens::Cmd),
    /// IMPLEMENTATION_PLAN W11a/W11b — build `stratum-wasm`, gate it at 700 KB
    /// brotli, and (`--check-bundle`) prove the development stub did not ship.
    Wasm(wasm::Cmd),
}

/// Options every check shares. Kept in one struct so `--root` behaves the same
/// everywhere and subcommand modules never reach for the process environment.
pub struct Ctx {
    pub root: Utf8PathBuf,
}

impl Ctx {
    pub fn path(&self, rel: &str) -> Utf8PathBuf {
        self.root.join(rel)
    }
}

/// Shared `--check` flag: verify a committed artifact instead of writing it.
#[derive(Args, Clone, Copy)]
pub struct CheckFlag {
    /// Verify the committed artifacts byte-for-byte instead of rewriting them.
    /// Exits non-zero on any drift. This is what CI runs.
    #[arg(long)]
    pub check: bool,
}

fn default_root() -> Result<Utf8PathBuf> {
    // `xtask` always lives at `<root>/xtask`, so its manifest directory's parent
    // is the workspace root. Deriving it this way rather than from the process
    // cwd means `cargo xtask` behaves identically from any subdirectory.
    let manifest = Utf8Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(Utf8Path::to_path_buf)
        .context("xtask manifest has no parent directory")
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let root = match cli.root {
        Some(r) => r,
        None => default_root()?,
    };
    anyhow::ensure!(root.is_dir(), "repository root {root} is not a directory");
    let ctx = Ctx { root };

    match cli.cmd {
        Cmd::Layering(c) => layering::run(&ctx, &c),
        Cmd::Ownership(c) => ownership::run(&ctx, &c),
        Cmd::Tokens(c) => tokens::run(&ctx, &c),
        Cmd::Sdp1(c) => sdp1::run(&ctx, &c),
        Cmd::NormalizeNdjson(c) => normalize_ndjson::run(&ctx, &c),
        Cmd::Conformance(c) => conformance::run(&ctx, &c),
        Cmd::CspCheck(c) => csp_check::run(&ctx, &c),
        Cmd::Difftest(c) => difftest::run(&ctx, &c),
        Cmd::E2e(c) => e2e::run(&ctx, &c),
        Cmd::Dist(c) => dist::run(&ctx, &c),
        Cmd::Sign(c) => sign::run(&ctx, &c),
        Cmd::Smoke(c) => smoke::run(&ctx, &c),
        Cmd::Goldens(c) => goldens::run(&ctx, &c),
        Cmd::Wasm(c) => wasm::run(&ctx, &c),
    }
}
