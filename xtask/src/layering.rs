//! ARCHITECTURE §8.1, §8.2, §8.4, §8.5 and the crate half of §8.6 — the
//! dependency direction of the crate graph, checked against `cargo metadata`.
//!
//! The whole point of this check is that it reasons over the *resolved* graph,
//! so a forbidden crate reached through four intermediaries is caught with the
//! path printed. A text scan of `Cargo.toml` files can only see direct edges,
//! which is precisely the case that never goes wrong.
//!
//! Four passes:
//!
//! 1. **Forbidden edges** (08 §2.3, ARCHITECTURE §8.1/§8.2) over the host graph
//!    with `--all-features`, because a violation that only appears under a
//!    non-default feature is still a violation.
//! 2. **§32 / §8.6, reachability** — nothing reachable from `default-members`
//!    may reach `stratum-difftest` or `stratum-e2e`.
//! 3. **§32 / §5, membership** — `default-members` itself is the right set.
//!    This pass exists because the reachability pass above cannot see the bug
//!    it is meant to prevent: while the root manifest read
//!    `default-members = ["crates/*"]`, `crates/stratum-e2e` landed and became
//!    a default member, and pass 2 stayed green because nothing *reaches* it —
//!    it simply *is* one. Cargo's member patterns have no negation, so the
//!    exclusion list can only be written by enumerating everything else, and an
//!    enumeration nobody checks drifts in both directions. Both are asserted.
//! 4. **§8.4 (amended)** — the wasm-clean set builds for
//!    `wasm32-unknown-unknown` and reaches none of `tokio`, `time`, `memmap2`
//!    or a locale crate. The dependency half is decided from
//!    `cargo metadata --filter-platform wasm32-unknown-unknown`, which resolves
//!    exactly the edges that target enables; the `std::fs` half cannot be seen
//!    in the graph at all and is a source scan over those crates only.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{Context, Result};
use camino::Utf8Path;
use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, PackageId};
use clap::Args;

use crate::Ctx;

/// One "crate X may not reach crate Y" rule.
pub struct Rule {
    pub subject: &'static str,
    pub forbidden: &'static [&'static str],
    /// Printed with the violation. A rule nobody can explain gets deleted, so
    /// the explanation lives next to the rule.
    pub why: &'static str,
}

/// 08 §2.3 verbatim, widened by ARCHITECTURE §8.1 (which adds `stratum-exec`
/// and the three `stratum-platform-*` impls plus `stratum-platform-host`) and
/// §8.2 (the desktop's ban on the engine).
pub const FORBIDDEN_EDGES: &[Rule] = &[
    Rule {
        subject: "stratum-core",
        forbidden: &PLATFORM_AND_SHELL_PLUS_TOKIO,
        why: "the numeric core must be callable from a plain synchronous test \
              loop and from wasm; async lives in the CLI/desktop shell only",
    },
    Rule {
        subject: "stratum-data",
        forbidden: &PLATFORM_AND_SHELL,
        why: ENGINE_WHY,
    },
    Rule {
        subject: "stratum-dta",
        forbidden: &PLATFORM_AND_SHELL,
        why: ENGINE_WHY,
    },
    Rule {
        subject: "stratum-parse",
        forbidden: &PLATFORM_AND_SHELL,
        why: ENGINE_WHY,
    },
    Rule {
        subject: "stratum-effects",
        forbidden: &PLATFORM_AND_SHELL,
        why: ENGINE_WHY,
    },
    Rule {
        subject: "stratum-stats",
        forbidden: &PLATFORM_AND_SHELL,
        why: ENGINE_WHY,
    },
    Rule {
        subject: "stratum-graph",
        forbidden: &PLATFORM_AND_SHELL,
        why: ENGINE_WHY,
    },
    Rule {
        subject: "stratum-runtime",
        forbidden: &PLATFORM_AND_SHELL,
        why: ENGINE_WHY,
    },
    Rule {
        subject: "stratum-session",
        forbidden: &PLATFORM_AND_SHELL,
        why: ENGINE_WHY,
    },
    Rule {
        subject: "stratum-exec",
        forbidden: &PLATFORM_AND_SHELL,
        why: ENGINE_WHY,
    },
    Rule {
        subject: "stratum-intel",
        forbidden: &[
            "stratum-platform",
            "stratum-platform-macos",
            "stratum-platform-windows",
            "stratum-platform-linux",
            "stratum-platform-host",
            "tauri",
            "reqwest",
            "tokio",
            "hyper",
            "ureq",
        ],
        why: "ARCHITECTURE §8.5 — deterministic intelligence runs at keystroke \
              latency inside the wasm segmenter; it reaches no network crate \
              and no async runtime",
    },
    Rule {
        subject: "stratum-cli",
        forbidden: &["tauri", "stratum-difftest"],
        why: "ARCHITECTURE §8.8 / spec §30 — the headless binary links no GUI \
              and cannot talk to Stata",
    },
    Rule {
        subject: "stratum-desktop",
        forbidden: &[
            "stratum-runtime",
            "stratum-exec",
            "stratum-session",
            "stratum-parse",
            "stratum-stats",
            "stratum-graph",
            "stratum-data",
            "stratum-dta",
            "stratum-core",
            "stratum-effects",
            "stratum-difftest",
        ],
        why: "ARCHITECTURE §8.2 — a bug in the engine must be physically unable \
              to crash the UI; this edge is what makes the process boundary \
              meaningful (ADR-014)",
    },
];

