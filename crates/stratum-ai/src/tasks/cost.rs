//! `07` §11.1–§11.2 — what a request cost, and the three caps.
//!
//! # Every limit failure is visible and explained
//!
//! That is the whole design rule for this module. A tool that quietly stops
//! being intelligent is worse than one that tells you why: the user does not
//! conclude "I hit my cap", they conclude "the AI is broken", and they are right
//! to, because from where they sit those are the same observation.
//!
//! So [`BudgetVerdict`] is never a bare `false`. It names which cap bound, what
//! the cap is, and what the user can do — and the panel renders that sentence
//! next to a one-click raise.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::provider::types::{ModelId, ProviderId, TokenUsage};

/// The shipped price table, embedded at build time so the headless CLI can
/// estimate a cost with no resource directory (`07` §13.6).
pub const SHIPPED: &str = include_str!("../../ai-pricing.toml");

/// Per-million-token rates for one model.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct ModelPrice {
    /// Uncached input.
    #[serde(default)]
    pub input_per_mtok: f64,
    /// Generated output.
    #[serde(default)]
    pub output_per_mtok: f64,
    /// Writing the prompt cache. Defaults to the input rate when the provider
    /// has a cache and we do not know its multiplier — over-estimating is the
    /// safe direction for a number the user budgets against.
    #[serde(default)]
    pub cache_write_per_mtok: Option<f64>,
    /// Reading the prompt cache.
    #[serde(default)]
    pub cache_read_per_mtok: Option<f64>,
    /// True for a provider whose price we genuinely cannot know — an
    /// institutional gateway, a private vLLM. The UI shows "cost unknown"
    /// rather than "$0.00", which is a different and much worse claim.
    #[serde(default)]
    pub estimate_unavailable: bool,
}

impl ModelPrice {
    /// What one request cost, in US dollars.
    #[must_use]
    pub fn cost_usd(&self, usage: TokenUsage) -> f64 {
        let per = |tokens: u32, rate: f64| f64::from(tokens) * rate / 1_000_000.0;
        let write = self.cache_write_per_mtok.unwrap_or(self.input_per_mtok);
        let read = self.cache_read_per_mtok.unwrap_or(self.input_per_mtok);
        per(usage.input, self.input_per_mtok)
            + per(usage.output, self.output_per_mtok)
            + per(usage.cache_write, write)
            + per(usage.cache_read, read)
    }
}

/// The parsed price table.
///
/// Walked out of a [`toml::Table`] by hand rather than derived with
/// `#[serde(flatten)]`: flatten forces the whole document through serde's
/// buffering `Content` type, which turns a mistyped rate from a pointed
/// "invalid type: string, expected f64 for `input_per_mtok`" into "data did not
/// match any variant". This file is meant to be edited by users, so the error
/// message is part of the feature.
#[derive(Clone, PartialEq, Debug, Default, Serialize)]
pub struct PriceTable {
    /// Schema version.
    pub version: u32,
    /// The date the rates were transcribed, verbatim, so the UI can say
    /// "rates as shipped on <date>; verify with your provider".
    pub as_of: String,
    /// `provider -> model -> price`. `"*"` is the fallback within a provider.
    pub providers: BTreeMap<String, BTreeMap<String, ModelPrice>>,
}

/// A price table that could not be read.
#[derive(Debug, thiserror::Error)]
pub enum PriceError {
    /// Not valid TOML, or not the expected shape.
    #[error("ai-pricing.toml: {0}")]
    Malformed(String),
}

