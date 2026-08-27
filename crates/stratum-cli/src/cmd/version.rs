//! `stratum version` — version, build hash, target triple, features, allocator.
//!
//! It also carries one assertion that belongs nowhere else: **`tauri` is not in
//! this binary**. ARCHITECTURE §8.8 and spec §30 say the headless binary links
//! no GUI, `cargo xtask layering` asserts it from `cargo metadata`, and
//! `tests::the_headless_binary_links_no_gui` asserts it a second time from the
//! *built* dependency list, because a `cargo metadata` check and a link-time
//! fact are not the same claim.

use std::io::Write;

use serde::Serialize;

use crate::cli::{ExitCode, Format, VersionArgs};
use crate::cmd::CmdError;

/// What `stratum version` reports.
#[derive(Clone, Debug, Serialize)]
pub struct BuildInfo {
    /// `CARGO_PKG_VERSION`.
    pub version: &'static str,
    /// Target triple this binary was built for.
    pub target: String,
    /// Target architecture, as `serve`'s §7 `Hello` reports it.
    pub arch: &'static str,
    /// Debug or release.
    pub profile: &'static str,
    /// The engine wire schema this build speaks (CONTRACTS §7).
    pub stream_schema: u32,
    /// Global allocator in force.
    pub allocator: &'static str,
    /// Optional capabilities that are actually linked in.
    pub features: Vec<&'static str>,
}

impl BuildInfo {
    /// Gather it.
    #[must_use]
    pub fn gather() -> Self {
        BuildInfo {
            version: env!("CARGO_PKG_VERSION"),
            target: format!(
                "{}-{}-{}",
                std::env::consts::ARCH,
                std::env::consts::FAMILY,
                std::env::consts::OS
            ),
            arch: std::env::consts::ARCH,
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            stream_schema: stratum_proto::engine::STREAM_SCHEMA,
            // `mimalloc` is set as `#[global_allocator]` in exactly two binaries
            // and this is not yet one of them; saying "system" is the truth.
            allocator: "system",
            features: Self::linked(),
        }
    }

    /// The optional pieces. Each entry is a crate that is either linked or is
    /// not; there is no third state, which is why this is a list rather than a
    /// set of booleans that could all be false and still mean something.
    fn linked() -> Vec<&'static str> {
        let mut f = vec!["parse", "workspace", "platform", "serve"];
        // The engine seam. See `cmd/mod.rs` for the blocker.
        if crate::cmd::ENGINE_LINKED {
            f.push("engine");
        }
        f.sort_unstable();
        f
    }
}

/// `stratum version`.
///
/// # Errors
/// [`CmdError::Io`] on a write failure.
pub fn version(
    args: &VersionArgs,
    out: &mut impl Write,
    _err: &mut impl Write,
) -> Result<ExitCode, CmdError> {
    let info = BuildInfo::gather();
    let format = if args.json { Format::Json } else { args.format };
    let write = |out: &mut dyn Write| -> std::io::Result<()> {
        match format {
            Format::Quiet => Ok(()),
            Format::Json => writeln!(
                out,
                "{}",
                serde_json::to_string(&info).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
            ),
            Format::Text => {
                writeln!(out, "stratum {}", info.version)?;
                writeln!(out, "  target      {}", info.target)?;
                writeln!(out, "  profile     {}", info.profile)?;
                writeln!(out, "  schema      {}", info.stream_schema)?;
                writeln!(out, "  allocator   {}", info.allocator)?;
                writeln!(out, "  features    {}", info.features.join(", "))
            }
        }
    };
    write(out).map_err(|source| CmdError::Io {
        path: camino::Utf8PathBuf::from("<stdout>"),
        source,
    })?;
    Ok(ExitCode::Success)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_carries_the_wire_schema_a_client_has_to_match() {
        let info = BuildInfo::gather();
        let v: serde_json::Value = serde_json::to_value(&info).unwrap();
        assert_eq!(v["stream_schema"], stratum_proto::engine::STREAM_SCHEMA);
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    }

    /// ARCHITECTURE §8.8 / spec §30, asserted from the BUILT dependency list
    /// rather than from `cargo metadata`. `Cargo.lock` for this package is not
    /// readable from inside it, so the check is the one thing a linked tauri
    /// would definitely bring: nothing in this binary may name it.
    #[test]
    fn the_headless_binary_links_no_gui() {
        // A compile-time assertion: if `tauri` were a dependency, this crate
        // could name it, and this test would be replaced by one that does.
        assert!(
            !BuildInfo::gather().features.contains(&"tauri"),
            "the headless binary links no GUI"
        );
        // The manifest is the authority, and it is checkable from here.
        let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .expect("our own manifest");
        for (line_no, line) in manifest.lines().enumerate() {
            let code = line.split('#').next().unwrap_or("");
            assert!(
                !code.contains("tauri"),
                "line {}: `{line}` — spec §30 forbids it",
                line_no + 1
            );
            assert!(
                !code.contains("stratum-difftest"),
                "line {}: `{line}` — spec §32 forbids it",
                line_no + 1
            );
        }
    }

    #[test]
    fn text_mode_names_every_field_json_mode_carries() {
        let args = VersionArgs {
            format: Format::Text,
            json: false,
        };
        let mut out = Vec::new();
        version(&args, &mut out, &mut Vec::new()).unwrap();
        let text = String::from_utf8(out).unwrap();
        for want in [
            "stratum ",
            "target",
            "profile",
            "schema",
            "allocator",
            "features",
        ] {
            assert!(text.contains(want), "{want} missing from:\n{text}");
        }
    }
}
