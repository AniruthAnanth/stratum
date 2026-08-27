//! `cargo xtask wasm` — build `stratum-wasm`, compress it, and enforce the two
//! gates that keep the webview honest.
//!
//! **Gate 1, size.** IMPLEMENTATION_PLAN W11a: the brotli-compressed artifact
//! must stay under 700 KB. The module is bundled, not fetched, so the number is
//! not about network time — it is about the webview parsing and compiling wasm
//! before the first frame, and about noticing the day someone links a formatter
//! or a locale table into the keystroke path.
//!
//! **Gate 2, the stub fence.** `--check-bundle` greps a built frontend for the
//! development stub's sentinel. `apps/desktop/src/wasm/stub/**` is a naive line
//! splitter that must never reach a user; the Vite `define` in `loader.ts` is
//! what removes it, and this is what proves the removal happened. A fence you do
//! not test is a fence you do not have.
//!
//! # Toolchain
//!
//! `wasm-pack` is preferred: it runs `wasm-bindgen` and `wasm-opt` and produces
//! exactly what ships. `wasm-bindgen` alone is accepted (no `wasm-opt`, so the
//! measurement is conservative — it can only over-report). With neither, the
//! command fails and says how to install one, unless `--allow-unbundled` is
//! passed, which measures the raw `cargo` output for local iteration and says
//! loudly that the number is not the shipping one.

use std::process::Command;

use anyhow::{bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;

use crate::Ctx;

/// The gate, in bytes. IMPLEMENTATION_PLAN W11a / W11b: "≤ 700 KB brotli".
pub const MAX_BROTLI_BYTES: u64 = 700 * 1024;

/// Where the loader expects the generated glue, relative to the repo root.
///
/// NOTE FOR W00: this directory is build output and belongs in `.gitignore`
/// alongside `dist/`. It is untracked, so `xtask ownership` ignores it, but
/// without the ignore line it shows up in every `git status` under W11a's
/// `apps/desktop/src/wasm/**`.
pub const DEFAULT_OUT_DIR: &str = "apps/desktop/src/wasm/generated";

/// The literal `apps/desktop/src/wasm/stub/index.ts` carries.
///
/// Spelled in pieces here for the same reason it is spelled in pieces there: a
/// checker that contains the string it searches for would match its own source
/// if either ever landed in a bundle directory.
fn stub_sentinel() -> String {
    ["STRATUM", "WASM", "STUB", "DO", "NOT", "SHIP"].join("_")
}

#[derive(Args)]
pub struct Cmd {
    /// Add the `panic-hook` feature.
    ///
    /// `wasm-pack` also drops to the dev profile; the cargo fallback stays on
    /// release, so the size number printed stays comparable between runs. Never
    /// used for a release measurement either way — the hook costs ~10 KB to
    /// print a stack trace no user reads.
    #[arg(long)]
    pub dev: bool,

    /// Where to write the wasm-bindgen output.
    #[arg(long, value_name = "DIR")]
    pub out_dir: Option<Utf8PathBuf>,

    /// Measure and gate an artifact that is already built.
    #[arg(long)]
    pub skip_build: bool,

    /// Override the size gate, in kilobytes. CI never passes this.
    #[arg(long, value_name = "KB")]
    pub max_brotli_kb: Option<u64>,

    /// Accept a raw `cargo` build when neither wasm-pack nor wasm-bindgen is
    /// installed. The measurement is then an upper bound, not the artifact.
    #[arg(long)]
    pub allow_unbundled: bool,

    /// Scan a built frontend for the development stub and fail if it survived.
    /// Runs on its own; no wasm build is performed.
    #[arg(long, value_name = "DIR")]
    pub check_bundle: Option<Utf8PathBuf>,
}

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    if let Some(dir) = &cmd.check_bundle {
        let dir = absolute(ctx, dir);
        return check_bundle(&dir);
    }

    let out_dir = cmd
        .out_dir
        .clone()
        .map_or_else(|| ctx.path(DEFAULT_OUT_DIR), |d| absolute(ctx, &d));
    let limit = cmd.max_brotli_kb.map_or(MAX_BROTLI_BYTES, |kb| kb * 1024);

    let artifact = if cmd.skip_build {
        find_existing(&out_dir)?
    } else {
        build(ctx, cmd, &out_dir)?
    };

    let raw = std::fs::metadata(&artifact.wasm)
        .with_context(|| format!("stat {}", artifact.wasm))?
        .len();
    let compressed = brotli(&artifact.wasm, &artifact.brotli)?;

    println!("xtask wasm: {}", artifact.wasm);
    println!("  built by      {}", artifact.builder);
    println!("  raw           {}", human(raw));
    println!(
        "  brotli        {}  -> {}",
        human(compressed),
        artifact.brotli
    );
    println!("  gate          {}", human(limit));

    if artifact.builder == Builder::RawCargo {
        eprintln!(
            "xtask wasm: WARNING — built without wasm-bindgen, so this is NOT the \
             shipping artifact. Install wasm-pack before trusting the number."
        );
    }

    check_size(compressed, limit)
}

