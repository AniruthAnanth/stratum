//! `stratum completions <SHELL>` — bash | zsh | fish | powershell | elvish.
//!
//! Generated from the same `clap::Command` the binary parses with, so a verb
//! that exists is completable and a verb that does not, is not. There is no
//! second list.

use std::io::Write;

use clap::CommandFactory;

use crate::cli::{Cli, CompletionsArgs, ExitCode};
use crate::cmd::CmdError;

/// `stratum completions`.
///
/// # Errors
/// [`CmdError::Io`] on a write failure.
pub fn completions(
    args: &CompletionsArgs,
    out: &mut impl Write,
    _err: &mut impl Write,
) -> Result<ExitCode, CmdError> {
    let mut cmd = Cli::command();
    clap_complete::generate(args.shell, &mut cmd, "stratum", out);
    out.flush().map_err(|source| CmdError::Io {
        path: camino::Utf8PathBuf::from("<stdout>"),
        source,
    })?;
    Ok(ExitCode::Success)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gen(shell: clap_complete::Shell) -> String {
        let mut out = Vec::new();
        completions(&CompletionsArgs { shell }, &mut out, &mut Vec::new()).unwrap();
        String::from_utf8(out).expect("completions are UTF-8")
    }

    #[test]
    fn every_supported_shell_produces_a_script_naming_every_verb() {
        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Zsh,
            clap_complete::Shell::Fish,
            clap_complete::Shell::PowerShell,
            clap_complete::Shell::Elvish,
        ] {
            let script = gen(shell);
            assert!(script.len() > 200, "{shell:?} produced {script:?}");
            for verb in ["run", "exec", "serve", "check", "describe"] {
                assert!(script.contains(verb), "{shell:?} forgot `{verb}`");
            }
            assert!(!script.contains("difftest"), "{shell:?} offered difftest");
        }
    }
}
