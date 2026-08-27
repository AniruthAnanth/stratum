//! `stratum init [DIR]` — scaffold a project.
//!
//! Three files, and the reason for each is in `stratum-workspace`, not here:
//!
//! * `.stratum/workspace.json` — committed project preferences
//!   (`Project::save`, ARCHITECTURE C19's *durable* sidecar half).
//! * `.stratum/.gitignore` — written by `Project::save` itself; it says `*`, so
//!   the gitignored `cache/` tree under `.stratum/` self-ignores and a team that
//!   wants to commit `workspace.json` adds a `!workspace.json` negation. That is
//!   deliberately the user's decision.
//! * `stratum.toml` — the project configuration `doctor` resolves against
//!   (design 08 §4.2's precedence: flag > env > project > user > default).
//!
//! `init` deliberately writes no `.do` file. The four gated `.do` writers are
//! `stratum-workspace`'s and each holds a proof its edit is safe (ADR-010); a
//! scaffolder that wrote source would be a fifth.

use std::io::Write;

use camino::Utf8PathBuf;
use stratum_workspace::Project;

use crate::cli::{ExitCode, InitArgs};
use crate::cmd::CmdError;

/// The project config `doctor` reads. Kept minimal on purpose: a key here is a
/// key somebody has to keep meaning the same thing forever.
pub const STRATUM_TOML: &str = "\
# Stratum project configuration.
#
# Precedence (design 08 §4.2): CLI flag > STRATUM_* env var > this file >
# user config > built-in default. `stratum doctor` prints the resolved value
# and its source for every setting.

[project]
# The .do that `stratum run` and the desktop's \"run project\" resolve to.
# entry = \"analysis.do\"

[run]
# Classic output is rendered at 80 columns in v1; `set linesize` other than 80
# fails with rc 10 (ADR-016). The key is listed so its absence is a decision.
linesize = 80
";

/// `stratum init`.
///
/// # Errors
/// [`CmdError::Io`] if the directory cannot be written.
pub fn init(
    args: &InitArgs,
    out: &mut impl Write,
    _err: &mut impl Write,
) -> Result<ExitCode, CmdError> {
    let root = args.dir.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
            .unwrap_or_else(|| Utf8PathBuf::from("."))
    });
    std::fs::create_dir_all(&root).map_err(|source| CmdError::Io {
        path: root.clone(),
        source,
    })?;

    let project = Project::load(&root).map_err(|e| CmdError::Io {
        path: root.clone(),
        source: std::io::Error::other(e.to_string()),
    })?;
    let state = project.save().map_err(|e| CmdError::Io {
        path: root.clone(),
        source: std::io::Error::other(e.to_string()),
    })?;

    let toml = root.join("stratum.toml");
    if toml.exists() && !args.force {
        writeln!(out, "{toml} exists; left alone (pass --force to overwrite)").ok();
    } else {
        std::fs::write(&toml, STRATUM_TOML).map_err(|source| CmdError::Io {
            path: toml.clone(),
            source,
        })?;
        writeln!(out, "{toml}").ok();
    }
    writeln!(out, "{state}").ok();
    Ok(ExitCode::Success)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};

    fn go(dir: &str, extra: &[&str]) -> (ExitCode, String) {
        let mut argv = vec!["stratum", "init", dir];
        argv.extend_from_slice(extra);
        let args = match Cli::try_parse_from(&argv).expect("argv parses").command {
            Command::Init(a) => a,
            other => panic!("expected `init`, got {other:?}"),
        };
        let mut out = Vec::new();
        let code = init(&args, &mut out, &mut Vec::new()).expect("writable tempdir");
        (code, String::from_utf8(out).unwrap())
    }

    #[test]
    fn a_fresh_directory_gets_a_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        let (code, out) = go(root.to_str().unwrap(), &[]);
        assert_eq!(code, ExitCode::Success);
        assert!(root.join("stratum.toml").is_file(), "{out}");
        assert!(root.join(".stratum/workspace.json").is_file());
        // Written by `Project::save`: the cache tree under `.stratum/` ignores
        // itself, and committing `workspace.json` is an explicit negation.
        assert_eq!(
            std::fs::read_to_string(root.join(".stratum/.gitignore"))
                .unwrap()
                .trim(),
            "*"
        );
    }

    #[test]
    fn an_existing_config_is_not_clobbered_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("stratum.toml"), "# mine\n").unwrap();
        go(root.to_str().unwrap(), &[]);
        assert_eq!(
            std::fs::read_to_string(root.join("stratum.toml")).unwrap(),
            "# mine\n"
        );
        go(root.to_str().unwrap(), &["--force"]);
        assert_eq!(
            std::fs::read_to_string(root.join("stratum.toml")).unwrap(),
            STRATUM_TOML
        );
    }

    #[test]
    fn init_writes_no_do_file() {
        let dir = tempfile::tempdir().unwrap();
        go(dir.path().to_str().unwrap(), &[]);
        let dos: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "do"))
            .collect();
        assert!(dos.is_empty(), "the four gated .do writers are W26's");
    }
}
