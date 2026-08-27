//! `07` §12 — the first-class type the UI codes against.
//!
//! # The unconfigured state is the base product, not a degraded one
//!
//! Most users are in it on first launch and a substantial fraction stay in it
//! permanently: institutional policy, restricted data, no budget. All six
//! variants below therefore carry the same obligations — a headline, a detail
//! sentence, and where there is something to do, exactly what. None of them is
//! allowed to render as an empty panel or a disabled control with no
//! explanation, and [`tests::every_variant_renders_distinctly`] is that rule as
//! an assertion.
//!
//! # Why six and not "configured / not configured"
//!
//! Because the six have six different answers to "what do I do about it", and
//! collapsing them produces the worst UI in this space: a greyed-out button that
//! says "AI unavailable". "Your project policy forbids it", "you are over your
//! own cap", and "the provider is not answering" are three different
//! conversations, and only one of them is about setting up an API key.

use serde::{Deserialize, Serialize};

use crate::context::tiers::PrivacyTier;
use crate::provider::egress::NetworkMode;
use crate::provider::types::{ModelId, ProviderId};
use crate::tasks::cost::BudgetVerdict;

/// What the AI stack can do right now.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Availability {
    /// A provider is configured and reachable.
    Configured {
        /// Which backend.
        provider: ProviderId,
        /// Which model.
        model: ModelId,
        /// The effective tier for a surface with no ceiling of its own.
        tier: PrivacyTier,
    },
    /// Offline mode is on and a local provider is available. Not a lesser state:
    /// for a researcher under an IRB constraint it is the only acceptable one.
    OfflineOnly {
        /// The local backend.
        provider: ProviderId,
        /// Which model.
        model: ModelId,
    },
    /// No key anywhere: not in the environment, not in the OS store, not in a
    /// key file. **The default, and the product is fully functional in it.**
    NoCredential,
    /// A committed `.stratum/ai-policy.toml` forbids what was asked.
    DisabledByProjectPolicy {
        /// What the policy said, in the user's terms.
        reason: String,
    },
    /// One of the three caps in `07` §11.2 bound.
    OverBudget {
        /// Which cap, and what it is.
        verdict: BudgetVerdict,
    },
    /// The provider was configured and did not answer.
    ProviderUnreachable {
        /// When we last failed to reach it, Unix milliseconds (A2).
        since_unix_ms: u64,
        /// What the transport said, already scrubbed.
        detail: String,
    },
}

/// A stable key per variant, for the UI's `data-` attribute, the audit log and
/// the test that the six are distinguishable.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityKind {
    /// [`Availability::Configured`].
    Configured,
    /// [`Availability::OfflineOnly`].
    OfflineOnly,
    /// [`Availability::NoCredential`].
    NoCredential,
    /// [`Availability::DisabledByProjectPolicy`].
    DisabledByProjectPolicy,
    /// [`Availability::OverBudget`].
    OverBudget,
    /// [`Availability::ProviderUnreachable`].
    ProviderUnreachable,
}

impl AvailabilityKind {
    /// Every kind, in the order the settings pane documents them.
    pub const ALL: [Self; 6] = [
        Self::Configured,
        Self::OfflineOnly,
        Self::NoCredential,
        Self::DisabledByProjectPolicy,
        Self::OverBudget,
        Self::ProviderUnreachable,
    ];

    /// The stable key.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::OfflineOnly => "offline_only",
            Self::NoCredential => "no_credential",
            Self::DisabledByProjectPolicy => "disabled_by_project_policy",
            Self::OverBudget => "over_budget",
            Self::ProviderUnreachable => "provider_unreachable",
        }
    }
}

/// Something the user can do about this state.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Remedy {
    /// The control's label.
    pub label: String,
    /// The command id the shell dispatches. Never a URL: an availability state
    /// must not be able to open the network.
    pub command: String,
}