const ENGINE_WHY: &str = "ARCHITECTURE §8.1 — nothing at or below stratum-exec \
                          may reach the platform layer, tauri, or the network";

const PLATFORM_AND_SHELL: [&str; 7] = [
    "stratum-platform",
    "stratum-platform-macos",
    "stratum-platform-windows",
    "stratum-platform-linux",
    "stratum-platform-host",
    "tauri",
    "reqwest",
];

const PLATFORM_AND_SHELL_PLUS_TOKIO: [&str; 8] = [
    "stratum-platform",
    "stratum-platform-macos",
    "stratum-platform-windows",
    "stratum-platform-linux",
    "stratum-platform-host",
    "tauri",
    "reqwest",
    "tokio",
];

/// OS-binding crates that only the platform layer may name DIRECTLY.
///
/// This used to be a `[bans].deny … wrappers = [...]` list in `deny.toml`, and
/// it cannot live there: cargo-deny's `wrappers` means "the only crates in the
/// whole graph allowed to depend on X", and in a Tauri application `tokio`,
/// `tempfile`, `wry`, `tao`, `muda`, `rfd` and some forty other third-party
/// crates depend on `windows-sys` or `objc2` directly. The rule the manifest was
/// trying to state is about OUR crates — ARCHITECTURE §8.3: no first-party crate
/// outside `stratum-platform-*` reaches for an OS API by name — and a
/// first-party *direct* edge is exactly what `cargo metadata` can check.
///
/// `time` is the same shape with a different reason (A2: the wire carries
/// `UnixMs`, and only the four listed crates ever render one for a human).
pub const DIRECT_ONLY_FROM: &[(&str, &[&str], &str)] = &[
    (
        "objc2",
        &["stratum-platform-macos"],
        "§8.3 — macOS APIs stay in stratum-platform-macos",
    ),
    (
        "objc2-app-kit",
        &["stratum-platform-macos"],
        "§8.3 — macOS APIs stay in stratum-platform-macos",
    ),
    (
        "objc2-foundation",
        &["stratum-platform-macos"],
        "§8.3 — macOS APIs stay in stratum-platform-macos",
    ),
    (
        "security-framework",
        &["stratum-platform-macos"],
        "§8.3 — the macOS keychain is a platform concern",
    ),
    (
        "windows",
        &["stratum-platform-windows"],
        "§8.3 — Win32 stays in stratum-platform-windows",
    ),
    (
        "windows-sys",
        &["stratum-platform-windows"],
        "§8.3 — Win32 stays in stratum-platform-windows",
    ),
    (
        "zbus",
        &["stratum-platform-linux"],
        "§8.3 — D-Bus/portals stay in stratum-platform-linux",
    ),
    (
        "time",
        &[
            "stratum-cli",
            "stratum-desktop",
            "stratum-difftest",
            "stratum-e2e",
        ],
        "A2 — the wire is UnixMs; only a human-facing edge renders a date",
    ),
];

/// One crate that is a workspace member but must never be in `default-members`.
pub struct NonDefault {
    pub name: &'static str,
    /// Printed with the problem, same contract as `Rule::why`.
    pub why: &'static str,
}

/// ARCHITECTURE §5, transcribed — the crates a bare `cargo build` at the repo
/// root must not compile. The root `Cargo.toml`'s `default-members` block is the
/// other half of this pair, and this list is what makes it a check.
///
/// A name here that matches no workspace member is not an error: `stratum-
/// difftest` is listed before W23 creates it, deliberately, so that the crate
/// cannot land as a default member during the window where nobody is looking.
pub const NEVER_DEFAULT_MEMBER: &[NonDefault] = &[
    NonDefault {
        name: "stratum-difftest",
        why: "ARCHITECTURE §5 / spec §32 — the only crate that can talk to \
              Stata; a bare `cargo build` must never compile it",
    },
    NonDefault {
        name: "stratum-e2e",
        why: "ARCHITECTURE §5 — the e2e harness drives the packaged app under \
              WebDriver; excluding it from `default-members` is the \
              machine-checked form of §32",
    },
    NonDefault {
        name: "stratum-wasm",
        why: "ARCHITECTURE §5 — not a default member; `xtask wasm` is the only \
              build of it that produces the artifact the webview loads",
    },
    NonDefault {
        name: "stratum-desktop",
        why: "root Cargo.toml — a bare `cargo build` at the root must not start \
              compiling tauri; CI's `--workspace` still covers it",
    },
    NonDefault {
        name: "xtask",
        why: "ARCHITECTURE §5, §8.6 — build tooling, including this check",
    },
];