impl PriceTable {
    /// Parse a table.
    ///
    /// # Errors
    /// [`PriceError::Malformed`].
    pub fn parse(text: &str) -> Result<Self, PriceError> {
        let doc: toml::Table =
            toml::from_str(text).map_err(|e| PriceError::Malformed(e.to_string()))?;
        let mut table = Self {
            version: doc
                .get("version")
                .and_then(toml::Value::as_integer)
                .unwrap_or(0) as u32,
            as_of: doc
                .get("as_of")
                .and_then(toml::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            providers: BTreeMap::new(),
        };
        for (provider, value) in &doc {
            let Some(models) = value.as_table() else {
                continue; // `version` and `as_of`, which are not provider rows.
            };
            let mut priced = BTreeMap::new();
            for (model, row) in models {
                let price: ModelPrice = row
                    .clone()
                    .try_into()
                    .map_err(|e| PriceError::Malformed(format!("[{provider}.\"{model}\"]: {e}")))?;
                priced.insert(model.clone(), price);
            }
            table.providers.insert(provider.clone(), priced);
        }
        Ok(table)
    }

    /// The table shipped with this build.
    ///
    /// # Panics
    /// Never: `tests::the_shipped_table_parses` proves the embedded bytes parse,
    /// and they cannot change between that test and this call.
    #[must_use]
    pub fn shipped() -> Self {
        Self::parse(SHIPPED).expect("the embedded price table is checked by a test")
    }

    /// Load a user-edited table, falling back to the shipped one.
    ///
    /// A malformed user table is a warning, not a failure: a typo in a price
    /// file must not disable the AI panel, and the shipped estimate is a better
    /// answer than no estimate.
    #[must_use]
    pub fn load_or_shipped(path: &camino::Utf8Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match Self::parse(&text) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(%path, error = %e, "falling back to the shipped price table");
                    Self::shipped()
                }
            },
            Err(_) => Self::shipped(),
        }
    }

    /// The price for a model, falling back to the provider's `"*"` row.
    #[must_use]
    pub fn price(&self, provider: ProviderId, model: &ModelId) -> ModelPrice {
        let Some(models) = self.providers.get(provider.as_str()) else {
            return ModelPrice {
                estimate_unavailable: true,
                ..ModelPrice::default()
            };
        };
        models
            .get(model.as_str())
            .or_else(|| models.get("*"))
            .copied()
            .unwrap_or(ModelPrice {
                estimate_unavailable: true,
                ..ModelPrice::default()
            })
    }
}

/// `07` §11.2's three caps.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Budgets {
    /// Hard. The packer cannot exceed it; if the focus alone would, the request
    /// is refused with a clear message rather than truncated into nonsense.
    pub per_request_token_cap: u32,
    /// Default 200. On hit, AI surfaces disable with a banner and a one-click
    /// raise. Never silent.
    pub per_session_request_cap: u32,
    /// Unset by default. When unset we surface a passive notification at each
    /// $10 of cumulative estimated spend; when set, exceeding it disables
    /// network AI until an explicit override.
    pub monthly_usd_cap: Option<f64>,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            // The largest surface budget in 07 §5.2 is Chat's 60 000 context
            // tokens plus 16 000 of output; 80 000 leaves headroom for the
            // cached prefix without letting a runaway file-scope request through.
            per_request_token_cap: 80_000,
            per_session_request_cap: 200,
            monthly_usd_cap: None,
        }
    }
}

/// The passive-notification step when no monthly cap is set.
pub const PASSIVE_NOTICE_STEP_USD: f64 = 10.0;

/// Whether a request may proceed, and if not, exactly why.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum BudgetVerdict {
    /// Go ahead.
    Allowed,
    /// The prompt is larger than the per-request cap.
    TooLarge {
        /// What the packer produced.
        est_tokens: u32,
        /// The cap it exceeded.
        cap: u32,
    },
    /// The session has made its allowance of requests.
    SessionCapReached {
        /// The cap.
        cap: u32,
    },
    /// Estimated spend this month is over the user's cap.
    MonthlyCapReached {
        /// Spent so far, estimated.
        spent_usd: f64,
        /// The cap.
        cap_usd: f64,
    },
}

