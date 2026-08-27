//! IMPLEMENTATION_PLAN W22 — bundle staging and bundle-config invariants.
//!
//! Four verbs:
//!
//! - `stage` — everything `tauri build` needs that is not committed inside
//!   `apps/desktop/src-tauri`: the `stratum` CLI copied to
//!   `binaries/stratum-<triple>` (Tauri's externalBin naming),
//!   `packaging/macos/Info.additions.plist` copied to `src-tauri/Info.plist`
//!   (the file the bundler merges over its generated plist — this is where
//!   `LSHandlerRank = Alternate` comes from), and `packaging/ado` copied next
//!   to the sidecar as `binaries/ado` (the tree the per-OS `bundle.resources`
//!   mapping carries into the bundle for `sysuse`). All destinations are
//!   untracked build products; `xtask ownership` skips untracked files by
//!   design.
//! - `check` — static assertions over the committed packaging files. This is
//!   the grep gate the W22 acceptance names: `entitlements.plist` must NOT
//!   contain `disable-library-validation` (ARCHITECTURE C27, ADR-011), the
//!   per-OS configs must not override the CSP (A21 stays checkable), and the
//!   file-association plist must rank every claim `Alternate`.
//! - `verify` — post-build assertions over a built `.app` (macOS host only):
//!   the merged Info.plist really carries the Stata UTIs at rank Alternate,
//!   and the binary's entitlements match the committed file.
//! - `cross-check` — `cargo check --target <triple>` for the release triples
//!   this host is not, with the per-triple toolchain arrangements a dev box
//!   needs (packaging/README.md, "Cross-target checks"). It names what it
//!   could not cover and why, instead of failing in the middle of a C build.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::process::Command;

use anyhow::{bail, ensure, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, PackageId};
use clap::{Args, Subcommand};
use serde_json::Value;

use crate::Ctx;

#[derive(Args)]
pub struct Cmd {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Stage uncommitted bundle inputs: the externalBin sidecar, the
    /// Info.plist additions, and the ado tree the bundle's `resources`
    /// mapping references. Run before `tauri build`.
    Stage {
        /// Path to a built `stratum` CLI. Defaults to `target/release/stratum`.
        #[arg(long, value_name = "FILE")]
        cli: Option<Utf8PathBuf>,
        /// Target triple(s) to stage the sidecar for. Defaults to the host
        /// triple. A universal macOS build needs both
        /// `aarch64-apple-darwin` and `x86_64-apple-darwin`, each staged from
        /// its own `--cli`-built binary (or a universal one).
        #[arg(long, value_name = "TRIPLE")]
        triple: Vec<String>,
    },
    /// Static invariants over the committed packaging configuration.
    Check,
    /// Assertions over a built bundle (macOS host).
    Verify {
        /// Path to the built `Stratum.app`.
        #[arg(long, value_name = "DIR")]
        app: Utf8PathBuf,
        /// Additionally assert the main binary contains both x86_64 and arm64.
        #[arg(long)]
        universal: bool,
    },
    /// `cargo check --target <triple>` for the release triples this host is
    /// not, arranging per triple what a dev box needs (see packaging/README.md,
    /// "Cross-target checks"). Reports PARTIAL, naming every crate it skipped
    /// and why, when this host cannot cover the whole workspace.
    CrossCheck {
        /// Triple(s) to check. Defaults to the two non-macOS release triples,
        /// x86_64-unknown-linux-gnu and x86_64-pc-windows-msvc.
        #[arg(long, value_name = "TRIPLE")]
        target: Vec<String>,
        /// Fail instead of reporting PARTIAL when a triple cannot be covered
        /// in full from this host.
        #[arg(long)]
        strict: bool,
    },
}

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    match &cmd.action {
        Action::Stage { cli, triple } => stage(ctx, cli.as_deref(), triple),
        Action::Check => check(ctx),
        Action::Verify { app, universal } => verify(app, *universal),
        Action::CrossCheck { target, strict } => cross_check(ctx, target, *strict),
    }
}

const SRC_TAURI: &str = "apps/desktop/src-tauri";

fn host_triple() -> Result<String> {
    let out = std::process::Command::new("rustc")
        .arg("-vV")
        .output()
        .context("running `rustc -vV` for the host triple")?;
    ensure!(out.status.success(), "`rustc -vV` failed");
    let text = String::from_utf8(out.stdout).context("rustc -vV output is not UTF-8")?;
    text.lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(str::to_owned)
        .context("`rustc -vV` printed no `host:` line")
}

