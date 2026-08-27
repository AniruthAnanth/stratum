//! IMPLEMENTATION_PLAN W22 / ADR-011 — signature verification, and an honest
//! report of which signing credentials a build actually has.
//!
//! The signing itself belongs to the tools that own it (Tauri's bundler signs
//! macOS inside-out; `signCommand` drives Azure Trusted Signing on Windows).
//! What lives here is the part CI and a contributor both need to *check*:
//!
//! - `status` — which signing secret families are present in the environment.
//!   `package.yml` uses this to decide, per ADR-011 §7.3, whether it is in the
//!   signed or the unsigned era; either path must produce launchable bundles.
//! - `verify-app` / `verify-dmg` — the hard gates from the W22 acceptance:
//!   `codesign --verify --strict`, `spctl -a` (type `exec` for the .app; type
//!   `open --context context:primary-signature` for the disk image — Apple's
//!   documented assessment per artifact kind), `xcrun stapler validate`. For an
//!   ad-hoc build the Gatekeeper-facing checks are reported as the documented
//!   consequence, not silently skipped: an unnotarized quarantined download
//!   WILL show the "damaged" dialog.
//!
//! The two macOS artifacts are NOT in the same state in the unsigned era, and
//! the gates say so rather than pretending otherwise. With
//! `APPLE_SIGNING_IDENTITY=-` the bundler ad-hoc signs the `.app` (with the
//! committed entitlements) but deliberately leaves the `.dmg` unsigned:
//! tauri-bundler 2.2.3 (PR #12323) skips disk-image signing for the "-"
//! identity because an ad-hoc-signed image is refused by Gatekeeper at mount
//! on macOS 15 — before the user ever reaches the app, and right-click → Open
//! does not help (issue #12288) — whereas an unsigned image mounts and only the
//! app inside meets Gatekeeper. So `verify-dmg` accepts "not signed at all" as
//! the designed state of that era (and still checks the image's own checksum),
//! and refuses an ad-hoc-signed image in BOTH eras: it is the one state that is
//! never right.

use anyhow::{bail, ensure, Context, Result};
use camino::Utf8Path;
use clap::{Args, Subcommand};

use crate::Ctx;

#[derive(Args)]
pub struct Cmd {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Report which signing secret families exist in this environment.
    Status {
        /// Also append `<family>=true|false` lines to `$GITHUB_OUTPUT`.
        #[arg(long)]
        github_output: bool,
    },
    /// Verify a built .app's signature; strict on signed builds, honest on
    /// ad-hoc ones.
    VerifyApp {
        #[arg(value_name = "APP")]
        app: camino::Utf8PathBuf,
        /// Fail unless the app is Developer ID signed AND accepted by
        /// Gatekeeper (the release gate; ad-hoc becomes an error).
        #[arg(long)]
        require_notarized: bool,
    },
    /// Verify a .dmg: image checksum always; Developer ID signature, Gatekeeper
    /// and the stapled ticket when signed; unsigned is the designed state of
    /// the unsigned era (an ad-hoc-signed image is refused in every era).
    VerifyDmg {
        #[arg(value_name = "DMG")]
        dmg: camino::Utf8PathBuf,
        /// Fail unless the image is Developer ID signed, Gatekeeper-accepted
        /// and stapled (the release gate; unsigned becomes an error).
        #[arg(long)]
        require_notarized: bool,
    },
}

pub fn run(_ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    match &cmd.action {
        Action::Status { github_output } => status(*github_output),
        Action::VerifyApp {
            app,
            require_notarized,
        } => verify_app(app, *require_notarized),
        Action::VerifyDmg {
            dmg,
            require_notarized,
        } => verify_dmg(dmg, *require_notarized),
    }
}

/// The secret families ADR-011 §§7.1–7.4 define. A family counts as present
/// only when EVERY variable in it is set — a partial family is a
/// misconfiguration that would fail mid-pipeline, so it is called out.
const FAMILIES: &[(&str, &[&str])] = &[
    (
        "apple_codesign",
        &[
            "APPLE_CERTIFICATE",
            "APPLE_CERTIFICATE_PASSWORD",
            "APPLE_SIGNING_IDENTITY",
            "APPLE_TEAM_ID",
            "KEYCHAIN_PASSWORD",
        ],
    ),
    (
        "apple_notarize",
        &["APPLE_API_KEY_ID", "APPLE_API_ISSUER", "APPLE_API_KEY"],
    ),
    (
        "windows_trusted_signing",
        &[
            "AZURE_TENANT_ID",
            "AZURE_CLIENT_ID",
            "AZURE_CLIENT_SECRET",
            "AZURE_TS_ENDPOINT",
            "AZURE_TS_ACCOUNT",
            "AZURE_TS_CERT_PROFILE",
        ],
    ),
    (
        "linux_gpg",
        &["LINUX_GPG_PRIVATE_KEY", "LINUX_GPG_PASSPHRASE"],
    ),
    (
        "tauri_updater",
        &[
            "TAURI_SIGNING_PRIVATE_KEY",
            "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
        ],
    ),
];

