//! ARCHITECTURE §8.13 / audit finding A26 — R0 ("one owner per file") as a
//! test rather than a promise.
//!
//! Reads `docs/ownership.toml`, expands every pattern against `git ls-files`,
//! and fails if any tracked file is claimed by two work units or by none. Those
//! two conditions are what §8.13 makes fatal, and they are fatal here with no
//! flag involved.
//!
//! An exact path claiming a file that is *not* tracked is the third condition
//! the manifest header describes, and it is reported rather than fatal until
//! `[meta] complete = true` (or `--strict-paths`). The manifest is transcribed
//! from IMPLEMENTATION_PLAN §8 up front and §8 enumerates what the 27 units
//! *will* create, so while the tree is incomplete "claimed but not tracked" is
//! not distinguishable from "not written yet" — for a unit that has landed or
//! one that has not. Making it fatal before then would mean the check is red
//! from the first commit to the last, which is the same as no check at all.
//!
//! `--include-untracked` is a working-tree PREFLIGHT and the one place this
//! command looks past the index. §8.1 makes the tracked set normative so that a
//! build artifact can never trip the check, and the default is exactly that.
//! But during a parallel wave most of the tree is written and not yet
//! committed — twenty-odd agents with a file partition between them — and the
//! tracked-only answer is then both too quiet (it certifies a partition over
//! the minority of the source that happens to be in the index) and too loud (it
//! reports files as "claimed but not tracked" that are sitting on disk
//! finished). The alternative people reach for is to copy the tree, `git init`,
//! `git add -A` and run the check there, which also stages every scratch file
//! anyone left lying around and reports it as an R0 violation. This flag
//! answers the same question in place. Untracked findings are reported in their
//! own section and are never fatal: a file that is not in the repo cannot break
//! R0 for anybody else, and the instant it is committed the ordinary fatal rule
//! has it.
//!
//! The schema and the matching semantics are specified in the manifest's own
//! header; this module implements exactly that.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;
use glob::{MatchOptions, Pattern};
use serde::Deserialize;

use crate::Ctx;

/// `docs/ownership.toml`, schema 1.
#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    #[serde(default)]
    pub meta: Meta,
    #[serde(default, rename = "unit")]
    pub units: Vec<Unit>,
    #[serde(default)]
    pub unowned: Unowned,
}

#[derive(Debug, Default, Deserialize)]
pub struct Meta {
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub generated_from: String,
    #[serde(default)]
    pub counts: Option<Counts>,
    /// Does the tree contain every file §8 enumerates? While it does not, an
    /// exact path matching nothing is "not written yet"; once it does, the same
    /// condition is the rename-silently-unowns bug and becomes an error. The
    /// phase lives here rather than in a CI flag so the command a contributor
    /// runs and the one CI runs cannot disagree.
    #[serde(default)]
    pub complete: bool,
}

/// The plan's own verified totals, used as a cheap transcription check.
#[derive(Debug, Deserialize)]
pub struct Counts {
    pub owners: usize,
    pub exact_paths: usize,
    pub globs: usize,
}

#[derive(Debug, Deserialize)]
pub struct Unit {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub owns: Vec<String>,
    /// Carved out of this unit's `owns`; the file must then be claimed by some
    /// other unit or it is unowned.
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Unowned {
    #[serde(default)]
    pub paths: Vec<String>,
}

/// Normative, from the manifest header: `*`/`?` do not cross `/`, `**` does,
/// and a leading dot is an ordinary character so `.github/...` matches.
pub const MATCH_OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

#[derive(Args)]
pub struct Cmd {
    /// Manifest to read. Defaults to `docs/ownership.toml`.
    #[arg(long, value_name = "FILE")]
    pub manifest: Option<Utf8PathBuf>,

    /// Treat "an exact path claims a file that does not exist" as an error
    /// even while `[meta] complete = false`.
    ///
    /// The two conditions ARCHITECTURE §8.13 makes fatal — a file claimed by
    /// two units, a tracked file claimed by none — are always errors and this
    /// flag does not touch them.
    #[arg(long)]
    pub strict_paths: bool,