// ---------------------------------------------------------------------------
// The gates. Pure functions, so they are tested without a toolchain.
// ---------------------------------------------------------------------------

/// Fail when the compressed artifact exceeds the gate.
pub fn check_size(compressed: u64, limit: u64) -> Result<()> {
    if compressed > limit {
        bail!(
            "the wasm module is {} brotli-compressed, over the {} gate by {}.\n\
             This is a hard budget (IMPLEMENTATION_PLAN W11a): the module is \
             parsed and compiled by the webview before the first frame. Find \
             what was linked in — `twiggy top` on the .wasm names it — rather \
             than raising the number.",
            human(compressed),
            human(limit),
            human(compressed - limit)
        );
    }
    println!(
        "xtask wasm: OK — {} of the {} brotli budget ({:.0} %)",
        human(compressed),
        human(limit),
        (compressed as f64 / limit as f64) * 100.0
    );
    Ok(())
}

/// Files a bundle scan looks at. Anything a browser could execute or fetch.
const BUNDLE_EXTENSIONS: &[&str] = &["js", "mjs", "cjs", "css", "html", "map"];

/// True when `contents` carries the stub sentinel.
///
/// A plain substring scan, deliberately: the sentinel is assembled from six
/// fragments at run time in the stub, so a minifier that inlines the join is the
/// case this catches, and a minifier that does not inline it leaves the six
/// fragments — which is why the scan also looks for the fragment sequence.
pub fn contains_stub(contents: &str) -> bool {
    let sentinel = stub_sentinel();
    if contents.contains(&sentinel) {
        return true;
    }
    // The unjoined form: the six string literals, in order, inside one file.
    let fragments = ["STRATUM", "WASM", "STUB", "DO", "NOT", "SHIP"];
    let mut cursor = 0usize;
    for fragment in fragments {
        let quoted = format!("\"{fragment}\"");
        let single = format!("'{fragment}'");
        match contents[cursor..]
            .find(&quoted)
            .or_else(|| contents[cursor..].find(&single))
        {
            Some(at) => cursor += at + quoted.len(),
            None => return false,
        }
    }
    true
}

