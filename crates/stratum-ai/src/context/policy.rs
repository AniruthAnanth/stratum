//! 07 §4.3 / ADR-012 (D-AI-04) — `effective = min(global, project, dataset, surface)`.
//!
//! The committed `.stratum/ai-policy.toml` can only ever **lower** the tier.
//! That is not a check written somewhere; it is a consequence of the fold being
//! a `min` over a totally ordered enum. A collaborator cloning a restricted-data
//! repository cannot raise it from their own settings, by design.

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use super::tiers::PrivacyTier;
use crate::provider::egress::NetworkMode;
use crate::provider::types::ProviderId;

/// The file name, relative to the project root. One constant so the loader and
/// the documentation cannot disagree.
pub const POLICY_FILE: &str = ".stratum/ai-policy.toml";

/// The marker file that makes a directory's datasets tier-`Off` (07 §4.3).
pub const FORBIDDEN_MARKER: &str = ".ai-forbidden";

/// A committed project policy.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiPolicy {
    /// Ceiling. Absent means "this file does not constrain the tier".
    #[serde(default)]
    pub max_tier: Option<String>,
    /// Provider allowlist. Absent means "any configured provider".
    #[serde(default)]
    pub providers: Option<Vec<String>>,
    /// Force offline mode regardless of the user's global setting.
    #[serde(default)]
    pub require_offline: bool,
    /// Globs, relative to the project root or absolute, whose datasets are
    /// treated as restricted.
    #[serde(default)]
    pub restricted_paths: Vec<String>,
}

/// A policy file that could not be understood.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// The file is not valid TOML, or carries a key we do not recognise.
    #[error("{POLICY_FILE}: {0}")]
    Malformed(String),
    /// `max_tier` is not one of the four spellings.
    #[error("{POLICY_FILE}: `max_tier = \"{0}\"` is not one of off, schema_only, schema_and_stats, full")]
    BadTier(String),
    /// `providers` names something that is not a provider.
    #[error("{POLICY_FILE}: `{0}` is not one of anthropic, openai_compat, ollama")]
    BadProvider(String),
}

impl AiPolicy {
    /// Parse a policy file's text.
    ///
    /// # Errors
    /// [`PolicyError`] on malformed TOML or an unrecognised value. A malformed
    /// policy is **not** silently ignored: a project that meant to restrict
    /// sharing and typoed the tier must fail loudly, because the failure mode of
    /// ignoring it is a disclosure.
    pub fn parse(text: &str) -> Result<Self, PolicyError> {
        let policy: Self =
            toml::from_str(text).map_err(|e| PolicyError::Malformed(e.to_string()))?;
        policy.validate()?;
        Ok(policy)
    }

    fn validate(&self) -> Result<(), PolicyError> {
        if let Some(t) = &self.max_tier {
            PrivacyTier::parse(t).ok_or_else(|| PolicyError::BadTier(t.clone()))?;
        }
        for p in self.providers.iter().flatten() {
            parse_provider(p).ok_or_else(|| PolicyError::BadProvider(p.clone()))?;
        }
        Ok(())
    }

    /// Load the policy from a project root.
    ///
    /// `Ok(None)` when there is no policy file, which is the common case.
    ///
    /// # Errors
    /// [`PolicyError`] when the file exists and is malformed. Unreadable is
    /// treated as malformed for the same reason: a policy we cannot read is not
    /// a policy that does not exist.
    pub fn load(project_root: &Utf8Path) -> Result<Option<Self>, PolicyError> {
        let path = project_root.join(POLICY_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(PolicyError::Malformed(e.to_string())),
        }
    }

    /// The ceiling this policy imposes, if any.
    #[must_use]
    pub fn tier_ceiling(&self) -> Option<PrivacyTier> {
        self.max_tier.as_deref().and_then(PrivacyTier::parse)
    }

    /// The provider allowlist, if any.
    #[must_use]
    pub fn provider_allowlist(&self) -> Option<Vec<ProviderId>> {
        self.providers
            .as_ref()
            .map(|list| list.iter().filter_map(|p| parse_provider(p)).collect())
    }

    /// Whether `provider` may be used at all under this policy.
    #[must_use]
    pub fn permits(&self, provider: ProviderId) -> bool {
        self.provider_allowlist()
            .is_none_or(|allowed| allowed.contains(&provider))
    }

    /// The network mode this policy forces, if it forces one.
    #[must_use]
    pub const fn network_override(&self) -> Option<NetworkMode> {
        if self.require_offline {
            Some(NetworkMode::Offline)
        } else {
            None
        }
    }
}

fn parse_provider(s: &str) -> Option<ProviderId> {
    match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "anthropic" => Some(ProviderId::Anthropic),
        "openai_compat" | "openai" => Some(ProviderId::OpenAiCompat),
        "ollama" | "local" => Some(ProviderId::Ollama),
        _ => None,
    }
}