    /// Assert `[meta.counts]` against what the manifest actually contains.
    #[arg(long)]
    pub verify_counts: bool,

    /// Also check working-tree files that are not committed yet
    /// (`git ls-files --others --exclude-standard`), reporting what the
    /// partition would look like the moment they are.
    ///
    /// Findings about an uncommitted file are reported, never fatal — see the
    /// module header. The tracked set and the two conditions §8.13 makes fatal
    /// are unaffected by this flag.
    #[arg(long)]
    pub include_untracked: bool,
}

/// Everything the check found, so callers (and tests) can inspect it rather
/// than parse stderr.
#[derive(Debug, Default)]
pub struct Report {
    /// Tracked path -> the units that claim it. Only entries with != 1 claim.
    pub bad: BTreeMap<String, Vec<String>>,
    pub duplicate_ids: Vec<String>,
    /// Exact (metacharacter-free) `owns` entries matching no tracked file, in
    /// a unit that has at least one tracked file of its own.
    pub stale_exact: Vec<(String, String)>,
    /// The same, for a unit with nothing tracked at all — it has not landed.
    pub unbuilt_exact: Vec<(String, String)>,
    /// Globs matching no tracked file. A warning: the unit may not have landed.
    pub empty_globs: Vec<(String, String)>,
    pub owned: usize,
    pub skipped_unowned: usize,
    /// The same shape as `bad`, for working-tree files that are not committed
    /// yet. Empty unless `--include-untracked`. Kept apart from `bad` rather
    /// than merged into it so the counts the summary line reports, and the two
    /// conditions §8.13 makes fatal, stay about the tracked tree alone.
    pub pending: BTreeMap<String, Vec<String>>,
    pub pending_owned: usize,
    pub pending_skipped: usize,
    /// `id -> title`, so a failure names the work unit a human recognises
    /// rather than only its number.
    pub titles: BTreeMap<String, String>,
}

impl Report {
    pub fn title_of(&self, id: &str) -> &str {
        self.titles.get(id).map_or("no title", String::as_str)
    }

    pub fn unowned(&self) -> impl Iterator<Item = &String> {
        self.bad
            .iter()
            .filter(|(_, v)| v.is_empty())
            .map(|(k, _)| k)
    }
    pub fn double_claimed(&self) -> impl Iterator<Item = (&String, &Vec<String>)> {
        self.bad.iter().filter(|(_, v)| v.len() > 1)
    }
    pub fn pending_unowned(&self) -> impl Iterator<Item = &String> {
        self.pending
            .iter()
            .filter(|(_, v)| v.is_empty())
            .map(|(k, _)| k)
    }
    pub fn pending_double_claimed(&self) -> impl Iterator<Item = (&String, &Vec<String>)> {
        self.pending.iter().filter(|(_, v)| v.len() > 1)
    }
}

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    let manifest_path = cmd
        .manifest
        .clone()
        .unwrap_or_else(|| ctx.path("docs/ownership.toml"));
    let manifest = load(&manifest_path)?;
    let tracked = git_ls_files(&ctx.root)?;

    if cmd.verify_counts {
        verify_counts(&manifest)?;
    }

    // The default is the normative check of §8.1 and touches nothing but the
    // index; the preflight is reached only by asking for it.
    let report = if cmd.include_untracked {
        check_working_tree(&manifest, &tracked, &git_ls_others(&ctx.root)?)?
    } else {
        check(&manifest, &tracked)?
    };
    emit(&report, cmd.strict_paths || manifest.meta.complete).with_context(|| {
        format!(
            "manifest is {} ({})",
            manifest.meta.source, manifest.meta.generated_from
        )
    })
}

pub fn load(path: &Utf8Path) -> Result<Manifest> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let manifest: Manifest = toml::from_str(&text).with_context(|| format!("parsing {path}"))?;
    anyhow::ensure!(
        manifest.schema == 1,
        "{path}: schema {} is not supported by this xtask (expected 1)",
        manifest.schema
    );
    Ok(manifest)
}