fn check_bundle(dir: &Utf8Path) -> Result<()> {
    if !dir.is_dir() {
        bail!("--check-bundle {dir} is not a directory");
    }
    let mut scanned = 0usize;
    let mut offenders = Vec::new();
    walk(dir, &mut |path| {
        let Some(ext) = path.extension() else {
            return Ok(());
        };
        if !BUNDLE_EXTENSIONS.contains(&ext) {
            return Ok(());
        }
        scanned += 1;
        // A bundle chunk is not guaranteed to be UTF-8 (source maps embed
        // arbitrary strings); a lossy read is right here because we are looking
        // for an ASCII needle.
        let bytes = std::fs::read(path).with_context(|| format!("read {path}"))?;
        if contains_stub(&String::from_utf8_lossy(&bytes)) {
            offenders.push(path.to_owned());
        }
        Ok(())
    })?;

    if !offenders.is_empty() {
        bail!(
            "the development stub reached the production bundle:\n{}\n\n\
             `apps/desktop/src/wasm/stub/**` is a naive line splitter and must \
             never ship. Check that vite.config.ts defines \
             `__STRATUM_ALLOW_WASM_STUB__` to the literal `false` in production \
             — the loader's dynamic import is dropped only when that fold makes \
             the branch unreachable.",
            offenders
                .iter()
                .map(|p| format!("  {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    println!("xtask wasm: OK — no development stub in {scanned} bundle file(s) under {dir}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Building.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Builder {
    WasmPack,
    WasmBindgen,
    RawCargo,
    /// `--skip-build`: whatever produced it, we did not, and we will not claim
    /// otherwise in a log someone reads to decide whether a number is trustworthy.
    Prebuilt,
}

impl std::fmt::Display for Builder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Builder::WasmPack => "wasm-pack (wasm-bindgen + wasm-opt)",
            Builder::WasmBindgen => "wasm-bindgen (no wasm-opt)",
            Builder::RawCargo => "cargo only (NOT the shipping artifact)",
            Builder::Prebuilt => "a previous run (--skip-build)",
        })
    }
}

struct Artifact {
    wasm: Utf8PathBuf,
    brotli: Utf8PathBuf,
    builder: Builder,
}

fn build(ctx: &Ctx, cmd: &Cmd, out_dir: &Utf8Path) -> Result<Artifact> {
    let crate_dir = ctx.path("crates/stratum-wasm");
    if !crate_dir.is_dir() {
        bail!("crates/stratum-wasm does not exist");
    }
    std::fs::create_dir_all(out_dir).with_context(|| format!("mkdir {out_dir}"))?;

    let features = if cmd.dev { vec!["panic-hook"] } else { vec![] };

    if which("wasm-pack").is_some() {
        let mut c = Command::new("wasm-pack");
        c.current_dir(&ctx.root)
            .arg("build")
            .arg(crate_dir.as_str())
            .args(["--target", "web"])
            .args(["--out-dir", out_dir.as_str()])
            .args(["--out-name", "stratum_wasm"])
            .arg(if cmd.dev { "--dev" } else { "--release" });
        if !features.is_empty() {
            c.arg("--").args(["--features", &features.join(",")]);
        }
        status(c, "wasm-pack build")?;
        return Ok(artifact(out_dir, Builder::WasmPack));
    }

    let raw = cargo_build(ctx, &features)?;
    if which("wasm-bindgen").is_some() {
        let mut c = Command::new("wasm-bindgen");
        c.current_dir(&ctx.root)
            .args(["--target", "web"])
            .args(["--out-dir", out_dir.as_str()])
            .args(["--out-name", "stratum_wasm"])
            .arg(raw.as_str());
        status(c, "wasm-bindgen")?;
        return Ok(artifact(out_dir, Builder::WasmBindgen));
    }

    if !cmd.allow_unbundled {
        bail!(
            "neither wasm-pack nor wasm-bindgen is on PATH, so the shipping \
             artifact cannot be produced.\n  cargo install wasm-pack\n\
             Pass --allow-unbundled to measure the raw cargo output instead; \
             that number is an upper bound for local iteration, not the gate."
        );
    }
    Ok(Artifact {
        brotli: with_extension(&raw, "wasm.br"),
        wasm: raw,
        builder: Builder::RawCargo,
    })
}

/// Materialise the wasm target.
///
/// `rust-toolchain.toml` deliberately omits a `targets` list — listing seven
/// triples would make a first `cargo check` on a laptop pull ~1 GB of std — and
/// names this command as one of the two places that adds what it needs. Failure
/// is not fatal: an offline machine with the target already installed must still
/// build, and a machine without it gets rustc's own error, which says exactly
/// what to run.
fn ensure_target(ctx: &Ctx) {
    let Some(rustup) = which("rustup") else {
        return;
    };
    let mut c = Command::new(rustup);
    c.current_dir(&ctx.root)
        .args(["target", "add", "wasm32-unknown-unknown"]);
    let _ = c.status();
}

fn cargo_build(ctx: &Ctx, features: &[&str]) -> Result<Utf8PathBuf> {
    ensure_target(ctx);
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(&ctx.root)
        .args(["build", "-p", "stratum-wasm"])
        .args(["--target", "wasm32-unknown-unknown"])
        .arg("--release");
    if features.is_empty() {
        c.arg("--no-default-features");
    } else {
        c.args(["--features", &features.join(",")]);
    }
    // Debug info is ~85 % of the module and wasm-bindgen keeps it. The release
    // profile asks for line tables because a native backtrace needs them; a
    // webview does not.
    c.env("RUSTFLAGS", "-C strip=symbols");
    status(c, "cargo build --target wasm32-unknown-unknown")?;
    let path = ctx.path("target/wasm32-unknown-unknown/release/stratum_wasm.wasm");
    if !path.is_file() {
        bail!("cargo did not produce {path}");
    }
    Ok(path)
}

fn artifact(out_dir: &Utf8Path, builder: Builder) -> Artifact {
    let wasm = out_dir.join("stratum_wasm_bg.wasm");
    Artifact {
        brotli: with_extension(&wasm, "wasm.br"),
        wasm,
        builder,
    }
}

fn with_extension(p: &Utf8Path, ext: &str) -> Utf8PathBuf {
    let mut out = p.to_owned();
    out.set_extension(ext);
    out
}

// ---------------------------------------------------------------------------
// Compression.
// ---------------------------------------------------------------------------

/// Compress `src` to `dst` at quality 11 and return the compressed size.
///
/// The `brotli` CLI first, node's `zlib.brotliCompressSync` second. Both are
/// deterministic at fixed parameters and produce the same length, and node is
/// already a required part of the toolchain, so the fallback is not a
/// second-class path — it is what CI uses on a runner without the CLI.
///
/// A brotli crate in `xtask/Cargo.toml` would be tidier, but that file is W00's.
fn brotli(src: &Utf8Path, dst: &Utf8Path) -> Result<u64> {
    if which("brotli").is_some() {
        let mut c = Command::new("brotli");
        c.args(["-q", "11", "-f", "-k", "-o", dst.as_str(), src.as_str()]);
        status(c, "brotli")?;
    } else if let Some(node) = which("node") {
        let script = format!(
            "const z=require('zlib'),f=require('fs');\
             f.writeFileSync({dst:?},z.brotliCompressSync(f.readFileSync({src:?}),\
             {{params:{{[z.constants.BROTLI_PARAM_QUALITY]:11}}}}));",
            dst = dst.as_str(),
            src = src.as_str()
        );
        let mut c = Command::new(node);
        c.args(["-e", &script]);
        status(c, "node brotli")?;
    } else {
        bail!(
            "no brotli compressor found. Install the `brotli` CLI, or make \
             `node` available — the toolchain requires it anyway."
        );
    }
    Ok(std::fs::metadata(dst)
        .with_context(|| format!("stat {dst}"))?
        .len())
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

fn find_existing(out_dir: &Utf8Path) -> Result<Artifact> {
    let a = artifact(out_dir, Builder::Prebuilt);
    if !a.wasm.is_file() {
        bail!("--skip-build was passed but {} does not exist", a.wasm);
    }
    Ok(a)
}

fn absolute(ctx: &Ctx, p: &Utf8Path) -> Utf8PathBuf {
    if p.is_absolute() {
        p.to_owned()
    } else {
        ctx.path(p.as_str())
    }
}

fn which(bin: &str) -> Option<Utf8PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(bin);
        candidate
            .is_file()
            .then(|| Utf8PathBuf::from_path_buf(candidate).ok())
            .flatten()
    })
}