fn stage(ctx: &Ctx, cli: Option<&Utf8Path>, triples: &[String]) -> Result<()> {
    // The host's own extension: the CLI being staged was built on this
    // machine, whatever triple it is being staged FOR.
    let host_ext = if cfg!(windows) { ".exe" } else { "" };
    let cli = match cli {
        Some(p) => p.to_owned(),
        None => {
            // Release first — what a shipping bundle wants — then debug, so
            // that a CI job which only needs the sidecar to EXIST (Tauri's
            // build script refuses to compile without it) does not have to
            // pay for a fat-LTO release build of the CLI to run the tests.
            let release = ctx.path(&format!("target/release/stratum{host_ext}"));
            let debug = ctx.path(&format!("target/debug/stratum{host_ext}"));
            if release.is_file() {
                release
            } else if debug.is_file() {
                println!("dist: no release CLI; staging the debug build {debug}");
                debug
            } else {
                release
            }
        }
    };
    ensure!(
        cli.is_file(),
        "no CLI at {cli}; build it first: cargo build -p stratum-cli (debug is accepted) \
         or cargo build --release -p stratum-cli"
    );

    let triples = if triples.is_empty() {
        vec![host_triple()?]
    } else {
        triples.to_vec()
    };

    let bin_dir = ctx.path(SRC_TAURI).join("binaries");
    std::fs::create_dir_all(&bin_dir).with_context(|| format!("creating {bin_dir}"))?;
    // A universal macOS bundle is compiled once per architecture before it is
    // lipo'd, and each per-arch compile runs the Tauri build script against its
    // OWN triple — which looks for `binaries/stratum-<arch>-apple-darwin`, not
    // the universal name. Staging only `stratum-universal-apple-darwin` fails
    // the aarch64 compile ("resource path … doesn't exist", package run
    // 33075924127). So the universal triple fans out to all three names.
    let staged: Vec<String> = triples
        .iter()
        .flat_map(|t| {
            if t == "universal-apple-darwin" {
                vec![
                    t.clone(),
                    "aarch64-apple-darwin".to_owned(),
                    "x86_64-apple-darwin".to_owned(),
                ]
            } else {
                vec![t.clone()]
            }
        })
        .collect();
    for triple in &staged {
        let ext = if triple.contains("windows") {
            ".exe"
        } else {
            ""
        };
        let dest = bin_dir.join(format!("stratum-{triple}{ext}"));
        std::fs::copy(&cli, &dest).with_context(|| format!("copying {cli} -> {dest}"))?;
        println!("dist: staged externalBin {dest}");
    }

    // The Info.plist merge source (macOS bundler convention). Harmless on
    // other hosts; the Windows/Linux bundlers never read it.
    let additions = ctx.path("packaging/macos/Info.additions.plist");
    let dest = ctx.path(SRC_TAURI).join("Info.plist");
    std::fs::copy(&additions, &dest).with_context(|| format!("copying {additions} -> {dest}"))?;
    println!("dist: staged {dest} (from packaging/macos/Info.additions.plist)");

    // The ado tree `sysuse` resolves (base/<first-letter>/<name>.dta). Staged
    // next to the sidecar because the desktop's build script — like the one
    // that refuses to compile without the sidecar — refuses to compile without
    // the files `bundle.resources` names; the mapping in every per-OS config
    // then carries the tree into the bundle, and the desktop host hands the
    // bundled location to the engine child through STRATUM_ADO_BASE.
    let ado_dest = bin_dir.join("ado");
    let staged_ado = stage_ado_tree(&ctx.path("packaging/ado"), &ado_dest)?;
    println!(
        "dist: staged ado tree {ado_dest} ({} file{})",
        staged_ado.len(),
        if staged_ado.len() == 1 { "" } else { "s" }
    );
    Ok(())
}

/// The one file `bundle.resources` maps today, relative to `packaging/ado` and
/// to the staged `binaries/ado`. A new ado file needs a new mapping line in
/// every per-OS config; `check_platform_conf` asserts this one is never lost.
const ADO_AUTO_DTA: &str = "base/a/auto.dta";

/// Copy the committed `packaging/ado` tree to `dest`, returning the staged
/// files relative to `dest`, sorted. A separate function so the unit test can
/// aim it at a tempdir instead of the working tree.
fn stage_ado_tree(src: &Utf8Path, dest: &Utf8Path) -> Result<Vec<Utf8PathBuf>> {
    ensure!(
        src.is_dir(),
        "no ado tree at {src}; packaging/ado is committed — restore it before staging"
    );
    let mut staged: Vec<Utf8PathBuf> = Vec::new();
    let mut queue: VecDeque<Utf8PathBuf> = VecDeque::from([Utf8PathBuf::new()]);
    while let Some(rel) = queue.pop_front() {
        let from = src.join(&rel);
        for entry in from
            .read_dir_utf8()
            .with_context(|| format!("reading {from}"))?
        {
            let entry = entry.with_context(|| format!("reading an entry of {from}"))?;
            let rel = rel.join(entry.file_name());
            if entry
                .file_type()
                .with_context(|| format!("stat {rel} under {src}"))?
                .is_dir()
            {
                queue.push_back(rel);
            } else {
                let to = dest.join(&rel);
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("creating {parent}"))?;
                }
                std::fs::copy(entry.path(), &to)
                    .with_context(|| format!("copying {} -> {to}", entry.path()))?;
                staged.push(rel);
            }
        }
    }
    staged.sort();
    ensure!(
        staged.iter().any(|p| p == ADO_AUTO_DTA),
        "the ado tree at {src} is missing {ADO_AUTO_DTA}, the file `sysuse auto` loads"
    );
    Ok(staged)
}

/// The static gate. Every assertion here is also a unit test below, so a PR
/// that breaks one fails `cargo nextest` even if nobody ran `xtask dist check`.
fn check(ctx: &Ctx) -> Result<()> {
    check_entitlements(&read(ctx, "apps/desktop/src-tauri/entitlements.plist")?)?;
    check_info_additions(&read(ctx, "packaging/macos/Info.additions.plist")?)?;
    for os in ["macos", "windows", "linux"] {
        let rel = format!("{SRC_TAURI}/tauri.{os}.conf.json");
        let text = read(ctx, &rel)?;
        let conf: Value =
            serde_json::from_str(&text).with_context(|| format!("{rel} is not valid JSON"))?;
        check_platform_conf(os, &conf).with_context(|| rel.clone())?;
    }
    check_nsis_hook(&read(ctx, "packaging/windows/file-assoc.nsh")?)?;
    check_ado_auto_dta(
        &std::fs::read(ctx.path("packaging/ado").join(ADO_AUTO_DTA))
            .context("reading the shipped packaging/ado auto.dta")?,
        &std::fs::read(ctx.path("tests/fixtures/dta/auto.dta"))
            .context("reading the committed auto.dta fixture")?,
    )?;
    println!("dist check: OK");
    Ok(())
}