impl Availability {
    /// Which variant.
    #[must_use]
    pub const fn kind(&self) -> AvailabilityKind {
        match self {
            Self::Configured { .. } => AvailabilityKind::Configured,
            Self::OfflineOnly { .. } => AvailabilityKind::OfflineOnly,
            Self::NoCredential => AvailabilityKind::NoCredential,
            Self::DisabledByProjectPolicy { .. } => AvailabilityKind::DisabledByProjectPolicy,
            Self::OverBudget { .. } => AvailabilityKind::OverBudget,
            Self::ProviderUnreachable { .. } => AvailabilityKind::ProviderUnreachable,
        }
    }

    /// Whether a request may be issued at all.
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        matches!(self, Self::Configured { .. } | Self::OfflineOnly { .. })
    }

    /// The line the panel's status strip shows.
    #[must_use]
    pub fn headline(&self) -> String {
        match self {
            Self::Configured {
                provider,
                model,
                tier,
            } => {
                format!("{provider} · {model} · {}", tier.title())
            }
            Self::OfflineOnly { provider, model } => format!("Offline AI · {provider} · {model}"),
            Self::NoCredential => "No AI provider configured".to_owned(),
            Self::DisabledByProjectPolicy { .. } => "AI disabled by this project".to_owned(),
            Self::OverBudget { .. } => "AI paused — limit reached".to_owned(),
            Self::ProviderUnreachable { .. } => "Provider not answering".to_owned(),
        }
    }

    /// The sentence under it. Never empty for any variant — that is `07` §12's
    /// "must never be a broken or empty UI", made checkable.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Configured { tier, .. } => tier.describes().to_owned(),
            Self::OfflineOnly { .. } => {
                "Requests can only reach a model running on this machine. Nothing leaves it."
                    .to_owned()
            }
            Self::NoCredential => {
                "Everything except the AI surfaces works exactly as it does with a key: \
                 completion, quick fixes, lints, all ten reproducibility checks, model \
                 comparison, Document View and the whole command line."
                    .to_owned()
            }
            // Both of these carry text from somewhere else — a committed TOML
            // file, a transport error — and neither source is obliged to hand us
            // a sentence. Framing it here rather than trusting every construction
            // site is what makes "never an empty or broken panel" a property of
            // the type instead of a rule authors have to remember.
            Self::DisabledByProjectPolicy { reason } => {
                format!(
                    "{} This project's committed AI policy decides it, not this \
                     machine's settings; the policy lives in {}.",
                    as_sentence(reason),
                    crate::context::policy::POLICY_FILE
                )
            }
            Self::OverBudget { verdict } => verdict.explain(),
            Self::ProviderUnreachable { detail, .. } => {
                format!(
                    "{} Nothing was sent beyond the attempt that failed, and every \
                     non-AI surface is unaffected.",
                    as_sentence(detail)
                )
            }
        }
    }

    /// What the user can do, when there is something.
    #[must_use]
    pub fn remedy(&self) -> Option<Remedy> {
        match self {
            Self::Configured { .. } | Self::OfflineOnly { .. } => None,
            Self::NoCredential => Some(Remedy {
                label: "Set up a provider".to_owned(),
                command: "ai.setup".to_owned(),
            }),
            // Deliberately none. A committed policy that a collaborator could
            // click past would not be a policy (D-AI-04); the only remedy is a
            // conversation with whoever committed it.
            Self::DisabledByProjectPolicy { .. } => None,
            Self::OverBudget { .. } => Some(Remedy {
                label: "Raise the limit".to_owned(),
                command: "ai.settings.budgets".to_owned(),
            }),
            Self::ProviderUnreachable { .. } => Some(Remedy {
                label: "Test connection".to_owned(),
                command: "ai.health".to_owned(),
            }),
        }
    }
}