impl BudgetVerdict {
    /// Whether the request may proceed.
    #[must_use]
    pub const fn allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// The sentence the banner shows. Always names the cap and always says what
    /// to do — "AI is unavailable" with no reason is the failure this exists to
    /// prevent.
    #[must_use]
    pub fn explain(&self) -> String {
        match self {
            Self::Allowed => String::new(),
            Self::TooLarge { est_tokens, cap } => format!(
                "This request needs about {est_tokens} tokens of context and the per-request \
                 limit is {cap}. Narrow the selection, or raise the limit in Settings › AI."
            ),
            Self::SessionCapReached { cap } => format!(
                "This session has made its {cap} AI requests. Raise the limit in Settings › AI, \
                 or start a new session."
            ),
            Self::MonthlyCapReached { spent_usd, cap_usd } => format!(
                "Estimated spend this month is ${spent_usd:.2}, over your ${cap_usd:.2} cap. \
                 Raise or clear the cap in Settings › AI. Local models are not counted."
            ),
        }
    }
}

/// Check a request against the caps.
#[must_use]
pub fn check(
    budgets: Budgets,
    est_tokens: u32,
    requests_this_session: u32,
    spent_this_month_usd: f64,
) -> BudgetVerdict {
    if est_tokens > budgets.per_request_token_cap {
        return BudgetVerdict::TooLarge {
            est_tokens,
            cap: budgets.per_request_token_cap,
        };
    }
    if requests_this_session >= budgets.per_session_request_cap {
        return BudgetVerdict::SessionCapReached {
            cap: budgets.per_session_request_cap,
        };
    }
    if let Some(cap) = budgets.monthly_usd_cap {
        if spent_this_month_usd >= cap {
            return BudgetVerdict::MonthlyCapReached {
                spent_usd: spent_this_month_usd,
                cap_usd: cap,
            };
        }
    }
    BudgetVerdict::Allowed
}

/// Running totals for the panel footer.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct CostSummary {
    /// Requests this session.
    pub requests: u32,
    /// Tokens this session.
    pub usage: TokenUsage,
    /// Estimated dollars this session.
    pub est_cost_usd: f64,
    /// Response-cache hits.
    pub cache_hits: u64,
    /// Response-cache misses.
    pub cache_misses: u64,
    /// True when any request used a provider whose price we cannot know, so the
    /// footer says "at least $x" rather than "$x".
    pub cost_incomplete: bool,
}

impl CostSummary {
    /// Cache hit rate in `[0, 1]`; zero when nothing has been asked yet.
    #[must_use]
    pub fn cache_hit_rate(&self) -> f64 {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 {
            0.0
        } else {
            self.cache_hits as f64 / total as f64
        }
    }