/// ARCHITECTURE §8.4 (amended by A2): these five build for
/// `wasm32-unknown-unknown` and stay clean of host-only surface.
pub const WASM_CLEAN: &[&str] = &[
    "stratum-proto",
    "stratum-core",
    "stratum-parse",
    "stratum-effects",
    "stratum-intel",
];

/// The crates §8.4 names, plus the concrete locale/timezone crates that "a
/// locale crate" means in practice. Listing them beats a substring heuristic:
/// `unicode-width` is not a locale crate and must not trip this.
pub const WASM_FORBIDDEN_DEPS: &[&str] = &[
    "tokio",
    "time",
    "memmap2",
    // "a locale crate" — anything that carries CLDR/tz data or reads the host
    // locale. Any of these makes formatting depend on the machine, which is the
    // determinism gate (ADR-013) failing quietly.
    "chrono",
    "chrono-tz",
    "iana-time-zone",
    "tz-rs",
    "sys-locale",
    "locale_config",
    "pure-rust-locales",
    "num-format",
    "icu",
    "icu_locid",
    "icu_locale_core",
    "unic-langid",
    "fluent",
    "fluent-bundle",
];

/// Source-level surface that a graph check cannot see. `wasm32-unknown-unknown`
/// still *compiles* `std::fs`, it just fails at run time, so building for the
/// target proves nothing here.
const WASM_FORBIDDEN_SOURCE: &[(&str, &str)] = &[
    (
        "std::fs",
        "§8.4 — no filesystem access in the wasm-clean set",
    ),
    (
        "std::net",
        "§8.4/§8.5 — no network access in the wasm-clean set",
    ),
    (
        "std::process",
        "§8.4 — no subprocesses in the wasm-clean set",
    ),
];

#[derive(Args)]
pub struct Cmd {
    /// Also run `cargo check --target wasm32-unknown-unknown
    /// --no-default-features` for the wasm-clean set. Off by default because it
    /// needs `rustup target add wasm32-unknown-unknown`; CI turns it on.
    #[arg(long)]
    pub wasm_build: bool,

    /// Fail if a crate named by a rule is missing from the workspace. Off while
    /// the workspace is still being filled in; CI turns it on once every crate
    /// in ARCHITECTURE §5 exists, so that a rule can never be silently retired
    /// by a rename.
    #[arg(long)]
    pub require_all: bool,
}

/// A violation, with the reachability path that proves it.
#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    pub subject: String,
    pub forbidden: String,
    pub path: Vec<String>,
    pub why: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "  {} must not reach {}", self.subject, self.forbidden)?;
        writeln!(f, "      via: {}", self.path.join(" -> "))?;
        write!(f, "      why: {}", self.why)
    }
}

/// The two ways `default-members` can be wrong. Separate from `Violation`
/// because there is no dependency path to print: the evidence is a line that is
/// present in the root manifest or missing from it, and the message has to name
/// the edit rather than a graph.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MembershipProblem {
    /// Named by `NEVER_DEFAULT_MEMBER` and yet a default member.
    MustBeExcluded { name: String, why: String },
    /// A workspace member that is neither a default member nor exempt, so a
    /// bare `cargo build` silently stopped compiling it.
    SilentlyDropped {
        name: String,
        /// Workspace-relative directory, i.e. the exact string to add.
        dir: String,
    },
}