/// Punctuate a fragment so it can begin a paragraph.
///
/// A policy file may say `max_tier = off` and a transport may say
/// `connection refused`; both are true and neither is a sentence.
fn as_sentence(fragment: &str) -> String {
    let t = fragment.trim();
    if t.is_empty() {
        return String::new();
    }
    if t.ends_with(['.', '!', '?']) {
        t.to_owned()
    } else {
        format!("{t}.")
    }
}

/// Everything [`compute`] needs to decide.
#[derive(Clone, PartialEq, Debug)]
pub struct AvailabilityInputs {
    /// The configured backend.
    pub provider: ProviderId,
    /// The configured model.
    pub model: ModelId,
    /// Whether a credential resolved. Never the credential itself: this struct
    /// is `Debug`, and a key must never be printable.
    pub has_credential: bool,
    /// Whether the backend needs the network at all.
    pub requires_network: bool,
    /// The global network mode, after any project override.
    pub network: NetworkMode,
    /// The effective tier at a surface with no ceiling.
    pub tier: PrivacyTier,
    /// `Some` when a committed policy forbids this provider outright.
    pub policy_block: Option<String>,
    /// The budget check.
    pub budget: BudgetVerdict,
    /// `Some` when the last health check failed.
    pub unreachable: Option<(u64, String)>,
}