    /// Fold one completed request in.
    pub fn record(&mut self, usage: TokenUsage, price: ModelPrice) {
        self.requests += 1;
        self.usage.input += usage.input;
        self.usage.output += usage.output;
        self.usage.cache_write += usage.cache_write;
        self.usage.cache_read += usage.cache_read;
        self.est_cost_usd += price.cost_usd(usage);
        self.cost_incomplete |= price.estimate_unavailable;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_table_parses_and_names_the_default_model() {
        let t = PriceTable::shipped();
        assert_eq!(t.version, 1);
        assert!(
            !t.as_of.is_empty(),
            "the UI has to be able to say when these rates are from"
        );
        let p = t.price(ProviderId::Anthropic, &ModelId::from("claude-opus-5"));
        assert!((p.input_per_mtok - 5.00).abs() < f64::EPSILON);
        assert!((p.output_per_mtok - 25.00).abs() < f64::EPSILON);
    }

    #[test]
    fn an_unknown_anthropic_model_is_priced_at_the_wildcard_not_at_zero() {
        // A cost estimate that silently reads "$0.00" for a model we do not know
        // is worse than no estimate, because the user believes it.
        let t = PriceTable::shipped();
        let p = t.price(
            ProviderId::Anthropic,
            &ModelId::from("claude-something-new"),
        );
        assert!(p.input_per_mtok > 0.0);
        assert!(!p.estimate_unavailable);
    }

    #[test]
    fn a_local_model_is_free_and_a_gateway_is_unknown() {
        let t = PriceTable::shipped();
        let ollama = t.price(ProviderId::Ollama, &ModelId::from("qwen2.5-coder:7b"));
        assert!(
            (ollama.cost_usd(TokenUsage {
                input: 1_000_000,
                ..TokenUsage::default()
            }))
            .abs()
                < f64::EPSILON
        );
        assert!(
            !ollama.estimate_unavailable,
            "zero is the price, not an absence of one"
        );

        let gateway = t.price(ProviderId::OpenAiCompat, &ModelId::from("gpt-4o-mini"));
        assert!(
            gateway.estimate_unavailable,
            "we cannot know a gateway's rates"
        );
    }

    #[test]
    fn cost_counts_cache_reads_at_the_cache_rate() {
        let t = PriceTable::shipped();
        let p = t.price(ProviderId::Anthropic, &ModelId::from("claude-opus-5"));
        let usage = TokenUsage {
            input: 0,
            output: 0,
            cache_write: 0,
            cache_read: 1_000_000,
        };
        assert!(
            (p.cost_usd(usage) - 0.50).abs() < 1e-9,
            "{}",
            p.cost_usd(usage)
        );
    }

    #[test]
    fn every_cap_explains_itself() {
        // A tool that quietly stops being intelligent is worse than one that
        // tells you why.
        let verdicts = [
            BudgetVerdict::TooLarge {
                est_tokens: 90_000,
                cap: 80_000,
            },
            BudgetVerdict::SessionCapReached { cap: 200 },
            BudgetVerdict::MonthlyCapReached {
                spent_usd: 12.5,
                cap_usd: 10.0,
            },
        ];
        for v in verdicts {
            assert!(!v.allowed());
            let msg = v.explain();
            assert!(msg.len() > 40, "{v:?} has a placeholder explanation");
            assert!(msg.contains("Settings"), "{v:?} must say what to do");
        }
        assert!(BudgetVerdict::Allowed.allowed());
        assert!(BudgetVerdict::Allowed.explain().is_empty());
    }

    #[test]
    fn the_caps_are_checked_in_the_order_the_user_can_act_on() {
        let b = Budgets {
            monthly_usd_cap: Some(1.0),
            ..Budgets::default()
        };
        // Too large wins over the session cap: it is the one specific to this
        // request and the one a narrower selection fixes.
        assert!(matches!(
            check(b, 1_000_000, 999, 99.0),
            BudgetVerdict::TooLarge { .. }
        ));
        assert!(matches!(
            check(b, 10, 999, 99.0),
            BudgetVerdict::SessionCapReached { .. }
        ));
        assert!(matches!(
            check(b, 10, 0, 99.0),
            BudgetVerdict::MonthlyCapReached { .. }
        ));
        assert!(check(b, 10, 0, 0.0).allowed());
    }

    #[test]
    fn no_monthly_cap_means_no_monthly_block() {
        let b = Budgets::default();
        assert!(b.monthly_usd_cap.is_none());
        assert!(check(b, 10, 0, 10_000.0).allowed());
    }

    #[test]
    fn a_summary_reports_an_incomplete_estimate_rather_than_an_understated_one() {
        let t = PriceTable::shipped();
        let mut s = CostSummary::default();
        s.record(
            TokenUsage {
                input: 1_000,
                output: 100,
                ..TokenUsage::default()
            },
            t.price(ProviderId::Anthropic, &ModelId::from("claude-opus-5")),
        );
        assert!(!s.cost_incomplete);
        s.record(
            TokenUsage {
                input: 1_000,
                output: 100,
                ..TokenUsage::default()
            },
            t.price(ProviderId::OpenAiCompat, &ModelId::from("whatever")),
        );
        assert!(s.cost_incomplete);
        assert_eq!(s.requests, 2);
        assert_eq!(s.usage.input, 2_000);
    }

    #[test]
    fn the_hit_rate_is_zero_before_anything_is_asked() {
        assert!((CostSummary::default().cache_hit_rate()).abs() < f64::EPSILON);
    }
}