impl std::fmt::Display for MembershipProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MustBeExcluded { name, why } => {
                writeln!(f, "  {name} is in `default-members` and must not be")?;
                writeln!(
                    f,
                    "      fix: delete its line from `default-members` in Cargo.toml"
                )?;
                write!(f, "      why: {why}")
            }
            Self::SilentlyDropped { name, dir } => {
                writeln!(f, "  {name} is a workspace member but not a default member")?;
                writeln!(
                    f,
                    "      fix: add \"{dir}\", to `default-members` in Cargo.toml"
                )?;
                write!(
                    f,
                    "      why: `default-members` is an enumeration (cargo has no \
                     negation for the §32 exclusions), so a new crate joins the \
                     default build only when someone adds it. W00 owns Cargo.toml \
                     — escalate rather than editing it if it is not yours."
                )
            }
        }
    }
}

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    let manifest = ctx.path("Cargo.toml");
    let host =
        metadata(&manifest, &["--all-features"]).context("cargo metadata --all-features failed")?;

    let mut violations = Vec::new();
    let mut checked = 0usize;
    let mut absent = Vec::new();

    for rule in FORBIDDEN_EDGES {
        match find_package(&host, rule.subject) {
            None => absent.push(rule.subject),
            Some(id) => {
                checked += 1;
                for forbidden in rule.forbidden {
                    if let Some(path) = reaches(&host, &id, forbidden) {
                        violations.push(Violation {
                            subject: rule.subject.to_owned(),
                            forbidden: (*forbidden).to_owned(),
                            path,
                            why: rule.why.to_owned(),
                        });
                    }
                }
            }
        }
    }

    // §8.3, direct-edge half: a first-party crate naming an OS binding in its
    // own manifest. Transitive reach is not the question here (everything that
    // links tokio reaches windows-sys); the *direct* edge is.
    violations.extend(direct_edge_violations(&host));

    // §8.6, crate half: nothing a bare `cargo build` compiles may reach the
    // Stata harness or the e2e harness. Checked over default-members rather than
    // over the two binaries, because that is the exact set `cargo build` with no
    // arguments resolves.
    for id in &host.workspace_default_members as &[PackageId] {
        let name = host[id].name.to_string();
        for (target, why) in NEVER_IN_DEFAULT_BUILD {
            if name == *target {
                // Being the crate is not reaching it, so `reaches` cannot see
                // this case. Pass 3 reports it too, from the manifest side.
                violations.push(Violation {
                    subject: "default-members".to_owned(),
                    forbidden: (*target).to_owned(),
                    path: vec![name.clone()],
                    why: (*why).to_owned(),
                });
                continue;
            }
            if let Some(path) = reaches(&host, id, target) {
                violations.push(Violation {
                    subject: name.clone(),
                    forbidden: (*target).to_owned(),
                    path,
                    why: (*why).to_owned(),
                });
            }
        }
    }

    // §32 / §5, membership half. See the module header: this is the pass that
    // would have caught `crates/stratum-e2e` becoming a default member.
    let membership = membership_problems(&host);

    // §8.4 dependency half, resolved for the wasm target so that a
    // `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` escape hatch is
    // honoured rather than flagged.
    let wasm = metadata(&manifest, &["--filter-platform", "wasm32-unknown-unknown"])
        .context("cargo metadata --filter-platform wasm32-unknown-unknown failed")?;
    for subject in WASM_CLEAN {
        let Some(id) = find_package(&wasm, subject) else {
            continue;
        };
        for forbidden in WASM_FORBIDDEN_DEPS {
            if let Some(path) = reaches(&wasm, &id, forbidden) {
                violations.push(Violation {
                    subject: (*subject).to_owned(),
                    forbidden: (*forbidden).to_owned(),
                    path,
                    why: WASM_WHY.to_owned(),
                });
            }
        }
    }

    // §8.4 source half.
    for subject in WASM_CLEAN {
        let Some(id) = find_package(&host, subject) else {
            continue;
        };
        let src = host[&id]
            .manifest_path
            .parent()
            .context("package manifest has no parent")?
            .join("src");
        violations.extend(scan_source(subject, &src, WASM_FORBIDDEN_SOURCE)?);
    }

    if cmd.wasm_build {
        wasm_build(ctx, &host)?;
    }

    if !absent.is_empty() {
        let list = absent.join(", ");
        if cmd.require_all {
            anyhow::bail!(
                "--require-all: these crates are named by a layering rule but do not exist: {list}"
            );
        }
        println!(
            "layering: {} rule(s) not yet applicable (crate absent): {list}",
            absent.len()
        );
    }

    if violations.is_empty() && membership.is_empty() {
        println!(
            "layering: OK — {checked} crate rule(s), {} wasm-clean crate(s), \
             {} default member(s), {} member(s) excluded by name",
            WASM_CLEAN
                .iter()
                .filter(|c| find_package(&host, c).is_some())
                .count(),
            host.workspace_default_members.len(),
            NEVER_DEFAULT_MEMBER
                .iter()
                .filter(|e| find_package(&host, e.name).is_some())
                .count(),
        );
        return Ok(());
    }

    eprintln!(
        "layering: {} violation(s)\n",
        violations.len() + membership.len()
    );
    for m in &membership {
        eprintln!("{m}\n");
    }
    for v in &violations {
        eprintln!("{v}\n");
    }
    anyhow::bail!("crate layering is violated");
}