/// `git ls-files` and not a filesystem walk: §8.1 says an untracked build
/// artifact must never trip the check, and only git knows what is tracked.
pub fn git_ls_files(root: &Utf8Path) -> Result<Vec<String>> {
    git_paths(root, &["ls-files", "-z"])
}

/// The working-tree preflight input set: files that exist but are not in the
/// index. `--exclude-standard` is what keeps `target/` and `node_modules/` out,
/// so the flag inherits `.gitignore` rather than re-deciding what an artifact
/// is — the same authority `git add -A` would use if these were committed.
pub fn git_ls_others(root: &Utf8Path) -> Result<Vec<String>> {
    git_paths(root, &["ls-files", "-z", "--others", "--exclude-standard"])
}

fn git_paths(root: &Utf8Path, args: &[&str]) -> Result<Vec<String>> {
    let out = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .with_context(|| format!("running `git {}`", args.join(" ")))?;
    anyhow::ensure!(
        out.status.success(),
        "`git {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let mut paths: Vec<String> = String::from_utf8(out.stdout)
        .with_context(|| format!("`git {}` produced non-UTF-8 output", args.join(" ")))?
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    paths.sort();
    Ok(paths)
}

/// A pattern with none of `*`, `?` or `[` is an exact path: it names one file
/// rather than a set, so matching nothing is worth reporting.
pub fn is_exact(pattern: &str) -> bool {
    !pattern.contains(['*', '?', '['])
}

fn compile(unit: &str, pattern: &str) -> Result<Pattern> {
    anyhow::ensure!(
        !pattern.starts_with('/') && !pattern.starts_with("./") && !pattern.contains('\\'),
        "unit {unit}: pattern {pattern:?} must be repo-relative, `/`-separated, \
         and must not begin with `/` or `./`"
    );
    Pattern::new(pattern).with_context(|| format!("unit {unit}: bad pattern {pattern:?}"))
}

/// The normative check of §8.1: the tracked set and nothing else.
pub fn check(manifest: &Manifest, tracked: &[String]) -> Result<Report> {
    check_working_tree(manifest, tracked, &[])
}

/// `check`, plus the `--include-untracked` preflight. `untracked` is attributed
/// exactly like `tracked` and *does* count towards whether a pattern matched
/// anything — a file finished on disk but not committed is written, and calling
/// it "not written yet" is the false report this flag exists to remove — but
/// its findings land in `Report::pending`, which `emit` never makes fatal.
pub fn check_working_tree(
    manifest: &Manifest,
    tracked: &[String],
    untracked: &[String],
) -> Result<Report> {
    let mut report = Report {
        titles: manifest
            .units
            .iter()
            .filter(|u| !u.title.is_empty())
            .map(|u| (u.id.clone(), u.title.clone()))
            .collect(),
        ..Report::default()
    };

    let mut seen_ids: BTreeMap<&str, usize> = BTreeMap::new();
    for unit in &manifest.units {
        *seen_ids.entry(unit.id.as_str()).or_default() += 1;
    }
    report.duplicate_ids = seen_ids
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(id, _)| id.to_owned())
        .collect();

    let unowned: Vec<Pattern> = manifest
        .unowned
        .paths
        .iter()
        .map(|p| compile("[unowned]", p))
        .collect::<Result<_>>()?;

    struct Compiled<'a> {
        id: &'a str,
        owns: Vec<(&'a str, Pattern)>,
        exclude: Vec<Pattern>,
    }
    let units: Vec<Compiled<'_>> = manifest
        .units
        .iter()
        .map(|u| -> Result<Compiled<'_>> {
            Ok(Compiled {
                id: &u.id,
                owns: u
                    .owns
                    .iter()
                    .map(|p| Ok((p.as_str(), compile(&u.id, p)?)))
                    .collect::<Result<_>>()?,
                exclude: u
                    .exclude
                    .iter()
                    .map(|p| compile(&u.id, p))
                    .collect::<Result<_>>()?,
            })
        })
        .collect::<Result<_>>()?;

    // Which `owns` patterns matched at least one file in the working tree.
    let mut matched: BTreeMap<(&str, &str), bool> = BTreeMap::new();
    for u in &units {
        for (raw, _) in &u.owns {
            matched.insert((u.id, raw), false);
        }
    }

    // `attribute` is steps 2–4 of the manifest's CHECK ALGORITHM for one path,
    // and `None` is step 2's "matched [unowned], drop it". Both input sets go
    // through it unchanged; only where the verdict is filed differs.
    let mut attribute = |path: &String| -> Option<Vec<String>> {
        let p = Utf8Path::new(path);
        if unowned
            .iter()
            .any(|g| g.matches_path_with(p.as_std_path(), MATCH_OPTIONS))
        {
            return None;
        }
        let mut claims = Vec::new();
        for u in &units {
            let mut hit = None;
            for (raw, g) in &u.owns {
                if g.matches_path_with(p.as_std_path(), MATCH_OPTIONS) {
                    matched.insert((u.id, raw), true);
                    hit = Some(*raw);
                }
            }
            if hit.is_none() {
                continue;
            }
            let excluded = u
                .exclude
                .iter()
                .any(|g| g.matches_path_with(p.as_std_path(), MATCH_OPTIONS));
            if !excluded {
                claims.push(u.id.to_owned());
            }
        }
        Some(claims)
    };

    for path in tracked {
        match attribute(path) {
            None => report.skipped_unowned += 1,
            Some(claims) if claims.len() == 1 => report.owned += 1,
            Some(claims) => {
                report.bad.insert(path.clone(), claims);
            }
        }
    }
    for path in untracked {
        match attribute(path) {
            None => report.pending_skipped += 1,
            Some(claims) if claims.len() == 1 => report.pending_owned += 1,
            Some(claims) => {
                report.pending.insert(path.clone(), claims);
            }
        }
    }

    // A unit none of whose patterns matched anything has not landed; one that
    // matched something has, and an unmatched exact path of *its* is the more
    // interesting report even though neither is decidably a rename until the
    // tree is complete.
    let landed: BTreeSet<&str> = matched
        .iter()
        .filter(|(_, hit)| **hit)
        .map(|((id, _), _)| *id)
        .collect();

    for ((id, raw), hit) in &matched {
        if *hit {
            continue;
        }
        let entry = ((*id).to_owned(), (*raw).to_owned());
        if !is_exact(raw) {
            report.empty_globs.push(entry);
        } else if landed.contains(id) {
            report.stale_exact.push(entry);
        } else {
            report.unbuilt_exact.push(entry);
        }
    }

    Ok(report)
}