/// The shipped auto.dta must be the committed fixture, byte for byte. The
/// fixture is the conformance oracle's input and cannot be regenerated (the
/// Stata licence has expired); a shipped copy that drifted would make a
/// packaged `sysuse auto` disagree with every golden cut from it.
fn check_ado_auto_dta(shipped: &[u8], fixture: &[u8]) -> Result<()> {
    ensure!(
        shipped == fixture,
        "packaging/ado/{ADO_AUTO_DTA} differs from tests/fixtures/dta/auto.dta; \
         re-copy the fixture (never the other direction)"
    );
    Ok(())
}

fn read(ctx: &Ctx, rel: &str) -> Result<String> {
    let path = ctx.path(rel);
    std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))
}

/// Strip XML comments so a check about GRANTED keys is not fooled — in either
/// direction — by documentation. The committed entitlements.plist explains in
/// a comment which entitlements are deliberately absent and why; that comment
/// must be allowed to name them.
fn strip_xml_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return out, // unterminated comment: nothing after it counts
        }
    }
    out.push_str(rest);
    out
}

/// ARCHITECTURE C27 / ADR-011 / W22 acceptance: the hardened-runtime
/// entitlements must carry the JIT exception and nothing that weakens library
/// validation. Nothing in v1 dlopens a third-party binary.
fn check_entitlements(raw: &str) -> Result<()> {
    let text = strip_xml_comments(raw);
    ensure!(
        text.contains("<key>com.apple.security.cs.allow-jit</key>"),
        "entitlements.plist must grant com.apple.security.cs.allow-jit \
         (WKWebView's JIT; without it the editor is visibly laggy)"
    );
    for banned in [
        "disable-library-validation",
        "allow-unsigned-executable-memory",
        "com.apple.security.automation.apple-events",
        "com.apple.security.app-sandbox",
    ] {
        ensure!(
            !text.contains(banned),
            "entitlements.plist must not contain `{banned}` (ADR-011; ARCHITECTURE C27)"
        );
    }
    Ok(())
}

/// Every document-type claim must be ranked `Alternate` — Stratum never steals
/// a double-click from Stata (design 08 §6.3) — and the Stata UTIs must be
/// imported, never exported.
fn check_info_additions(raw: &str) -> Result<()> {
    let text = strip_xml_comments(raw);
    let claims = text.matches("<key>LSItemContentTypes</key>").count();
    let alternates = text.matches("<string>Alternate</string>").count();
    ensure!(claims >= 2, "expected .do and .dta document-type claims");
    ensure!(
        alternates >= claims,
        "every CFBundleDocumentTypes entry must carry LSHandlerRank = Alternate \
         ({claims} claims, {alternates} Alternate ranks)"
    );
    for banned in ["<string>Owner</string>", "<string>Default</string>"] {
        ensure!(
            !text.contains(banned),
            "LSHandlerRank {banned} would steal the default handler from Stata"
        );
    }
    ensure!(
        !text.contains("UTExportedTypeDeclarations"),
        "Stata owns com.stata.stata.*; export a conflicting UTI declaration and \
         Launch Services resolution becomes nondeterministic — import only"
    );
    ensure!(
        text.contains("com.stata.stata.do") && text.contains("com.stata.stata.dta"),
        "the .do and .dta claims must use Stata's own UTIs"
    );
    Ok(())
}

fn check_platform_conf(os: &str, conf: &Value) -> Result<()> {
    let bundle = conf.get("bundle").context(
        "per-OS config must define `bundle` (the base config ships bundle.active=false)",
    )?;
    ensure!(
        bundle.get("active") == Some(&Value::Bool(true)),
        "bundle.active must be true"
    );
    let targets: Vec<&str> = bundle
        .get("targets")
        .and_then(Value::as_array)
        .context("bundle.targets must be an array")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let expected: &[&str] = match os {
        "macos" => &["app", "dmg"],
        "windows" => &["nsis", "msi"],
        "linux" => &["deb", "rpm", "appimage"],
        _ => bail!("unknown platform {os}"),
    };
    ensure!(
        targets == expected,
        "bundle.targets for {os} must be {expected:?}, found {targets:?}"
    );

    // A21 stays checkable: the CSP lives in W17's base tauri.conf.json and a
    // platform file must not override it, or `xtask csp-check` over the base
    // config would be checking a policy the build does not ship.
    ensure!(
        conf.pointer("/app/security/csp").is_none(),
        "per-OS config must not override app.security.csp (ARCHITECTURE §8.12 / A21)"
    );

    // File associations are never declared through Tauri's generic
    // `fileAssociations`: its NSIS/WiX/Info.plist output registers the app as
    // the DEFAULT handler, which is exactly the theft design 08 §6.3 forbids.
    // macOS uses the merged Info.plist (rank Alternate), Windows the installer
    // hook / WiX fragment (OpenWithProgids only), Linux additive MIME files.
    ensure!(
        bundle.get("fileAssociations").is_none(),
        "use the per-OS never-steal mechanisms, not bundle.fileAssociations"
    );

    ensure!(
        bundle.pointer("/externalBin/0") == Some(&Value::String("binaries/stratum".into())),
        "the stratum CLI must ride as bundle.externalBin[0] = \"binaries/stratum\" \
         (the packaged host looks for `stratum` next to its own executable)"
    );

    // The ado tree rides as a resource: `dist stage` puts it at binaries/ado,
    // the mapping lands it at <resource dir>/ado, and the desktop host points
    // the engine child's STRATUM_ADO_BASE at <resource dir>/ado/base. Losing
    // the mapping breaks `sysuse auto` only in PACKAGED builds — the one
    // configuration no workspace test runs — hence a static gate.
    ensure!(
        bundle
            .get("resources")
            .and_then(|r| r.get(format!("binaries/ado/{ADO_AUTO_DTA}")))
            == Some(&Value::String(format!("ado/{ADO_AUTO_DTA}"))),
        "bundle.resources must map binaries/ado/{ADO_AUTO_DTA} -> ado/{ADO_AUTO_DTA} \
         (staged by `dist stage`; sysuse's tree in the packaged app)"
    );

    if os == "macos" {
        ensure!(
            bundle.pointer("/macOS/entitlements")
                == Some(&Value::String("entitlements.plist".into())),
            "macOS bundle must sign with the committed entitlements.plist"
        );
        ensure!(
            bundle.pointer("/macOS/hardenedRuntime") == Some(&Value::Bool(true)),
            "hardened runtime is a notarization precondition; keep it on even ad-hoc"
        );
    }
    Ok(())
}

