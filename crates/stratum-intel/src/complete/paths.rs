//! Path completion — design 07 §7.1's "string/path position" row.
//!
//! > filesystem completion, project-relative first, then cwd
//!
//! # This module reads no directory, and cannot
//!
//! `stratum-intel` builds for `wasm32-unknown-unknown` and runs inside the
//! editor's module: there is no filesystem there to read, and reaching one from
//! the native build would put a blocking directory walk on the keystroke path
//! anyway. The candidate list is therefore [`crate::Env::project_files`], which
//! the host supplies and refreshes when the project tree changes — the same list
//! the r(601) quick fix ranks against, so a path the popup offered and a path the
//! error suggests can never disagree.
//!
//! Absent that list the popup is empty. That is the honest state, and it is
//! visibly different from "no matches", because the host either knows the
//! project tree or it does not.

use super::rank::Ranker;
use super::{CompletionContext, CompletionKind};

pub(super) fn offer<'a>(r: &mut Ranker<'a>, ctx: &CompletionContext<'a>) {
    // Project-relative first (group 0), then anything the host listed under the
    // working directory (group 1). Both come from the same vector; the split is
    // by whether the path is inside the project root.
    let root = ctx.env.project_root.as_deref();
    for (i, p) in ctx.env.project_files.iter().enumerate() {
        let inside = root.is_some_and(|r| p.starts_with(r)) || p.is_relative();
        r.offer(
            p.as_str(),
            CompletionKind::Path,
            u8::from(!inside),
            i as u32,
            None,
            None,
            u32::MAX,
            0,
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use camino::Utf8PathBuf;

    use crate::complete::{complete, CompletionContext, CompletionKind};
    use crate::Env;

    fn env() -> Env {
        Env {
            project_root: Some(Utf8PathBuf::from("proj")),
            project_files: [
                "proj/data/wave2019.dta",
                "proj/data/wave2020.dta",
                "/tmp/other.dta",
            ]
            .iter()
            .map(Utf8PathBuf::from)
            .collect(),
            ..Env::default()
        }
    }

    #[test]
    fn a_using_position_completes_project_files() {
        let e = env();
        let src = "merge 1:1 pid using proj/data/wave20";
        let items = complete(&CompletionContext::new(src, src.len(), &e)).items;
        assert_eq!(items.len(), 2, "{items:?}");
        assert!(items.iter().all(|i| i.kind == CompletionKind::Path));
    }

    #[test]
    fn project_relative_paths_come_before_anything_outside() {
        let e = env();
        let src = "use ";
        let items = complete(&CompletionContext::new(src, src.len(), &e)).items;
        assert_eq!(
            items.first().map(|i| i.label.as_str()),
            Some("proj/data/wave2019.dta")
        );
        assert_eq!(
            items.last().map(|i| i.label.as_str()),
            Some("/tmp/other.dta")
        );
    }

    #[test]
    fn with_no_project_listing_the_popup_is_empty() {
        let e = Env::default();
        let src = "use data/w";
        assert!(complete(&CompletionContext::new(src, src.len(), &e))
            .items
            .is_empty());
    }
}