fn verify_counts(manifest: &Manifest) -> Result<()> {
    let Some(counts) = &manifest.meta.counts else {
        return Ok(());
    };
    let owners = manifest.units.len();
    let (mut exact, mut globs) = (0usize, 0usize);
    for u in &manifest.units {
        for p in &u.owns {
            if is_exact(p) {
                exact += 1;
            } else {
                globs += 1;
            }
        }
    }
    anyhow::ensure!(
        (owners, exact, globs) == (counts.owners, counts.exact_paths, counts.globs),
        "[meta.counts] disagrees with the manifest body: declared \
         owners={} exact_paths={} globs={}, found owners={owners} \
         exact_paths={exact} globs={globs}",
        counts.owners,
        counts.exact_paths,
        counts.globs
    );
    println!("ownership: [meta.counts] verified ({owners} owners, {exact} exact, {globs} globs)");
    Ok(())
}

fn emit(report: &Report, strict_paths: bool) -> Result<()> {
    let mut failed = false;

    if !report.duplicate_ids.is_empty() {
        failed = true;
        eprintln!(
            "ownership: duplicate unit id(s): {}",
            report.duplicate_ids.join(", ")
        );
    }

    let unowned: Vec<&String> = report.unowned().collect();
    if !unowned.is_empty() {
        failed = true;
        eprintln!(
            "ownership: {} tracked file(s) owned by NOBODY:",
            unowned.len()
        );
        for p in &unowned {
            eprintln!("    {p}");
        }
        eprintln!("  Add each to a unit's `owns` in docs/ownership.toml, or to [unowned].");
    }

    let doubled: Vec<_> = report.double_claimed().collect();
    if !doubled.is_empty() {
        failed = true;
        eprintln!(
            "ownership: {} tracked file(s) claimed by MORE THAN ONE unit:",
            doubled.len()
        );
        for (path, units) in &doubled {
            eprintln!("    {path}  <- {}", units.join(", "));
        }
        eprintln!("  R0 allows exactly one owner. See IMPLEMENTATION_PLAN §8.2.");
    }

    // Neither bucket is decidably a bug until the tree is complete: an exact
    // path matching nothing is either a file nobody has written yet or one a
    // rename unowned, and nothing in the tree distinguishes them. `[meta]
    // complete` (or --strict-paths) is the declaration that the first reading
    // is no longer available. Until then these are reports, and the split is
    // there because a gap in a unit that IS on disk is the one worth reading.
    if !report.stale_exact.is_empty() {
        eprintln!(
            "ownership: {} exact path(s) claimed but not tracked, by a unit that has landed:",
            report.stale_exact.len()
        );
        for (id, p) in &report.stale_exact {
            eprintln!("    {id} ({}): {p}", report.title_of(id));
        }
        eprintln!("  Either the owner has not written it yet, or a rename silently unowned it.");
        if strict_paths {
            failed = true;
        }
    }

    if !report.unbuilt_exact.is_empty() {
        if strict_paths {
            failed = true;
            eprintln!(
                "ownership: {} exact path(s) STALE (claimed but not tracked):",
                report.unbuilt_exact.len()
            );
            for (id, p) in &report.unbuilt_exact {
                eprintln!("    {id} ({}): {p}", report.title_of(id));
            }
        } else {
            println!(
                "ownership: warning — {} exact path(s) not written yet (unit not landed)",
                report.unbuilt_exact.len()
            );
        }
    }

    if !report.empty_globs.is_empty() {
        println!(
            "ownership: warning — {} glob(s) match nothing yet (unit not landed)",
            report.empty_globs.len()
        );
    }

    // --include-untracked only. Reported by name so the reader can act on it,
    // and never fatal: an uncommitted file is not in the repo, so it cannot
    // break R0 for anybody — and the moment it is committed the tracked rules
    // above have it with no leniency at all. Making it fatal here would turn
    // any scratch file anyone left in the tree into an R0 violation, which is
    // the false positive this preflight exists to stop people improvising.
    let pending_unowned: Vec<&String> = report.pending_unowned().collect();
    if !pending_unowned.is_empty() {
        println!(
            "ownership: preflight — {} UNCOMMITTED file(s) would be owned by NOBODY:",
            pending_unowned.len()
        );
        for p in &pending_unowned {
            println!("    {p}");
        }
        println!("  Claim each in docs/ownership.toml before committing, or delete it.");
    }

    let pending_doubled: Vec<_> = report.pending_double_claimed().collect();
    if !pending_doubled.is_empty() {
        println!(
            "ownership: preflight — {} UNCOMMITTED file(s) would be claimed by MORE THAN ONE unit:",
            pending_doubled.len()
        );
        for (path, units) in &pending_doubled {
            println!("    {path}  <- {}", units.join(", "));
        }
    }

    if failed {
        anyhow::bail!("the ownership manifest does not partition the tracked tree");
    }
    println!(
        "ownership: OK — {} tracked file(s) each owned by exactly one unit, {} skipped as [unowned]",
        report.owned, report.skipped_unowned
    );
    // Only printed when the preflight ran, and stated separately from the line
    // above so "OK — N tracked file(s)" never silently starts counting files
    // that are not in the repo.
    let pending_total = report.pending_owned + report.pending_skipped + report.pending.len();
    if pending_total > 0 {
        println!(
            "ownership: preflight — {} uncommitted file(s) checked, {} owned, {} skipped as [unowned]",
            pending_total, report.pending_owned, report.pending_skipped
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(toml_src: &str) -> Manifest {
        toml::from_str(toml_src).expect("manifest parses")
    }

    const BASE: &str = r#"
schema = 1
[[unit]]
id = "W00"
owns = ["Cargo.toml", "xtask/**"]
[[unit]]
id = "W01"
owns = ["crates/stratum-core/**"]
[unowned]
paths = ["target/**"]
"#;

    #[test]
    fn a_clean_partition_passes() {
        let m = manifest(BASE);
        let tracked = vec![
            "Cargo.toml".to_owned(),
            "xtask/src/main.rs".to_owned(),
            "crates/stratum-core/src/lib.rs".to_owned(),
            "target/debug/xtask".to_owned(),
        ];
        let r = check(&m, &tracked).unwrap();
        assert!(r.bad.is_empty(), "{:?}", r.bad);
        assert_eq!(r.owned, 3);
        assert_eq!(r.skipped_unowned, 1);
        assert!(r.stale_exact.is_empty());
        emit(&r, true).expect("a clean partition must pass even under --strict-paths");
    }

    /// NEGATIVE TEST (W00 acceptance): a file in two units' globs must fail.
    #[test]
    fn a_file_owned_twice_fails() {
        let m = manifest(
            r#"
schema = 1
[[unit]]
id = "W00"
owns = ["xtask/**"]
[[unit]]
id = "W07"
owns = ["xtask/src/*.rs"]
"#,
        );
        let tracked = vec!["xtask/src/main.rs".to_owned()];
        let r = check(&m, &tracked).unwrap();
        let doubled: Vec<_> = r.double_claimed().collect();
        assert_eq!(doubled.len(), 1);
        assert_eq!(doubled[0].1, &vec!["W00".to_owned(), "W07".to_owned()]);
        assert!(
            emit(&r, false).is_err(),
            "a double claim is fatal with no flag: §8.13 says so"
        );
    }

    /// NEGATIVE TEST (W00 acceptance): a file in no unit's globs must fail.
    #[test]
    fn a_file_owned_by_nobody_fails() {
        let m = manifest(BASE);
        let tracked = vec!["scripts/orphan.sh".to_owned()];
        let r = check(&m, &tracked).unwrap();
        assert_eq!(r.unowned().count(), 1);
        assert!(
            emit(&r, false).is_err(),
            "an unowned tracked file is fatal with no flag: §8.13 says so"
        );
    }

    /// `exclude` is the single declared exception in the whole partition
    /// (stratum-proto's frame.rs). It must carve out, not merely deprioritise:
    /// the carved file is unowned unless another unit claims it.
    #[test]
    fn exclude_carves_out_and_can_leave_a_file_unowned() {
        let m = manifest(
            r#"
schema = 1
[[unit]]
id = "W00"
owns = ["crates/stratum-proto/**"]
exclude = ["crates/stratum-proto/src/frame.rs"]
"#,
        );
        let r = check(&m, &["crates/stratum-proto/src/frame.rs".to_owned()]).unwrap();
        assert_eq!(r.unowned().count(), 1);

        let m = manifest(
            r#"
schema = 1
[[unit]]
id = "W00"
owns = ["crates/stratum-proto/**"]
exclude = ["crates/stratum-proto/src/frame.rs"]
[[unit]]
id = "W07"
owns = ["crates/stratum-proto/src/frame.rs"]
"#,
        );
        let r = check(&m, &["crates/stratum-proto/src/frame.rs".to_owned()]).unwrap();
        assert!(r.bad.is_empty(), "{:?}", r.bad);
        assert_eq!(r.owned, 1);
    }

    #[test]
    fn duplicate_unit_ids_fail() {
        let m = manifest(
            r#"
schema = 1
[[unit]]
id = "W00"
owns = ["a"]
[[unit]]
id = "W00"
owns = ["b"]
"#,
        );
        let r = check(&m, &["a".to_owned(), "b".to_owned()]).unwrap();
        assert_eq!(r.duplicate_ids, vec!["W00".to_owned()]);
        assert!(emit(&r, false).is_err());
    }

    /// An unmatched exact path is sorted by whether its owner is on disk at
    /// all, and neither bucket is fatal until the tree is declared complete.
    #[test]
    fn unmatched_exact_paths_are_split_by_whether_the_unit_landed() {
        let m = manifest(
            r#"
schema = 1
[[unit]]
id = "W00"
owns = ["Cargo.toml", "gone.rs", "crates/not-yet/**"]
[[unit]]
id = "W09"
owns = ["crates/stratum-cli/src/main.rs"]
"#,
        );
        let r = check(&m, &["Cargo.toml".to_owned()]).unwrap();
        assert_eq!(
            r.stale_exact,
            vec![("W00".to_owned(), "gone.rs".to_owned())],
            "W00 has a tracked file, so its missing claim is the loud one"
        );
        assert_eq!(
            r.unbuilt_exact,
            vec![(
                "W09".to_owned(),
                "crates/stratum-cli/src/main.rs".to_owned()
            )],
            "W09 has nothing tracked, so it simply has not landed"
        );
        assert_eq!(
            r.empty_globs,
            vec![("W00".to_owned(), "crates/not-yet/**".to_owned())]
        );

        emit(&r, false).expect("during build-out both buckets are reports");
        assert!(
            emit(&r, true).is_err(),
            "--strict-paths / [meta] complete makes both fatal"
        );
    }

    /// The phase lives in the manifest, so `cargo xtask ownership` with no
    /// arguments means the same thing to a contributor and to CI.
    #[test]
    fn meta_complete_defaults_to_false_and_round_trips() {
        assert!(!manifest(BASE).meta.complete);
        assert!(
            manifest(
                r#"
schema = 1
[meta]
complete = true
"#
            )
            .meta
            .complete
        );
    }

    /// The matching semantics are normative, so they get their own test rather
    /// than being inherited from whatever `glob` happens to do this release.
    #[test]
    fn pattern_semantics_are_the_declared_ones() {
        let star = Pattern::new("xtask/src/*.rs").unwrap();
        assert!(star.matches_path_with(
            Utf8Path::new("xtask/src/main.rs").as_std_path(),
            MATCH_OPTIONS
        ));
        assert!(
            !star.matches_path_with(
                Utf8Path::new("xtask/src/deep/main.rs").as_std_path(),
                MATCH_OPTIONS
            ),
            "* must not cross a separator"
        );

        let deep = Pattern::new("xtask/**").unwrap();
        assert!(deep.matches_path_with(
            Utf8Path::new("xtask/src/deep/main.rs").as_std_path(),
            MATCH_OPTIONS
        ));
        assert!(
            !deep.matches_path_with(Utf8Path::new("xtask").as_std_path(), MATCH_OPTIONS),
            "dir/** must not match the directory itself"
        );

        let dot = Pattern::new(".github/workflows/ci.yml").unwrap();
        assert!(
            dot.matches_path_with(
                Utf8Path::new(".github/workflows/ci.yml").as_std_path(),
                MATCH_OPTIONS
            ),
            "a leading dot is an ordinary character"
        );

        let case = Pattern::new("README.md").unwrap();
        assert!(
            !case.matches_path_with(Utf8Path::new("readme.md").as_std_path(), MATCH_OPTIONS),
            "matching is case-sensitive"
        );

        assert!(is_exact("docs/ownership.toml"));
        assert!(!is_exact("docs/**"));
        assert!(!is_exact("xtask/src/*.rs"));
        assert!(!is_exact("tests/fixtures/sdp1/*.bin"));
    }

    /// The `--include-untracked` preflight (regression, wave-2 repair round 1).
    ///
    /// The wave-1 gate could not answer "does the manifest partition this tree?"
    /// in place — only 478 of 882 source files were committed — so it copied the
    /// tree, `git init`, `git add -A` and ran the check there. That staged a
    /// scratch `sync.sh` somebody had left at the root and reported it as a
    /// fatal R0 violation of a file that was never going to be committed. The
    /// preflight answers the same question without the copy: an uncommitted
    /// file is attributed exactly like a tracked one, but its verdict is a
    /// report, not an exit code.
    #[test]
    fn untracked_files_are_attributed_but_never_fatal() {
        let m = manifest(BASE);
        let tracked = vec!["Cargo.toml".to_owned()];
        let untracked = vec![
            "crates/stratum-core/src/lib.rs".to_owned(), // owned, just not committed
            "target/debug/xtask".to_owned(),             // [unowned]
            "sync.sh".to_owned(),                        // the scratch file
        ];

        // Default: the working tree is invisible, exactly as §8.1 requires.
        let tracked_only = check(&m, &tracked).unwrap();
        assert!(tracked_only.pending.is_empty());
        assert_eq!(tracked_only.pending_owned, 0);
        emit(&tracked_only, false).expect("tracked-only is clean");

        let r = check_working_tree(&m, &tracked, &untracked).unwrap();
        assert!(r.bad.is_empty(), "no TRACKED file is unowned: {:?}", r.bad);
        assert_eq!(r.owned, 1);
        assert_eq!(r.pending_owned, 1);
        assert_eq!(r.pending_skipped, 1, "[unowned] applies to both sets");
        assert_eq!(
            r.pending_unowned().collect::<Vec<_>>(),
            vec!["sync.sh"],
            "the scratch file is named, so it can be acted on"
        );
        emit(&r, false).expect("an uncommitted unowned file must not fail the gate");
        emit(&r, true).expect("nor under --strict-paths: it is not in the repo");

        // ...and the moment it IS committed, the ordinary rule has it.
        let committed = check(&m, &["Cargo.toml".to_owned(), "sync.sh".to_owned()]).unwrap();
        assert_eq!(committed.unowned().collect::<Vec<_>>(), vec!["sync.sh"]);
        assert!(emit(&committed, false).is_err());
    }

    /// The other half of the same false report: `crates/stratum-core/src/lib.rs`
    /// written but not committed was being printed as "claimed but not tracked",
    /// which reads as a rename that unowned a file. A file on disk is written.
    #[test]
    fn an_uncommitted_file_satisfies_the_claim_that_names_it() {
        let m = manifest(
            r#"
schema = 1
[[unit]]
id = "W00"
owns = ["Cargo.toml"]
[[unit]]
id = "W03"
owns = ["crates/stratum-dta/**", "docs/THREAT_MODEL.md"]
"#,
        );
        let tracked = vec![
            "Cargo.toml".to_owned(),
            "crates/stratum-dta/src/lib.rs".to_owned(),
        ];

        let blind = check(&m, &tracked).unwrap();
        assert_eq!(
            blind.stale_exact,
            vec![("W03".to_owned(), "docs/THREAT_MODEL.md".to_owned())],
            "tracked-only cannot tell 'not written' from 'not committed'"
        );

        let r = check_working_tree(&m, &tracked, &["docs/THREAT_MODEL.md".to_owned()]).unwrap();
        assert!(
            r.stale_exact.is_empty() && r.unbuilt_exact.is_empty(),
            "the file exists, so nothing is missing: {:?} {:?}",
            r.stale_exact,
            r.unbuilt_exact
        );
    }

    #[test]
    fn absolute_and_backslash_patterns_are_rejected() {
        for bad in ["/etc/passwd", "./Cargo.toml", "xtask\\src"] {
            let m = Manifest {
                schema: 1,
                meta: Meta::default(),
                units: vec![Unit {
                    id: "W00".into(),
                    title: String::new(),
                    owns: vec![bad.to_owned()],
                    exclude: vec![],
                }],
                unowned: Unowned::default(),
            };
            assert!(check(&m, &[]).is_err(), "{bad} must be rejected");
        }
    }
}