/// Which of the four constraints actually bound the effective tier.
///
/// Recorded in the audit log and rendered in the pre-send preview, because "why
/// is this so restricted?" has exactly four possible answers and a user who
/// cannot see which one applies will assume the product is broken.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TierBound {
    /// Settings › AI › Data sharing.
    Global,
    /// `.stratum/ai-policy.toml`.
    Project,
    /// A `.ai-forbidden` marker or a `restricted_paths` match.
    Dataset,
    /// The surface's own ceiling (07 §4.3) — e.g. ghost completion is capped.
    Surface,
}

/// The four inputs to the fold.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TierInputs {
    /// The user's global setting.
    pub global: PrivacyTier,
    /// The committed project policy's ceiling, when there is one.
    pub project: Option<PrivacyTier>,
    /// The loaded dataset's marking, when there is one.
    pub dataset: Option<PrivacyTier>,
    /// The surface's own ceiling.
    pub surface: PrivacyTier,
}

impl Default for TierInputs {
    fn default() -> Self {
        Self {
            global: PrivacyTier::default(),
            project: None,
            dataset: None,
            surface: PrivacyTier::Full,
        }
    }
}

impl TierInputs {
    /// Which constraint bound the result. Ties resolve in declaration order —
    /// global, project, dataset, surface — because the most useful answer to
    /// "why" is the one the user can act on first.
    #[must_use]
    pub fn binding(&self) -> TierBound {
        let e = effective_tier(*self);
        if self.global == e {
            TierBound::Global
        } else if self.project == Some(e) {
            TierBound::Project
        } else if self.dataset == Some(e) {
            TierBound::Dataset
        } else {
            TierBound::Surface
        }
    }
}

/// `effective = min(global, project policy, dataset marking, surface ceiling)`.
#[must_use]
pub fn effective_tier(inputs: TierInputs) -> PrivacyTier {
    let mut t = inputs.global.min(inputs.surface);
    if let Some(p) = inputs.project {
        t = t.min(p);
    }
    if let Some(d) = inputs.dataset {
        t = t.min(d);
    }
    t
}

/// The tier a loaded dataset contributes.
///
/// `Some(PrivacyTier::Off)` when a `.ai-forbidden` marker sits in the dataset's
/// directory or the path matches a `restricted_paths` glob — its schema never
/// appears in a prompt at all, and the AI panel shows a persistent "restricted
/// dataset loaded" badge.
#[must_use]
pub fn dataset_tier(source: Option<&Utf8Path>, restricted: &[String]) -> Option<PrivacyTier> {
    let path = source?;
    if let Some(dir) = path.parent() {
        if dir.join(FORBIDDEN_MARKER).exists() {
            return Some(PrivacyTier::Off);
        }
    }
    let as_str = path.as_str();
    for pattern in restricted {
        if glob_match(pattern, as_str) {
            return Some(PrivacyTier::Off);
        }
    }
    None
}

/// A glob matcher covering `*`, `**` and `?`.
///
/// Hand-written rather than the `glob` crate, which the workspace table scopes
/// to `xtask`: this is one function with one job, and `restricted_paths` is
/// consulted once per dataset load. Segment-aware — `*` does not cross a `/`,
/// `**` does — because `data/*` matching `data/restricted/secret.dta` would be a
/// privacy surprise in the *permissive* direction, which is the one that matters.
#[must_use]
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = path.chars().collect();
    matches_from(&p, 0, &s, 0)
}

fn matches_from(p: &[char], mut pi: usize, s: &[char], mut si: usize) -> bool {
    while pi < p.len() {
        match p[pi] {
            '*' => {
                let doubled = p.get(pi + 1) == Some(&'*');
                let next = pi + if doubled { 2 } else { 1 };
                // `**/` consumes the slash too, so `**/x` matches a bare `x`.
                let next = if doubled && p.get(next) == Some(&'/') {
                    next + 1
                } else {
                    next
                };
                if next >= p.len() {
                    // A trailing `*` may not cross a separator; `**` may.
                    return doubled || !s[si..].contains(&'/');
                }
                let mut k = si;
                loop {
                    if matches_from(p, next, s, k) {
                        return true;
                    }
                    if k >= s.len() {
                        return false;
                    }
                    if !doubled && s[k] == '/' {
                        return false;
                    }
                    k += 1;
                }
            }
            '?' => {
                if si >= s.len() || s[si] == '/' {
                    return false;
                }
                pi += 1;
                si += 1;
            }
            c => {
                if si >= s.len() || s[si] != c {
                    return false;
                }
                pi += 1;
                si += 1;
            }
        }
    }
    si == s.len()
}