fn status(mut c: Command, what: &str) -> Result<()> {
    let st = c.status().with_context(|| format!("running {what}"))?;
    if !st.success() {
        bail!("{what} failed ({st})");
    }
    Ok(())
}

fn walk(dir: &Utf8Path, f: &mut dyn FnMut(&Utf8Path) -> Result<()>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {dir}"))? {
        let entry = entry?;
        let path = Utf8PathBuf::from_path_buf(entry.path())
            .map_err(|p| anyhow::anyhow!("non-UTF-8 path {}", p.display()))?;
        if entry.file_type()?.is_dir() {
            walk(&path, f)?;
        } else {
            f(&path)?;
        }
    }
    Ok(())
}

/// Sizes as a human reads them, so a diff of two CI logs is comparable.
fn human(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_is_700_kb() {
        // Pinned against IMPLEMENTATION_PLAN W11a. If this changes, the plan
        // changed, and that is an architect decision rather than a build fix.
        assert_eq!(MAX_BROTLI_BYTES, 716_800);
    }

    #[test]
    fn size_gate_admits_and_rejects() {
        assert!(check_size(MAX_BROTLI_BYTES - 1, MAX_BROTLI_BYTES).is_ok());
        assert!(check_size(MAX_BROTLI_BYTES, MAX_BROTLI_BYTES).is_ok());
        let over = check_size(MAX_BROTLI_BYTES + 1, MAX_BROTLI_BYTES).unwrap_err();
        let msg = format!("{over}");
        assert!(msg.contains("over the"), "{msg}");
        assert!(
            msg.contains("twiggy"),
            "the failure must say how to diagnose it"
        );
    }

    #[test]
    fn the_stub_sentinel_is_found_joined_or_split() {
        // As it appears once a minifier has folded the join.
        assert!(contains_stub("const x=\"STRATUM_WASM_STUB_DO_NOT_SHIP\";"));
        // As it appears when the array survives bundling, double or single quoted.
        assert!(contains_stub(
            r#"const S=["STRATUM","WASM","STUB","DO","NOT","SHIP"].join("_")"#
        ));
        assert!(contains_stub(
            r#"const S=['STRATUM','WASM','STUB','DO','NOT','SHIP'].join('_')"#
        ));
    }

    #[test]
    fn a_clean_bundle_passes() {
        assert!(!contains_stub("export function segment(){return[]}"));
        // Out of order, so a bundle that merely mentions the words is not a hit.
        assert!(!contains_stub(r#"["SHIP","STUB","WASM"]"#));
        // A partial match must not count.
        assert!(!contains_stub(r#"["STRATUM","WASM","STUB"]"#));
    }

    #[test]
    fn bundle_scan_walks_only_web_assets() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/index-abc.js"), "console.log(1)").unwrap();
        // A `.ts` source is not a bundle asset: the check is about what shipped,
        // and the stub's own source is on disk in every checkout.
        std::fs::write(
            root.join("assets/notes.ts"),
            "const S=\"STRATUM_WASM_STUB_DO_NOT_SHIP\"",
        )
        .unwrap();
        check_bundle(&root).unwrap();

        std::fs::write(
            root.join("assets/index-def.js"),
            "const S=\"STRATUM_WASM_STUB_DO_NOT_SHIP\"",
        )
        .unwrap();
        let err = check_bundle(&root).unwrap_err();
        assert!(format!("{err}").contains("index-def.js"), "{err}");
    }

    #[test]
    fn human_sizes_read_the_way_a_budget_does() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1024), "1.0 KB");
        assert_eq!(human(MAX_BROTLI_BYTES), "700.0 KB");
    }
}