/// Crates that no default member may *be* or *reach*. `stratum-e2e` is here as
/// well as `stratum-difftest` because under `--all-features` — which is what
/// this check resolves — an edge to it pulls the whole WebDriver stack into the
/// default build.
const NEVER_IN_DEFAULT_BUILD: &[(&str, &str)] = &[
    ("stratum-difftest", DIFFTEST_WHY),
    (
        "stratum-e2e",
        "ARCHITECTURE §5 — the e2e harness is excluded from the default build; \
         nothing a bare `cargo build` compiles may depend on it",
    ),
];

const DIFFTEST_WHY: &str =
    "ARCHITECTURE §8.6 / spec §32 — a bare `cargo build` must never compile \
     anything that can talk to Stata";

const WASM_WHY: &str = "ARCHITECTURE §8.4 (amended by A2) — proto, core, parse, \
                        effects and intel build for wasm32-unknown-unknown and \
                        reach no host-only surface";

/// Pass 3, the cargo-facing half: reduce `cargo metadata` to the two name sets
/// the rule is actually about, then decide.
fn membership_problems(md: &Metadata) -> Vec<MembershipProblem> {
    let default: BTreeSet<&str> = md
        .workspace_default_members
        .iter()
        .map(|id| md[id].name.as_str())
        .collect();

    let members: Vec<(String, String)> = md
        .workspace_members
        .iter()
        .map(|id| {
            let pkg = &md[id];
            let dir = pkg
                .manifest_path
                .parent()
                .and_then(|d| d.strip_prefix(&md.workspace_root).ok())
                .map_or_else(|| pkg.name.to_string(), Utf8Path::to_string);
            (pkg.name.to_string(), dir)
        })
        .collect();

    check_membership(&members, &default)
}

/// Pass 3, the decision. Pure over `(member name, workspace-relative dir)` pairs
/// and the set of default-member names, so the two directions can be tested
/// without building a workspace for each case.
fn check_membership(
    members: &[(String, String)],
    default: &BTreeSet<&str>,
) -> Vec<MembershipProblem> {
    let mut out = Vec::new();

    // Direction 1 — the exclusion list is a check, not a comment in a manifest.
    for ex in NEVER_DEFAULT_MEMBER {
        if default.contains(ex.name) {
            out.push(MembershipProblem::MustBeExcluded {
                name: ex.name.to_owned(),
                why: ex.why.to_owned(),
            });
        }
    }

    // Direction 2 — nothing drifts OUT of the default build unnoticed. This is
    // the price of losing the `crates/*` glob and the reason the loss is
    // affordable: a crate that lands without its `default-members` line gets a
    // red check naming the line, instead of quietly leaving the default build.
    for (name, dir) in members {
        if default.contains(name.as_str()) || NEVER_DEFAULT_MEMBER.iter().any(|e| e.name == name) {
            continue;
        }
        out.push(MembershipProblem::SilentlyDropped {
            name: name.clone(),
            dir: dir.clone(),
        });
    }

    out.sort();
    out
}