/// Where a project policy was found, for the audit record.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PolicyOrigin {
    /// The project root the policy was loaded from.
    pub root: Option<Utf8PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_policy_can_only_lower_the_tier_never_raise_it() {
        // ADR-012 D-AI-04, and the reason the fold is a `min`.
        for global in PrivacyTier::ALL {
            for project in PrivacyTier::ALL {
                let e = effective_tier(TierInputs {
                    global,
                    project: Some(project),
                    dataset: None,
                    surface: PrivacyTier::Full,
                });
                assert!(e <= global, "policy raised {global} to {e}");
                assert!(e <= project);
            }
        }
    }

    #[test]
    fn a_restricted_dataset_forces_off_whatever_the_user_asked_for() {
        let e = effective_tier(TierInputs {
            global: PrivacyTier::Full,
            project: None,
            dataset: Some(PrivacyTier::Off),
            surface: PrivacyTier::Full,
        });
        assert_eq!(e, PrivacyTier::Off);
    }

    #[test]
    fn a_surface_ceiling_binds_even_when_everything_else_is_permissive() {
        let inputs = TierInputs {
            global: PrivacyTier::Full,
            project: None,
            dataset: None,
            surface: PrivacyTier::SchemaOnly,
        };
        assert_eq!(effective_tier(inputs), PrivacyTier::SchemaOnly);
        assert_eq!(inputs.binding(), TierBound::Surface);
    }

    #[test]
    fn the_binding_constraint_is_reported_so_the_user_can_act_on_it() {
        let inputs = TierInputs {
            global: PrivacyTier::Full,
            project: Some(PrivacyTier::SchemaOnly),
            dataset: None,
            surface: PrivacyTier::Full,
        };
        assert_eq!(inputs.binding(), TierBound::Project);
    }

    #[test]
    fn the_documented_policy_file_parses() {
        // 07 §4.3's example, verbatim.
        let text = r#"
max_tier = "schema_only"
providers = ["ollama"]
require_offline = true
"#;
        let p = AiPolicy::parse(text).unwrap();
        assert_eq!(p.tier_ceiling(), Some(PrivacyTier::SchemaOnly));
        assert_eq!(p.provider_allowlist(), Some(vec![ProviderId::Ollama]));
        assert_eq!(p.network_override(), Some(NetworkMode::Offline));
        assert!(p.permits(ProviderId::Ollama));
        assert!(!p.permits(ProviderId::Anthropic));
    }

    #[test]
    fn a_typoed_tier_is_an_error_not_a_silently_ignored_line() {
        // The failure mode of ignoring it is a disclosure, so it is loud.
        let err = AiPolicy::parse("max_tier = \"schema-onlyy\"").unwrap_err();
        assert!(matches!(err, PolicyError::BadTier(_)));
    }

    #[test]
    fn an_unknown_key_is_an_error() {
        let err = AiPolicy::parse("max_teir = \"off\"").unwrap_err();
        assert!(matches!(err, PolicyError::Malformed(_)));
    }

    #[test]
    fn an_absent_policy_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        assert_eq!(AiPolicy::load(&root).unwrap(), None);
    }

    #[test]
    fn the_forbidden_marker_makes_a_directorys_datasets_tier_off() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let dta = root.join("restricted.dta");
        std::fs::write(&dta, b"").unwrap();
        assert_eq!(dataset_tier(Some(&dta), &[]), None);
        std::fs::write(root.join(FORBIDDEN_MARKER), b"").unwrap();
        assert_eq!(dataset_tier(Some(&dta), &[]), Some(PrivacyTier::Off));
    }

    #[test]
    fn a_single_star_does_not_cross_a_directory_separator() {
        // The permissive direction is the dangerous one: `data/*` must not
        // silently cover `data/restricted/secret.dta`.
        assert!(glob_match("data/*.dta", "data/public.dta"));
        assert!(!glob_match("data/*.dta", "data/restricted/secret.dta"));
        assert!(glob_match("data/**/*.dta", "data/restricted/secret.dta"));
        assert!(glob_match("**/*.dta", "secret.dta"));
        assert!(glob_match("/secure/**", "/secure/a/b/c.dta"));
        assert!(!glob_match("/secure/**", "/other/a.dta"));
        assert!(glob_match("a?c.dta", "abc.dta"));
        assert!(!glob_match("a?c.dta", "a/c.dta"));
        assert!(!glob_match("exact.dta", "exact.dtaX"));
    }

    #[test]
    fn a_restricted_path_glob_forces_off() {
        let path = Utf8PathBuf::from("/srv/irb/wave1/health.dta");
        assert_eq!(
            dataset_tier(Some(&path), &["/srv/irb/**".to_owned()]),
            Some(PrivacyTier::Off)
        );
        assert_eq!(
            dataset_tier(Some(&path), &["/srv/public/**".to_owned()]),
            None
        );
    }
}