fn status(github_output: bool) -> Result<()> {
    let mut lines = Vec::new();
    for (family, vars) in FAMILIES {
        let set: Vec<&str> = vars
            .iter()
            .copied()
            .filter(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty()))
            .collect();
        let present = set.len() == vars.len();
        if !set.is_empty() && !present {
            let missing: Vec<&str> = vars.iter().copied().filter(|v| !set.contains(v)).collect();
            eprintln!(
                "sign status: WARNING — {family} is PARTIAL (missing {missing:?}); \
                 the pipeline treats it as absent"
            );
        }
        println!("{family}={present}");
        lines.push(format!("{family}={present}"));
    }
    if github_output {
        let path = std::env::var("GITHUB_OUTPUT").context("--github-output outside Actions")?;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .with_context(|| format!("opening {path}"))?;
        for l in &lines {
            writeln!(f, "{l}")?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// verify-app / verify-dmg

enum GateKind {
    App,
    Dmg,
}

/// ADR-011 §7.3, the consequence we print instead of hiding: an ad-hoc build
/// is fine to run locally and via `tar.gz`, but a browser download of it is
/// quarantined and Gatekeeper shows the misleading "damaged" dialog.
const ADHOC_CONSEQUENCE: &str = "sign: AD-HOC signature (no Developer ID in this environment).\n\
     Gatekeeper consequence for a quarantined (browser-downloaded) copy:\n\
     \"Stratum is damaged and can't be opened.\" Recovery: System Settings →\n\
     Privacy & Security → Open Anyway, or scripts/macos-unquarantine.sh.\n\
     curl/tar downloads carry no quarantine and launch normally (ADR-011 §7.3).";

/// Why an unsigned `.dmg` is the designed state of the unsigned era, not an
/// omission. Printed so nobody "fixes" it by ad-hoc signing the image.
const UNSIGNED_DMG_REASON: &str =
    "sign: UNSIGNED disk image (no Developer ID in this environment).\n\
     Designed, not forgotten: Tauri's bundler skips disk-image signing for the\n\
     ad-hoc identity \"-\" (tauri-bundler 2.2.3, PR #12323) because Gatekeeper\n\
     refuses an ad-hoc-signed .dmg at mount on macOS 15 — before the user reaches\n\
     the app, and right-click → Open does not help (issue #12288) — whereas an\n\
     unsigned image mounts and only the .app inside meets Gatekeeper. That app's\n\
     consequence, the same one `verify-app` prints, is the one that applies:";

/// Why an ad-hoc-signed `.dmg` is refused in every era. Only a Developer ID
/// signature or no signature at all is a shippable disk image.
const ADHOC_DMG_REFUSAL: &str =
    "is AD-HOC signed. A disk image must be Developer ID signed or not \
     signed at all: Gatekeeper refuses an ad-hoc-signed .dmg at mount on macOS 15, even via \
     right-click → Open (Tauri issue #12288), which is why the bundler leaves the image unsigned \
     for identity \"-\" (tauri-bundler 2.2.3, PR #12323). Rebuild without signing the image, or \
     sign it with a Developer ID.";

/// What `codesign -dvv` says about who sealed a code object.
#[derive(Debug, PartialEq, Eq)]
enum Signature {
    /// "code object is not signed at all".
    None,
    /// `Signature=adhoc` — sealed, but by no identity.
    AdHoc,
    /// Sealed by an identity; carries the first `Authority=` line for the report.
    Identity(String),
}

/// Classify `codesign -dvv` output. `ok` is the process exit status; `stderr`
/// is where codesign writes the description (and the "not signed" verdict).
fn classify_signature(ok: bool, stderr: &str) -> Result<Signature> {
    if stderr.contains("code object is not signed at all") {
        return Ok(Signature::None);
    }
    ensure!(ok, "codesign -dvv failed:\n{stderr}");
    if stderr.contains("Signature=adhoc") {
        return Ok(Signature::AdHoc);
    }
    let authority = stderr
        .lines()
        .find_map(|l| l.strip_prefix("Authority="))
        .unwrap_or("(no Authority line)")
        .trim()
        .to_owned();
    Ok(Signature::Identity(authority))
}

fn signature_of(path: &Utf8Path) -> Result<Signature> {
    let detail = std::process::Command::new("codesign")
        .args(["-dvv", path.as_str()])
        .output()
        .context("running codesign -dvv")?;
    classify_signature(
        detail.status.success(),
        &String::from_utf8_lossy(&detail.stderr),
    )
}

/// Structural validity. `--strict --deep` catches a nested binary someone
/// forgot to sign (the externalBin) and a seal that no longer matches the
/// bytes; it does not care who signed.
fn codesign_verify_strict(path: &Utf8Path) -> Result<()> {
    let verify = std::process::Command::new("codesign")
        .args([
            "--verify",
            "--strict",
            "--deep",
            "--verbose=2",
            path.as_str(),
        ])
        .output()
        .context("running codesign --verify")?;
    ensure!(
        verify.status.success(),
        "codesign --verify --strict --deep failed on {path}:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    Ok(())
}

/// The Developer ID gates: Gatekeeper's own assessment, with the type Apple
/// documents per artifact kind (`exec` for an app; `open` with the
/// primary-signature context for a disk image — `install` is for .pkg), then
/// the stapled notarization ticket. Any non-zero exit fails the release.
fn gatekeeper_gates(path: &Utf8Path, kind: GateKind) -> Result<()> {
    let mut spctl = std::process::Command::new("spctl");
    spctl.args(["-a", "-vvv", "-t"]);
    match kind {
        GateKind::App => {
            spctl.arg("exec");
        }
        GateKind::Dmg => {
            spctl.args(["open", "--context", "context:primary-signature"]);
        }
    }
    let spctl = spctl.arg(path.as_str()).output().context("running spctl")?;
    ensure!(
        spctl.status.success(),
        "spctl rejected {path}:\n{}",
        String::from_utf8_lossy(&spctl.stderr)
    );
    let stapler = std::process::Command::new("xcrun")
        .args(["stapler", "validate", path.as_str()])
        .output()
        .context("running stapler validate")?;
    ensure!(
        stapler.status.success(),
        "stapler validate failed on {path} — notarize and staple before shipping:\n{}",
        String::from_utf8_lossy(&stapler.stdout)
    );
    Ok(())
}

fn macos_only(path: &Utf8Path) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("sign verify-app/verify-dmg use codesign/spctl and run on macOS only");
    }
    ensure!(path.exists(), "{path} does not exist");
    Ok(())
}

fn verify_app(path: &Utf8Path, require_notarized: bool) -> Result<()> {
    macos_only(path)?;

    // 1. Structural validity, always — ad-hoc or not.
    codesign_verify_strict(path)?;

    // 2. Who signed it?
    match signature_of(path)? {
        // --verify already refuses an unsealed bundle; stated for completeness.
        Signature::None => bail!("{path} is not signed at all"),
        Signature::AdHoc => {
            if require_notarized {
                bail!(
                    "{path} is ad-hoc signed but --require-notarized was passed \
                     (the release gate needs Developer ID + notarization)"
                );
            }
            println!("{ADHOC_CONSEQUENCE}");
            println!("sign: OK (ad-hoc, structurally valid) {path}");
            Ok(())
        }
        // 3. Developer ID path: Gatekeeper and the stapled ticket are hard gates.
        Signature::Identity(authority) => {
            gatekeeper_gates(path, GateKind::App)?;
            println!("sign: OK (Developer ID [{authority}], Gatekeeper-accepted, stapled) {path}");
            Ok(())
        }
    }
}

fn verify_dmg(path: &Utf8Path, require_notarized: bool) -> Result<()> {
    macos_only(path)?;

    // 0. The image's own integrity, in every era: the UDIF checksum. Apple's
    //    notarization guidance says not to submit a corrupted image; a user
    //    download has the same interest. (No `-quiet`: it would also drop the
    //    reason — "image not recognized", a CRC mismatch — from stderr.)
    let hdiutil = std::process::Command::new("hdiutil")
        .args(["verify", path.as_str()])
        .output()
        .context("running hdiutil verify")?;
    ensure!(
        hdiutil.status.success(),
        "hdiutil verify failed on {path} (corrupt or truncated image):\n{}",
        String::from_utf8_lossy(&hdiutil.stderr)
    );

    // 1. Who signed it? For an image the unsigned state is legal (module doc).
    match signature_of(path)? {
        Signature::None => {
            if require_notarized {
                bail!(
                    "{path} is not signed but --require-notarized was passed \
                     (the release gate needs a Developer ID signed, notarized, stapled image)"
                );
            }
            println!("{UNSIGNED_DMG_REASON}");
            println!("{ADHOC_CONSEQUENCE}");
            println!(
                "sign: OK (unsigned image, the designed pre-signing state; checksum valid) {path}"
            );
            Ok(())
        }
        Signature::AdHoc => bail!("{path} {ADHOC_DMG_REFUSAL}"),
        // 2. Developer ID path: structural validity, then the hard gates.
        Signature::Identity(authority) => {
            codesign_verify_strict(path)?;
            gatekeeper_gates(path, GateKind::Dmg)?;
            println!("sign: OK (Developer ID [{authority}], Gatekeeper-accepted, stapled) {path}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every family must list each variable exactly once, or `status` would
    /// report presence wrongly.
    #[test]
    fn secret_families_have_no_duplicates() {
        let mut seen = std::collections::BTreeSet::new();
        for (_, vars) in FAMILIES {
            for v in *vars {
                assert!(seen.insert(*v), "{v} listed twice");
            }
        }
    }

    /// The consequence text must name the recovery path; it is user-facing
    /// documentation printed at build time, per ADR-011 §7.3.
    #[test]
    fn adhoc_consequence_names_the_recovery_path() {
        assert!(ADHOC_CONSEQUENCE.contains("Open Anyway"));
        assert!(ADHOC_CONSEQUENCE.contains("macos-unquarantine.sh"));
    }

    /// The disk-image texts must cite the upstream decision they rest on, so
    /// the next reader can check it rather than take our word.
    #[test]
    fn dmg_texts_cite_the_bundler_decision() {
        for text in [UNSIGNED_DMG_REASON, ADHOC_DMG_REFUSAL] {
            assert!(
                text.contains("#12323"),
                "PR that skips dmg signing for \"-\""
            );
            assert!(
                text.contains("#12288"),
                "issue that measured the mount refusal"
            );
        }
    }

    /// `codesign -dvv` on an unsealed file: exit 1 and the verdict on stderr.
    /// That verdict must win over the exit status — it is the legal dmg state.
    #[test]
    fn unsigned_is_classified_from_the_verdict_not_the_exit_code() {
        let out = "x.dmg: code object is not signed at all\n";
        assert_eq!(classify_signature(false, out).unwrap(), Signature::None);
    }

    /// The bundler's ad-hoc seal on the .app, as codesign describes it.
    #[test]
    fn adhoc_is_classified_from_the_signature_line() {
        let out = "Executable=/x/Stratum.app/Contents/MacOS/stratum-desktop\n\
                   Identifier=dev.stratum.desktop\n\
                   CodeDirectory v=20500 size=24244 flags=0x10002(adhoc,runtime) hashes=747+7 location=embedded\n\
                   Signature=adhoc\n\
                   TeamIdentifier=not set\n";
        assert_eq!(classify_signature(true, out).unwrap(), Signature::AdHoc);
    }

    /// A Developer ID seal: the first Authority line names the leaf.
    #[test]
    fn identity_is_classified_with_its_leaf_authority() {
        let out = "Identifier=dev.stratum.desktop\n\
                   Signature size=9000\n\
                   Authority=Developer ID Application: Example Corp (ABCDE12345)\n\
                   Authority=Developer ID Certification Authority\n\
                   Authority=Apple Root CA\n\
                   TeamIdentifier=ABCDE12345\n";
        assert_eq!(
            classify_signature(true, out).unwrap(),
            Signature::Identity("Developer ID Application: Example Corp (ABCDE12345)".to_owned())
        );
    }

    /// Any other codesign failure (not a code object, unreadable) is an error,
    /// never silently "unsigned".
    #[test]
    fn other_codesign_failures_are_errors() {
        let out = "x: No such file or directory\n";
        assert!(classify_signature(false, out).is_err());
    }
}