fn metadata(manifest: &Utf8Path, extra: &[&str]) -> Result<Metadata> {
    let mut cmd = MetadataCommand::new();
    cmd.manifest_path(manifest);
    cmd.other_options(extra.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
    Ok(cmd.exec()?)
}

fn find_package(md: &Metadata, name: &str) -> Option<PackageId> {
    md.packages
        .iter()
        .find(|p| p.name.as_str() == name)
        .map(|p| p.id.clone())
}

/// Breadth-first over *normal* dependency edges only. Build and dev edges are
/// deliberately excluded: a dev-dependency on `tokio` in `stratum-core`'s test
/// harness ships in nothing, and treating it as a violation would push people
/// into writing worse tests.
///
/// [`DIRECT_ONLY_FROM`], evaluated over every workspace member's normal
/// dependencies. Dev-dependencies ship in nothing and are not edges here.
fn direct_edge_violations(md: &Metadata) -> Vec<Violation> {
    let Some(resolve) = md.resolve.as_ref() else {
        return Vec::new();
    };
    let nodes: BTreeMap<&PackageId, &cargo_metadata::Node> =
        resolve.nodes.iter().map(|n| (&n.id, n)).collect();
    let mut out = Vec::new();
    for member in &md.workspace_members {
        let Some(node) = nodes.get(member) else {
            continue;
        };
        let subject = md[member].name.to_string();
        for dep in &node.deps {
            let normal = dep.dep_kinds.is_empty()
                || dep
                    .dep_kinds
                    .iter()
                    .any(|k| k.kind == DependencyKind::Normal || k.kind == DependencyKind::Build);
            if !normal {
                continue;
            }
            let dep_name = md[&dep.pkg].name.to_string();
            for (banned, allowed, why) in DIRECT_ONLY_FROM {
                if dep_name == *banned && !allowed.contains(&subject.as_str()) {
                    out.push(Violation {
                        subject: subject.clone(),
                        forbidden: dep_name.clone(),
                        path: vec![subject.clone(), dep_name.clone()],
                        why: (*why).to_owned(),
                    });
                }
            }
        }
    }
    out
}

/// Returns the shortest path from `from` to a package named `target`.
fn reaches(md: &Metadata, from: &PackageId, target: &str) -> Option<Vec<String>> {
    let resolve = md.resolve.as_ref()?;
    let nodes: BTreeMap<&PackageId, &cargo_metadata::Node> =
        resolve.nodes.iter().map(|n| (&n.id, n)).collect();

    let mut seen: BTreeSet<&PackageId> = BTreeSet::new();
    let mut prev: BTreeMap<&PackageId, &PackageId> = BTreeMap::new();
    let mut queue: VecDeque<&PackageId> = VecDeque::new();
    seen.insert(from);
    queue.push_back(from);

    while let Some(cur) = queue.pop_front() {
        let Some(node) = nodes.get(cur) else { continue };
        for dep in &node.deps {
            if !dep.dep_kinds.is_empty()
                && !dep
                    .dep_kinds
                    .iter()
                    .any(|k| k.kind == DependencyKind::Normal)
            {
                continue;
            }
            let next = &dep.pkg;
            if !seen.insert(next) {
                continue;
            }
            prev.insert(next, cur);
            if md[next].name.as_str() == target {
                // Walk the predecessor chain back to `from`.
                let mut chain = vec![next];
                let mut at = next;
                while let Some(p) = prev.get(at) {
                    chain.push(p);
                    at = p;
                }
                chain.reverse();
                return Some(chain.iter().map(|id| md[id].name.to_string()).collect());
            }
            queue.push_back(next);
        }
    }
    None
}

/// A deliberately literal substring scan, run over the crates §8.4 names only.
/// It is not a parser and does not need to be: the point is that `std::fs`
/// never appears at all, so the false-positive cost of a comment mentioning it
/// is a one-line rephrase.
fn scan_source(subject: &str, src: &Utf8Path, needles: &[(&str, &str)]) -> Result<Vec<Violation>> {
    let mut out = Vec::new();
    if !src.is_dir() {
        return Ok(out);
    }
    for file in rust_files(src)? {
        let text = std::fs::read_to_string(&file).with_context(|| format!("reading {file}"))?;
        for (line_no, line) in text.lines().enumerate() {
            for (needle, why) in needles {
                if line.contains(needle) {
                    out.push(Violation {
                        subject: subject.to_owned(),
                        forbidden: (*needle).to_owned(),
                        path: vec![format!("{file}:{}", line_no + 1)],
                        why: (*why).to_owned(),
                    });
                }
            }
        }
    }
    Ok(out)
}

fn rust_files(dir: &Utf8Path) -> Result<Vec<camino::Utf8PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).with_context(|| format!("reading {d}"))? {
            let entry = entry?;
            let path = camino::Utf8PathBuf::from_path_buf(entry.path())
                .map_err(|p| anyhow::anyhow!("non-UTF-8 path {}", p.display()))?;
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else if path.extension() == Some("rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// ARCHITECTURE §8.4's build half. Separate from the graph check because it
/// needs the target's std, which is a ~200 MB rustup download a contributor
/// should opt into.
fn wasm_build(ctx: &Ctx, md: &Metadata) -> Result<()> {
    let present: Vec<&str> = WASM_CLEAN
        .iter()
        .copied()
        .filter(|c| find_package(md, c).is_some())
        .collect();
    if present.is_empty() {
        println!("layering: --wasm-build skipped, none of the wasm-clean crates exist yet");
        return Ok(());
    }
    let mut cmd =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()));
    cmd.current_dir(&ctx.root)
        .arg("check")
        .arg("--target")
        .arg("wasm32-unknown-unknown")
        .arg("--no-default-features");
    for c in &present {
        cmd.arg("-p").arg(c);
    }
    let status = cmd.status().context("running cargo check for wasm32")?;
    anyhow::ensure!(
        status.success(),
        "the wasm-clean set does not build for wasm32-unknown-unknown ({})",
        present.join(", ")
    );
    println!("layering: wasm32 build OK for {}", present.join(", "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    /// Builds a throwaway workspace of path-only crates so the rule engine can
    /// be exercised without touching the real tree. `edges` is
    /// `(crate, [its normal deps])`.
    fn synthetic(dir: &Utf8Path, edges: &[(&str, &[&str])]) -> Result<Metadata> {
        let mut members = String::new();
        for (name, _) in edges {
            writeln!(members, "  \"{name}\",").unwrap();
        }
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[workspace]\nresolver = \"2\"\nmembers = [\n{members}]\n"),
        )?;
        for (name, deps) in edges {
            let crate_dir = dir.join(name);
            std::fs::create_dir_all(crate_dir.join("src"))?;
            let mut manifest = format!(
                "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\n\
                 edition = \"2021\"\npublish = false\n\n[dependencies]\n"
            );
            for d in *deps {
                writeln!(manifest, "{d} = {{ path = \"../{d}\" }}").unwrap();
            }
            std::fs::write(crate_dir.join("Cargo.toml"), manifest)?;
            std::fs::write(crate_dir.join("src/lib.rs"), "")?;
        }
        let mut cmd = MetadataCommand::new();
        cmd.manifest_path(dir.join("Cargo.toml"));
        cmd.current_dir(dir);
        cmd.other_options(vec!["--offline".to_owned()]);
        Ok(cmd.exec()?)
    }

    fn tmp() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let td = tempfile::tempdir().expect("tempdir");
        let p = camino::Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("utf-8 tempdir");
        (td, p)
    }

    fn check(md: &Metadata, subject: &str, forbidden: &str) -> Option<Vec<String>> {
        let id = find_package(md, subject)?;
        reaches(md, &id, forbidden)
    }

    /// THE NEGATIVE TEST (IMPLEMENTATION_PLAN W00 acceptance): a deliberate
    /// violation must be reported, and it must be reported through the
    /// transitive path rather than only when the edge is direct.
    #[test]
    fn deliberate_violation_is_caught() {
        let (_g, dir) = tmp();
        let md = synthetic(
            &dir,
            &[
                ("engine", &["middle"]),
                ("middle", &["forbidden"]),
                ("forbidden", &[]),
                ("unrelated", &[]),
            ],
        )
        .expect("synthetic workspace");

        let path = check(&md, "engine", "forbidden").expect("violation must be reported");
        assert_eq!(path, ["engine", "middle", "forbidden"]);

        // Positive control: the same engine does not reach an unrelated crate,
        // so the checker is not simply answering "yes" to everything.
        assert_eq!(check(&md, "engine", "unrelated"), None);
        assert_eq!(check(&md, "unrelated", "forbidden"), None);
    }

    /// Dev-dependencies are not shipped and must not be treated as violations.
    #[test]
    fn dev_dependencies_are_not_edges() {
        let (_g, dir) = tmp();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nmembers = [\"engine\", \"forbidden\"]\n",
        )
        .unwrap();
        for (name, extra) in [
            (
                "engine",
                "[dev-dependencies]\nforbidden = { path = \"../forbidden\" }\n",
            ),
            ("forbidden", ""),
        ] {
            std::fs::create_dir_all(dir.join(name).join("src")).unwrap();
            std::fs::write(
                dir.join(name).join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\n\
                     edition = \"2021\"\npublish = false\n\n{extra}"
                ),
            )
            .unwrap();
            std::fs::write(dir.join(name).join("src/lib.rs"), "").unwrap();
        }
        let mut cmd = MetadataCommand::new();
        cmd.manifest_path(dir.join("Cargo.toml"));
        cmd.current_dir(&dir);
        cmd.other_options(vec!["--offline".to_owned()]);
        let md = cmd.exec().expect("metadata");

        assert_eq!(check(&md, "engine", "forbidden"), None);
    }

    /// The `std::fs` half of §8.4 is a source scan, so it gets its own negative
    /// test: the graph can never see it.
    #[test]
    fn source_scan_catches_std_fs() {
        let (_g, dir) = tmp();
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("deep")).unwrap();
        std::fs::write(src.join("clean.rs"), "pub fn ok() -> u32 { 1 }\n").unwrap();
        let found = scan_source("stratum-parse", &src, WASM_FORBIDDEN_SOURCE).unwrap();
        assert!(found.is_empty(), "clean tree must pass: {found:?}");

        std::fs::write(
            src.join("deep/dirty.rs"),
            "pub fn load() { let _ = std::fs::read(\"commands.ron\"); }\n",
        )
        .unwrap();
        let found = scan_source("stratum-parse", &src, WASM_FORBIDDEN_SOURCE).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].forbidden, "std::fs");
        // The walk spells the path with host separators; which file and line
        // was flagged is the property, which slash is not.
        assert!(
            found[0].path[0]
                .replace('\\', "/")
                .ends_with("deep/dirty.rs:1"),
            "{:?}",
            found[0]
        );
    }

    fn members(names: &[&str]) -> Vec<(String, String)> {
        names
            .iter()
            .map(|n| ((*n).to_owned(), format!("crates/{n}")))
            .collect()
    }

    fn defaults<'a>(names: &[&'a str]) -> BTreeSet<&'a str> {
        names.iter().copied().collect()
    }

    /// THE REGRESSION. `default-members = ["crates/*"]` made `crates/stratum-e2e`
    /// a default member the moment W25 created it, and the reachability pass
    /// stayed green because nothing *reaches* the harness — it simply is one of
    /// them. Pass 3 is what closes that, so it gets the e2e case by name.
    #[test]
    fn a_named_exclusion_in_default_members_is_caught() {
        let all = members(&["stratum-proto", "stratum-e2e"]);
        let found = check_membership(&all, &defaults(&["stratum-proto", "stratum-e2e"]));
        assert_eq!(
            found,
            vec![MembershipProblem::MustBeExcluded {
                name: "stratum-e2e".to_owned(),
                why: NEVER_DEFAULT_MEMBER
                    .iter()
                    .find(|e| e.name == "stratum-e2e")
                    .expect("stratum-e2e is on the exclusion list")
                    .why
                    .to_owned(),
            }]
        );

        // Positive control: excluded and absent from the default set is the
        // correct state, and must be silent.
        assert!(check_membership(&all, &defaults(&["stratum-proto"])).is_empty());
    }

    /// The other direction, which is the price of losing the `crates/*` glob:
    /// a crate that lands without its `default-members` line must be a red
    /// check naming the line, not a silent shrink of the default build.
    #[test]
    fn a_member_missing_from_default_members_is_caught() {
        let found = check_membership(
            &members(&["stratum-proto", "stratum-newcrate"]),
            &defaults(&["stratum-proto"]),
        );
        assert_eq!(
            found,
            vec![MembershipProblem::SilentlyDropped {
                name: "stratum-newcrate".to_owned(),
                dir: "crates/stratum-newcrate".to_owned(),
            }]
        );
        // The message has to carry the exact string to paste, or the escalation
        // it asks for costs more than the bug.
        assert!(
            found[0]
                .to_string()
                .contains("add \"crates/stratum-newcrate\","),
            "{}",
            found[0]
        );
    }

    /// The committed root manifest itself, not just the rule engine. This is
    /// what makes `cargo test -p xtask` — which ci.yml runs — fail on a hand
    /// edit to `default-members`, in addition to `cargo xtask layering`.
    #[test]
    fn the_real_workspace_default_members_are_correct() {
        let root = Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask lives one level below the workspace root");
        let mut cmd = MetadataCommand::new();
        cmd.manifest_path(root.join("Cargo.toml"));
        // `--no-deps` is enough: `workspace_members` and
        // `workspace_default_members` are workspace facts, so the test needs no
        // dependency resolution and cannot touch the network.
        cmd.no_deps();
        let md = cmd
            .exec()
            .expect("cargo metadata --no-deps on the real tree");

        let found = membership_problems(&md);
        assert!(found.is_empty(), "{found:#?}");

        // And the one the whole repair is about, stated positively so a future
        // rename of the crate cannot make this test vacuous.
        let default: BTreeSet<&str> = md
            .workspace_default_members
            .iter()
            .map(|id| md[id].name.as_str())
            .collect();
        assert!(
            md.workspace_members
                .iter()
                .any(|id| md[id].name.as_str() == "stratum-e2e"),
            "stratum-e2e must still be a workspace member — CI's --workspace \
             is what compiles and tests it"
        );
        assert!(!default.contains("stratum-e2e"), "{default:?}");
    }

    /// The rule table is data; a typo in it silently disables a rule.
    #[test]
    fn rule_table_is_well_formed() {
        let mut seen = BTreeSet::new();
        for rule in FORBIDDEN_EDGES {
            assert!(
                seen.insert(rule.subject),
                "duplicate rule for {}",
                rule.subject
            );
            assert!(!rule.forbidden.is_empty(), "{} bans nothing", rule.subject);
            assert!(!rule.why.is_empty(), "{} has no rationale", rule.subject);
            assert!(
                !rule.forbidden.contains(&rule.subject),
                "{} bans itself",
                rule.subject
            );
        }
        for c in WASM_CLEAN {
            assert!(c.starts_with("stratum-"), "{c} is not a workspace crate");
        }

        let mut excluded = BTreeSet::new();
        for e in NEVER_DEFAULT_MEMBER {
            assert!(
                excluded.insert(e.name),
                "duplicate exclusion for {}",
                e.name
            );
            assert!(!e.why.is_empty(), "{} has no rationale", e.name);
        }
        // Every crate a default member may not *reach* must also be one it may
        // not *be*; the two lists disagreeing is how the e2e hole opened.
        for (name, why) in NEVER_IN_DEFAULT_BUILD {
            assert!(!why.is_empty(), "{name} has no rationale");
            assert!(
                excluded.contains(name),
                "{name} is banned from the default build's dep graph but is not \
                 on NEVER_DEFAULT_MEMBER, so it could still be a default member"
            );
        }
    }
}