/// Fold the inputs into one state.
///
/// Order matters and it is the order of *finality*: a project policy cannot be
/// clicked past, a missing key cannot be budgeted around, and a provider that is
/// not answering is only interesting once we know we were allowed to call it.
#[must_use]
pub fn compute(inputs: &AvailabilityInputs) -> Availability {
    if let Some(reason) = &inputs.policy_block {
        return Availability::DisabledByProjectPolicy {
            reason: reason.clone(),
        };
    }
    // Offline mode plus a backend that needs the network is a policy answer, not
    // a credential answer: the key may well be right there in the Keychain.
    if inputs.network == NetworkMode::Offline && inputs.requires_network {
        return Availability::DisabledByProjectPolicy {
            reason: format!(
                "Offline AI is on, and {} needs the network. Point the assistant at a local \
                 model, or turn offline mode off in Settings › AI.",
                inputs.provider
            ),
        };
    }
    // A local daemon on loopback has no credential to be missing.
    if inputs.requires_network && !inputs.has_credential {
        return Availability::NoCredential;
    }
    if !inputs.budget.allowed() {
        return Availability::OverBudget {
            verdict: inputs.budget.clone(),
        };
    }
    if let Some((since_unix_ms, detail)) = &inputs.unreachable {
        return Availability::ProviderUnreachable {
            since_unix_ms: *since_unix_ms,
            detail: detail.clone(),
        };
    }
    if inputs.network == NetworkMode::Offline {
        return Availability::OfflineOnly {
            provider: inputs.provider,
            model: inputs.model.clone(),
        };
    }
    Availability::Configured {
        provider: inputs.provider,
        model: inputs.model.clone(),
        tier: inputs.tier,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> AvailabilityInputs {
        AvailabilityInputs {
            provider: ProviderId::Anthropic,
            model: ModelId::from("claude-opus-5"),
            has_credential: true,
            requires_network: true,
            network: NetworkMode::Enabled,
            tier: PrivacyTier::SchemaOnly,
            policy_block: None,
            budget: BudgetVerdict::Allowed,
            unreachable: None,
        }
    }

    fn one_of_each() -> Vec<Availability> {
        vec![
            Availability::Configured {
                provider: ProviderId::Anthropic,
                model: ModelId::from("claude-opus-5"),
                tier: PrivacyTier::SchemaOnly,
            },
            Availability::OfflineOnly {
                provider: ProviderId::Ollama,
                model: ModelId::from("qwen2.5-coder:7b"),
            },
            Availability::NoCredential,
            Availability::DisabledByProjectPolicy {
                reason: "max_tier = off".to_owned(),
            },
            Availability::OverBudget {
                verdict: BudgetVerdict::SessionCapReached { cap: 200 },
            },
            Availability::ProviderUnreachable {
                since_unix_ms: 1,
                detail: "connection refused".to_owned(),
            },
        ]
    }

    #[test]
    fn every_variant_renders_distinctly() {
        // The acceptance bullet. Six states, six headlines, six details, six
        // stable keys — none empty, none shared.
        let all = one_of_each();
        assert_eq!(all.len(), AvailabilityKind::ALL.len());

        let mut kinds: Vec<&str> = all.iter().map(|a| a.kind().as_str()).collect();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(kinds.len(), 6, "two variants share a key");

        let mut headlines: Vec<String> = all.iter().map(Availability::headline).collect();
        headlines.sort();
        headlines.dedup();
        assert_eq!(headlines.len(), 6, "two variants share a headline");

        let mut details: Vec<String> = all.iter().map(Availability::detail).collect();
        for d in &details {
            assert!(d.len() > 30, "a placeholder detail: {d:?}");
        }
        details.sort();
        details.dedup();
        assert_eq!(details.len(), 6, "two variants share a detail");
    }

    #[test]
    fn only_the_two_working_states_are_usable() {
        for a in one_of_each() {
            let expect = matches!(
                a.kind(),
                AvailabilityKind::Configured | AvailabilityKind::OfflineOnly
            );
            assert_eq!(a.is_usable(), expect, "{:?}", a.kind());
        }
    }

    #[test]
    fn a_committed_policy_offers_no_click_past() {
        // D-AI-04: a user cannot override a policy file in their own clone, by
        // design. A remedy button here would be a lie.
        let a = Availability::DisabledByProjectPolicy {
            reason: "restricted".to_owned(),
        };
        assert!(a.remedy().is_none());
    }

    #[test]
    fn the_unconfigured_state_describes_what_still_works() {
        // 07 §12: it is not a degraded mode, it is the base product, and the
        // copy has to say so rather than nagging.
        let d = Availability::NoCredential.detail();
        for kept in ["completion", "reproducibility", "command line"] {
            assert!(d.contains(kept), "{d}");
        }
        assert!(!d.contains("upgrade") && !d.contains("unlock"));
    }

    #[test]
    fn a_policy_block_outranks_a_missing_key() {
        let mut i = inputs();
        i.has_credential = false;
        i.policy_block = Some("this project forbids network AI".to_owned());
        assert_eq!(
            compute(&i).kind(),
            AvailabilityKind::DisabledByProjectPolicy
        );
    }

    #[test]
    fn offline_mode_with_a_cloud_backend_is_a_policy_answer_not_a_key_answer() {
        // The key may well be sitting in the Keychain. Saying "no credential"
        // would send the user to fix something that is not broken.
        let mut i = inputs();
        i.network = NetworkMode::Offline;
        let a = compute(&i);
        assert_eq!(a.kind(), AvailabilityKind::DisabledByProjectPolicy);
        assert!(a.detail().contains("Offline AI is on"));
    }

    #[test]
    fn a_local_provider_needs_no_credential() {
        let mut i = inputs();
        i.provider = ProviderId::Ollama;
        i.model = ModelId::from("qwen2.5-coder:7b");
        i.has_credential = false;
        i.requires_network = false;
        i.network = NetworkMode::Offline;
        assert_eq!(compute(&i).kind(), AvailabilityKind::OfflineOnly);
    }

    #[test]
    fn the_budget_is_checked_before_reachability() {
        // Being over a cap is a fact; being unreachable is a guess about a
        // network we have decided not to use.
        let mut i = inputs();
        i.budget = BudgetVerdict::SessionCapReached { cap: 200 };
        i.unreachable = Some((1, "timeout".to_owned()));
        assert_eq!(compute(&i).kind(), AvailabilityKind::OverBudget);
    }

    #[test]
    fn the_happy_path_is_configured() {
        assert_eq!(compute(&inputs()).kind(), AvailabilityKind::Configured);
    }
}