/// The NSIS hook may add `OpenWithProgids` values but must never write the
/// `(Default)` value of an extension key — that is the never-steal contract in
/// executable form, and smoke.yml asserts its runtime effect after install.
fn check_nsis_hook(text: &str) -> Result<()> {
    ensure!(
        text.contains("NSIS_HOOK_POSTINSTALL") && text.contains("NSIS_HOOK_POSTUNINSTALL"),
        "file-assoc.nsh must define the install and uninstall hook macros"
    );
    ensure!(
        text.contains("OpenWithProgids"),
        "associations must be offered via OpenWithProgids"
    );
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with(';') || l.is_empty() {
            continue;
        }
        // A write to `...\Classes\.do` (or `.dta`) with an empty value name is
        // a write to the extension's (Default) — the theft.
        if l.starts_with("WriteRegStr") && (l.contains("\\.do\"") || l.contains("\\.dta\"")) {
            bail!("file-assoc.nsh writes the (Default) value of an extension key: `{l}`");
        }
    }
    Ok(())
}

fn verify(app: &Utf8Path, universal: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("dist verify inspects a .app bundle and runs on macOS only");
    }
    ensure!(app.is_dir(), "{app} is not a directory");
    let plist = app.join("Contents/Info.plist");

    // The bundler may emit binary plists; go through plutil.
    let out = std::process::Command::new("plutil")
        .args(["-convert", "xml1", "-o", "-", plist.as_str()])
        .output()
        .context("running plutil")?;
    ensure!(out.status.success(), "plutil failed on {plist}");
    let xml = String::from_utf8_lossy(&out.stdout).into_owned();
    check_info_additions(&xml)
        .context("the built Info.plist lost the file-association contract")?;

    // The signed binary's entitlements must match the committed policy.
    let ent = std::process::Command::new("codesign")
        .args(["-d", "--entitlements", ":-", app.as_str()])
        .output()
        .context("running codesign -d --entitlements")?;
    ensure!(
        ent.status.success(),
        "codesign could not read entitlements from {app}"
    );
    let ent_text = String::from_utf8_lossy(&ent.stdout).into_owned();
    ensure!(
        ent_text.contains("allow-jit"),
        "built app is missing the allow-jit entitlement"
    );
    ensure!(
        !ent_text.contains("disable-library-validation"),
        "built app carries disable-library-validation (ADR-011 violation)"
    );

    if universal {
        let exe = app.join("Contents/MacOS/stratum-desktop");
        let lipo = std::process::Command::new("lipo")
            .args(["-archs", exe.as_str()])
            .output()
            .context("running lipo -archs")?;
        ensure!(lipo.status.success(), "lipo failed on {exe}");
        let archs = String::from_utf8_lossy(&lipo.stdout).into_owned();
        ensure!(
            archs.contains("x86_64") && archs.contains("arm64"),
            "expected a universal binary, lipo reports: {}",
            archs.trim()
        );
    }
    println!("dist verify: OK ({app})");
    Ok(())
}

// --- cross-check -----------------------------------------------------------
//
// `cargo check --target T` runs every build script in the graph for T, and some
// of them compile C. What a non-T host can do about that differs per triple:
//
// * x86_64-unknown-linux-gnu from macOS: cc-rs probes PATH for a tool named
//   `x86_64-linux-gnu-gcc`; `scripts/dev-setup.sh --cross` installs zig-backed
//   shims under that name (packaging/cross/), zig bundles glibc headers, and
//   every C build script in the graph (aws-lc-sys, blake3, mimalloc) compiles
//   in full. Nothing else is needed, so the check is FULL.
// * x86_64-pc-windows-msvc from anywhere but Windows: two distinct obstacles.
//   (1) blake3 assumes MASM (`ml64.exe`) whenever it cross-compiles to msvc
//   with no `CC_x86_64_pc_windows_msvc` set; naming `clang` makes it take the
//   GNU-syntax `.S` twins of its Windows assembly, which clang's integrated
//   assembler emits as COFF without touching a header, and any LLVM `lib`/`ar`
//   driver archives the result. (2) mimalloc and aws-lc-sys `#include` the
//   Windows CRT/SDK headers (`wchar.h`, `windows.h`, …) — only a Windows host
//   has those, or a host that opted in to cargo-xwin downloading them under
//   Microsoft's license (packaging/README.md). So the crates reaching (2) are
//   skipped, named, and the run is PARTIAL; CI builds this triple natively on
//   windows-2022 (package.yml, smoke.yml), which is the gate that matters.

const LINUX_GNU: &str = "x86_64-unknown-linux-gnu";
const WINDOWS_MSVC: &str = "x86_64-pc-windows-msvc";

/// C-compiling crates whose sources include the Windows CRT / SDK headers.
/// blake3 is deliberately NOT here (see the module comment); add a crate only
/// once it has actually failed for that reason, and say so in the commit.
const NEEDS_WINDOWS_SDK: &[&str] = &["aws-lc-sys", "libmimalloc-sys"];

/// What one `cargo check --target` invocation will be.
#[derive(Debug, PartialEq, Eq)]
struct CrossPlan {
    triple: String,
    /// Workspace packages to pass as `-p`, in name order.
    members: Vec<String>,
    /// (package, why) left out of this run.
    skipped: Vec<(String, String)>,
    /// Environment for the build scripts (cc-rs reads these).
    env: Vec<(&'static str, String)>,
    /// Workspace members a bare `cargo check` does not build either (not in
    /// `default-members`), named so nobody reads "OK" as covering them. The
    /// desktop host is the one that matters: it needs the target OS's own
    /// system libraries (GTK/WebKitGTK through pkg-config; the Windows CRT)
    /// and is checked natively per OS in CI.
    out_of_scope: Vec<String>,
}

impl CrossPlan {
    fn is_partial(&self) -> bool {
        !self.skipped.is_empty()
    }
}

fn cross_check(ctx: &Ctx, targets: &[String], strict: bool) -> Result<()> {
    let host = host_triple()?;
    let targets: Vec<String> = if targets.is_empty() {
        vec![LINUX_GNU.to_owned(), WINDOWS_MSVC.to_owned()]
    } else {
        targets.to_vec()
    };

    let mut summary: Vec<String> = Vec::new();
    let mut any_partial = false;
    for triple in &targets {
        ensure_rust_target_installed(triple)?;
        let md = cross_metadata(ctx, triple)?;
        let plan = plan_cross_check(triple, &host, &md, &on_path)?;

        println!("dist cross-check: {triple} from {host}");
        for (k, v) in &plan.env {
            println!("  env {k}={v}");
        }
        for (name, why) in &plan.skipped {
            println!("  skip {name}: {why}");
        }
        if !plan.out_of_scope.is_empty() {
            println!(
                "  not in scope (not a default member; native per-OS builds cover these): {}",
                plan.out_of_scope.join(", ")
            );
        }
        println!(
            "  checking {} of {} default members",
            plan.members.len(),
            plan.members.len() + plan.skipped.len()
        );

        let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
        cargo
            .current_dir(&ctx.root)
            .args(["check", "--locked", "--target", triple]);
        for m in &plan.members {
            cargo.args(["-p", m]);
        }
        for (k, v) in &plan.env {
            cargo.env(k, v);
        }
        let status = cargo
            .status()
            .with_context(|| format!("running cargo check --target {triple}"))?;
        ensure!(
            status.success(),
            "cargo check --target {triple} failed ({status})"
        );

        if plan.is_partial() {
            any_partial = true;
            summary.push(format!(
                "{triple}: OK, PARTIAL ({} of {} packages skipped)",
                plan.skipped.len(),
                plan.members.len() + plan.skipped.len()
            ));
        } else {
            summary.push(format!("{triple}: OK, full"));
        }
    }

    println!("dist cross-check:");
    for line in &summary {
        println!("  {line}");
    }
    if any_partial {
        println!(
            "  PARTIAL means this host cannot compile the Windows CRT/SDK-dependent C in \
             the skipped crates. Full coverage: a Windows host (CI: package.yml, smoke.yml) \
             or an explicit opt-in to cargo-xwin — see packaging/README.md."
        );
        ensure!(
            !strict,
            "--strict: at least one triple could only be checked partially from this host"
        );
    }
    Ok(())
}

/// `cargo metadata` resolved for `triple`, so `cfg(windows)`-only edges are
/// present for the msvc plan and absent for the linux one.
fn cross_metadata(ctx: &Ctx, triple: &str) -> Result<Metadata> {
    let mut cmd = MetadataCommand::new();
    cmd.manifest_path(ctx.path("Cargo.toml"));
    cmd.other_options(vec![
        "--locked".to_owned(),
        "--filter-platform".to_owned(),
        triple.to_owned(),
    ]);
    cmd.exec()
        .with_context(|| format!("cargo metadata --filter-platform {triple}"))
}

/// The plan for one triple. `on_path` answers "is this program on PATH?" and is
/// injected so the decision table is unit-testable without a toolchain.
fn plan_cross_check(
    triple: &str,
    host: &str,
    md: &Metadata,
    on_path: &dyn Fn(&str) -> bool,
) -> Result<CrossPlan> {
    let mut env: Vec<(&'static str, String)> = Vec::new();
    let mut members: Vec<String> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();

    let candidates: Vec<&PackageId> = if md.workspace_default_members.is_available() {
        md.workspace_default_members.iter().collect()
    } else {
        md.workspace_members.iter().collect()
    };
    let mut out_of_scope: Vec<String> = md
        .workspace_members
        .iter()
        .filter(|id| !candidates.contains(id))
        .map(|id| md[id].name.to_string())
        .collect();
    out_of_scope.sort();

    if triple == WINDOWS_MSVC && !host.contains("windows") {
        // (1) blake3 — see the module comment. clang is the only C front end
        // that assembles for COFF without an MSVC installation.
        ensure!(
            on_path("clang"),
            "cross-checking {WINDOWS_MSVC} from {host} needs `clang` on PATH (it is the C \
             front end that assembles blake3's Windows code for COFF; Xcode's command-line \
             tools ship it, Linux distros package it as `clang`)"
        );
        env.push(("CC_x86_64_pc_windows_msvc", "clang".to_owned()));
        // cc-rs archives msvc objects with lib.exe unless told otherwise; any
        // LLVM lib/ar driver does the job, and zig carries both.
        let archiver = ["llvm-lib", "llvm-ar"]
            .into_iter()
            .find(|t| on_path(t))
            .map(str::to_owned)
            .or_else(|| on_path("zig").then(|| "zig lib".to_owned()))
            .with_context(|| {
                format!(
                    "cross-checking {WINDOWS_MSVC} from {host} needs an LLVM archiver on PATH: \
                     llvm-lib, llvm-ar, or zig (`scripts/dev-setup.sh --cross` installs zig)"
                )
            })?;
        env.push(("AR_x86_64_pc_windows_msvc", archiver));

        // (2) the crates whose C needs the Windows CRT/SDK headers.
        for id in candidates {
            let name = md[id].name.to_string();
            match reaches_any(md, id, NEEDS_WINDOWS_SDK) {
                Some(via) => skipped.push((
                    name,
                    format!(
                        "reaches {via}, whose C sources need the Windows CRT/SDK headers \
                         this host does not have"
                    ),
                )),
                None => members.push(name),
            }
        }
    } else {
        if triple == LINUX_GNU && host != LINUX_GNU {
            // cc-rs probes for this prefix itself; no env needed, only the tool.
            ensure!(
                on_path("x86_64-linux-gnu-gcc") || on_path("x86_64-unknown-linux-gnu-gcc"),
                "cross-checking {LINUX_GNU} from {host} needs a C compiler named \
                 `x86_64-linux-gnu-gcc` on PATH: macOS — `scripts/dev-setup.sh --cross` \
                 (zig-backed shims, packaging/cross/); Debian/Ubuntu — \
                 `apt install gcc-x86-64-linux-gnu`"
            );
        }
        members.extend(candidates.into_iter().map(|id| md[id].name.to_string()));
    }

    members.sort();
    skipped.sort();
    Ok(CrossPlan {
        triple: triple.to_owned(),
        members,
        skipped,
        env,
        out_of_scope,
    })
}

/// Does `from`'s normal+build dependency closure (what `cargo check` compiles;
/// dev-dependencies are not) contain a package named in `names`? Returns the
/// first such name found.
fn reaches_any(md: &Metadata, from: &PackageId, names: &[&str]) -> Option<String> {
    let resolve = md.resolve.as_ref()?;
    let nodes: BTreeMap<&PackageId, &cargo_metadata::Node> =
        resolve.nodes.iter().map(|n| (&n.id, n)).collect();
    let mut seen: BTreeSet<&PackageId> = BTreeSet::new();
    let mut queue: VecDeque<&PackageId> = VecDeque::new();
    seen.insert(from);
    queue.push_back(from);
    while let Some(cur) = queue.pop_front() {
        let Some(node) = nodes.get(cur) else { continue };
        for dep in &node.deps {
            let compiled_by_check = dep.dep_kinds.is_empty()
                || dep
                    .dep_kinds
                    .iter()
                    .any(|k| matches!(k.kind, DependencyKind::Normal | DependencyKind::Build));
            if !compiled_by_check || !seen.insert(&dep.pkg) {
                continue;
            }
            let name = md[&dep.pkg].name.as_str();
            if names.contains(&name) {
                return Some(name.to_owned());
            }
            queue.push_back(&dep.pkg);
        }
    }
    None
}

/// `rustup target list --installed` contains `triple`. A machine without
/// rustup (a distro toolchain) skips the check and lets cargo speak for itself.
fn ensure_rust_target_installed(triple: &str) -> Result<()> {
    let Ok(out) = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
    else {
        return Ok(());
    };
    if !out.status.success() {
        return Ok(());
    }
    let installed = String::from_utf8_lossy(&out.stdout);
    ensure!(
        installed.lines().any(|l| l.trim() == triple),
        "rust target {triple} is not installed: `rustup target add {triple}` \
         (`scripts/dev-setup.sh --cross` adds both cross-check triples)"
    );
    Ok(())
}

/// Is an executable named `name` on PATH?
fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(name);
        let exe = if cfg!(windows) {
            candidate.is_file() || candidate.with_extension("exe").is_file()
        } else {
            candidate.is_file()
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            exe && std::fs::metadata(&candidate)
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            exe
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> Utf8PathBuf {
        Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a parent")
            .to_path_buf()
    }

    fn read_repo(rel: &str) -> String {
        let p = repo_root().join(rel);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("reading {p}: {e}"))
    }

    /// The W22 acceptance's grep test, verbatim: `entitlements.plist` does NOT
    /// contain `disable-library-validation` (ARCHITECTURE C27).
    #[test]
    fn entitlements_never_disable_library_validation() {
        let text = read_repo("apps/desktop/src-tauri/entitlements.plist");
        assert!(
            !strip_xml_comments(&text).contains("disable-library-validation"),
            "ADR-011: nothing in v1 dlopens a third-party binary"
        );
        check_entitlements(&text).unwrap();
    }

    #[test]
    fn info_additions_rank_is_alternate_everywhere() {
        check_info_additions(&read_repo("packaging/macos/Info.additions.plist")).unwrap();
    }

    #[test]
    fn platform_configs_hold_the_bundle_invariants() {
        for os in ["macos", "windows", "linux"] {
            let text = read_repo(&format!("apps/desktop/src-tauri/tauri.{os}.conf.json"));
            let conf: Value = serde_json::from_str(&text).expect("valid JSON");
            check_platform_conf(os, &conf).unwrap_or_else(|e| panic!("{os}: {e:#}"));
        }
    }

    /// The shipped tree's auto.dta is the committed fixture, byte for byte —
    /// the same assertion `dist check` runs, over the same two files.
    #[test]
    fn the_shipped_ado_auto_dta_is_the_fixture_byte_for_byte() {
        let read_bytes = |rel: &str| {
            let p = repo_root().join(rel);
            std::fs::read(&p).unwrap_or_else(|e| panic!("reading {p}: {e}"))
        };
        check_ado_auto_dta(
            &read_bytes("packaging/ado/base/a/auto.dta"),
            &read_bytes("tests/fixtures/dta/auto.dta"),
        )
        .unwrap();
    }

    /// `dist stage` stages the whole committed ado tree, preserving the
    /// `base/<first-letter>/<name>.dta` shape sysuse resolves, bytes intact.
    #[test]
    fn stage_ado_tree_stages_the_committed_tree() {
        let td = tempfile::tempdir().expect("tempdir");
        let dest = Utf8PathBuf::from_path_buf(td.path().join("ado")).expect("utf-8 tempdir");
        let src = repo_root().join("packaging/ado");
        let staged = stage_ado_tree(&src, &dest).unwrap();
        assert!(
            staged.iter().any(|p| p == ADO_AUTO_DTA),
            "staged listing must contain {ADO_AUTO_DTA}, got {staged:?}"
        );
        for rel in &staged {
            let out = std::fs::read(dest.join(rel)).expect("staged file readable");
            let orig = std::fs::read(src.join(rel)).expect("source file readable");
            assert_eq!(out, orig, "{rel} changed in staging");
        }
    }

    /// A tree without the file `sysuse auto` loads is refused, with the
    /// missing path named.
    #[test]
    fn an_ado_tree_without_auto_dta_is_refused() {
        let td = tempfile::tempdir().expect("tempdir");
        let dir = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("utf-8 tempdir");
        let src = dir.join("ado");
        std::fs::create_dir_all(src.join("base/b")).unwrap();
        std::fs::write(src.join("base/b/bplong.dta"), b"not auto").unwrap();
        let err = stage_ado_tree(&src, &dir.join("out")).unwrap_err();
        assert!(err.to_string().contains(ADO_AUTO_DTA), "{err}");
    }

    #[test]
    fn a_drifted_shipped_auto_dta_is_caught() {
        assert!(check_ado_auto_dta(b"drifted", b"fixture").is_err());
    }

    #[test]
    fn nsis_hook_never_writes_an_extension_default() {
        check_nsis_hook(&read_repo("packaging/windows/file-assoc.nsh")).unwrap();
    }

    #[test]
    fn a_stolen_default_is_caught() {
        let hostile = "!macro NSIS_HOOK_POSTINSTALL\n\
             WriteRegStr SHCTX \"Software\\Classes\\.do\" \"\" \"Stratum.DoFile\"\n\
             !macroend\n!macro NSIS_HOOK_POSTUNINSTALL\n!macroend\n\
             ; OpenWithProgids mentioned so only the theft check can fire";
        assert!(check_nsis_hook(hostile).is_err());
    }

    #[test]
    fn a_snuck_in_library_validation_escape_is_caught() {
        let hostile = "<key>com.apple.security.cs.allow-jit</key><true/>\
             <key>com.apple.security.cs.disable-library-validation</key><true/>";
        assert!(check_entitlements(hostile).is_err());
    }

    // --- cross-check -------------------------------------------------------
    //
    // A synthetic workspace in a tempdir, resolved with `cargo metadata
    // --offline` (path dependencies only, so no registry and no network). The
    // two SDK-needing crates are stand-ins that merely carry the real names:
    // the plan keys on names, which is exactly what is under test.
    //
    //   app      --normal--> libmimalloc-sys        (skipped for msvc)
    //   tls      --build---> aws-lc-sys             (skipped: build edges count)
    //   devonly  --dev-----> libmimalloc-sys        (kept: dev-deps are not checked)
    //   lib                                          (kept)
    //   extra    --normal--> aws-lc-sys              (not a default member: ignored)
    fn synthetic_workspace() -> (tempfile::TempDir, Metadata) {
        let td = tempfile::tempdir().expect("tempdir");
        let dir = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("utf-8 tempdir");
        let crates: &[(&str, &str)] = &[
            ("libmimalloc-sys", ""),
            ("aws-lc-sys", ""),
            ("lib", ""),
            (
                "app",
                "[dependencies]\nlibmimalloc-sys = { path = \"../libmimalloc-sys\" }\n",
            ),
            (
                "tls",
                "[build-dependencies]\naws-lc-sys = { path = \"../aws-lc-sys\" }\n",
            ),
            (
                "devonly",
                "[dev-dependencies]\nlibmimalloc-sys = { path = \"../libmimalloc-sys\" }\n",
            ),
            (
                "extra",
                "[dependencies]\naws-lc-sys = { path = \"../aws-lc-sys\" }\n",
            ),
        ];
        let members: Vec<String> = crates.iter().map(|(n, _)| format!("\"{n}\"")).collect();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[workspace]\nresolver = \"2\"\nmembers = [{}]\n\
                 default-members = [\"app\", \"tls\", \"devonly\", \"lib\"]\n",
                members.join(", ")
            ),
        )
        .unwrap();
        for (name, deps) in crates {
            let crate_dir = dir.join(name);
            std::fs::create_dir_all(crate_dir.join("src")).unwrap();
            std::fs::write(
                crate_dir.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n{deps}"
                ),
            )
            .unwrap();
            std::fs::write(crate_dir.join("src/lib.rs"), "").unwrap();
        }
        let mut cmd = MetadataCommand::new();
        cmd.manifest_path(dir.join("Cargo.toml"));
        cmd.current_dir(&dir);
        cmd.other_options(vec![
            "--offline".to_owned(),
            "--filter-platform".to_owned(),
            WINDOWS_MSVC.to_owned(),
        ]);
        let md = cmd
            .exec()
            .expect("cargo metadata on the synthetic workspace");
        (td, md)
    }

    const MAC: &str = "aarch64-apple-darwin";

    fn names(v: &[(String, String)]) -> Vec<&str> {
        v.iter().map(|(n, _)| n.as_str()).collect()
    }

    #[test]
    fn msvc_from_a_mac_skips_exactly_the_sdk_crates_and_names_the_culprit() {
        let (_td, md) = synthetic_workspace();
        let plan =
            plan_cross_check(WINDOWS_MSVC, MAC, &md, &|t| t == "clang" || t == "zig").unwrap();
        assert_eq!(plan.members, ["devonly", "lib"]);
        assert_eq!(names(&plan.skipped), ["app", "tls"]);
        assert!(
            plan.skipped[0].1.contains("libmimalloc-sys"),
            "{:?}",
            plan.skipped[0]
        );
        assert!(
            plan.skipped[1].1.contains("aws-lc-sys"),
            "{:?}",
            plan.skipped[1]
        );
        assert_eq!(
            plan.env,
            [
                ("CC_x86_64_pc_windows_msvc", "clang".to_owned()),
                ("AR_x86_64_pc_windows_msvc", "zig lib".to_owned()),
            ]
        );
        assert!(plan.is_partial());
        assert_eq!(
            plan.out_of_scope,
            ["aws-lc-sys", "extra", "libmimalloc-sys"],
            "non-default members are named, never silently dropped"
        );
    }

    #[test]
    fn msvc_archiver_prefers_a_real_llvm_driver_over_zig() {
        let (_td, md) = synthetic_workspace();
        let all = |t: &str| ["clang", "llvm-lib", "llvm-ar", "zig"].contains(&t);
        let plan = plan_cross_check(WINDOWS_MSVC, MAC, &md, &all).unwrap();
        assert_eq!(plan.env[1].1, "llvm-lib");
        let no_lib = |t: &str| ["clang", "llvm-ar", "zig"].contains(&t);
        let plan = plan_cross_check(WINDOWS_MSVC, MAC, &md, &no_lib).unwrap();
        assert_eq!(plan.env[1].1, "llvm-ar");
    }

    #[test]
    fn msvc_without_the_tools_says_which_one_to_install() {
        let (_td, md) = synthetic_workspace();
        let err = plan_cross_check(WINDOWS_MSVC, MAC, &md, &|_| false).unwrap_err();
        assert!(err.to_string().contains("`clang`"), "{err}");
        let err = plan_cross_check(WINDOWS_MSVC, MAC, &md, &|t| t == "clang").unwrap_err();
        assert!(err.to_string().contains("zig"), "{err}");
    }

    #[test]
    fn msvc_on_a_windows_host_is_plain_and_full() {
        let (_td, md) = synthetic_workspace();
        let plan = plan_cross_check(WINDOWS_MSVC, WINDOWS_MSVC, &md, &|_| false).unwrap();
        assert!(plan.env.is_empty());
        assert!(!plan.is_partial());
        assert_eq!(plan.members, ["app", "devonly", "lib", "tls"]);
    }

    #[test]
    fn linux_from_a_mac_needs_the_prefixed_gcc_and_nothing_else() {
        let (_td, md) = synthetic_workspace();
        let plan = plan_cross_check(LINUX_GNU, MAC, &md, &|t| t == "x86_64-linux-gnu-gcc").unwrap();
        assert!(plan.env.is_empty());
        assert!(!plan.is_partial());
        assert_eq!(plan.members, ["app", "devonly", "lib", "tls"]);

        let err = plan_cross_check(LINUX_GNU, MAC, &md, &|_| false).unwrap_err();
        assert!(err.to_string().contains("dev-setup.sh --cross"), "{err}");
    }

    #[test]
    fn linux_on_linux_is_native() {
        let (_td, md) = synthetic_workspace();
        let plan = plan_cross_check(LINUX_GNU, LINUX_GNU, &md, &|_| false).unwrap();
        assert!(plan.env.is_empty() && !plan.is_partial());
    }

    #[test]
    fn reachability_follows_normal_and_build_edges_but_never_dev() {
        let (_td, md) = synthetic_workspace();
        let id = |name: &str| {
            md.packages
                .iter()
                .find(|p| p.name.as_str() == name)
                .map(|p| p.id.clone())
                .unwrap()
        };
        assert_eq!(
            reaches_any(&md, &id("app"), NEEDS_WINDOWS_SDK).as_deref(),
            Some("libmimalloc-sys")
        );
        assert_eq!(
            reaches_any(&md, &id("tls"), NEEDS_WINDOWS_SDK).as_deref(),
            Some("aws-lc-sys")
        );
        assert_eq!(reaches_any(&md, &id("devonly"), NEEDS_WINDOWS_SDK), None);
        assert_eq!(reaches_any(&md, &id("lib"), NEEDS_WINDOWS_SDK), None);
    }
}
