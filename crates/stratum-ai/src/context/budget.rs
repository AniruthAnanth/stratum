//! 07 §5.2 and §5.3 — the budget table and the allocation policy.
//!
//! Budget is measured in **estimated input tokens excluding the cached system
//! prefix**, because the cached prefix is close to free after the first request.

use std::time::Duration;

use super::ContextSource;
use crate::provider::types::{Effort, ModelId, Thinking};
use crate::service::surface::Surface;

/// One surface's row of 07 §5.2.
#[derive(Clone, PartialEq, Debug)]
pub struct Budget {
    /// Estimated input tokens available to context, excluding the cached prefix.
    pub context_tokens: u32,
    /// `max_tokens` on the wire.
    pub max_output: u32,
    /// Wall-clock ceiling for the exchange. `None` for `Chat`, whose row reads
    /// "streaming, unbounded" — a long answer the user is watching arrive is not
    /// a hung request.
    pub deadline: Option<Duration>,
    /// The retry chain's total budget.
    pub retry_budget: Duration,
    /// `output_config.effort`.
    pub effort: Effort,
    /// Thinking configuration.
    pub thinking: Thinking,
    /// The model, when the surface has a default. `None` for
    /// [`Surface::GhostCompletion`], which 07 §5.2 leaves without one on
    /// purpose: an 800 ms deadline is not achievable with a thinking model, so
    /// enabling it forces an explicit choice.
    pub model: Option<ModelId>,
}

/// Whether an auto-comment request is one block or a whole file. 07 §5.2 gives
/// the two scopes different rows, and the difference is a factor of ten.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentScope {
    /// One block or one section.
    #[default]
    Block,
    /// A whole file: **one** request returning an array, never N requests.
    File,
}

/// The default model on every surface (ADR D-AI-10).
fn default_model() -> Option<ModelId> {
    Some(ModelId::from(
        crate::provider::backends::anthropic::DEFAULT_MODEL,
    ))
}

impl Budget {
    /// 07 §5.2, transcribed.
    #[must_use]
    pub fn for_surface(surface: Surface, scope: CommentScope) -> Self {
        let adaptive = Thinking::Adaptive {
            show_summary: false,
        };
        match surface {
            Surface::QuickFix => Self {
                context_tokens: 1_500,
                max_output: 600,
                deadline: Some(Duration::from_secs(6)),
                retry_budget: Duration::from_secs(4),
                effort: Effort::Low,
                thinking: adaptive,
                model: default_model(),
            },
            Surface::ResultExplain => Self {
                context_tokens: 6_000,
                max_output: 1_500,
                deadline: Some(Duration::from_secs(60)),
                retry_budget: Duration::from_secs(15),
                effort: Effort::Medium,
                thinking: adaptive,
                model: default_model(),
            },
            Surface::CheckModel => Self {
                context_tokens: 8_000,
                max_output: 2_000,
                deadline: Some(Duration::from_secs(60)),
                retry_budget: Duration::from_secs(15),
                effort: Effort::High,
                thinking: adaptive,
                model: default_model(),
            },
            Surface::NextStep => Self {
                context_tokens: 3_000,
                max_output: 400,
                deadline: Some(Duration::from_secs(10)),
                retry_budget: Duration::from_secs(5),
                effort: Effort::Low,
                thinking: adaptive,
                model: default_model(),
            },
            Surface::GhostCompletion => Self {
                context_tokens: 1_200,
                max_output: 120,
                deadline: Some(Duration::from_millis(800)),
                // 07 §5.2: retry budget 0. There is no room in 800 ms, and a
                // suggestion that arrives after the next keystroke is discarded.
                retry_budget: Duration::ZERO,
                effort: Effort::Low,
                thinking: Thinking::Off,
                // Deliberately none. See the field docs.
                model: None,
            },
            Surface::AutoComment => match scope {
                CommentScope::Block => Self {
                    context_tokens: 4_000,
                    max_output: 800,
                    deadline: Some(Duration::from_secs(45)),
                    retry_budget: Duration::from_secs(10),
                    effort: Effort::Medium,
                    thinking: adaptive,
                    model: default_model(),
                },
                CommentScope::File => Self {
                    context_tokens: 40_000,
                    max_output: 16_000,
                    deadline: Some(Duration::from_secs(180)),
                    retry_budget: Duration::from_secs(30),
                    effort: Effort::High,
                    thinking: adaptive,
                    model: default_model(),
                },
            },
            Surface::ReproExplain => Self {
                context_tokens: 12_000,
                max_output: 4_000,
                deadline: Some(Duration::from_secs(120)),
                retry_budget: Duration::from_secs(20),
                effort: Effort::High,
                thinking: adaptive,
                model: default_model(),
            },
            Surface::HistoryCleanup => Self {
                context_tokens: 8_000,
                max_output: 3_000,
                deadline: Some(Duration::from_secs(60)),
                retry_budget: Duration::from_secs(15),
                effort: Effort::Medium,
                thinking: adaptive,
                model: default_model(),
            },
            Surface::Chat => Self {
                context_tokens: 60_000,
                max_output: 16_000,
                deadline: None,
                retry_budget: Duration::from_secs(20),
                effort: Effort::High,
                // The one surface where the user is watching, so the thinking
                // summary is progress rather than a silent pause.
                thinking: Thinking::Adaptive { show_summary: true },
                model: default_model(),
            },
        }
    }

    /// The Fast-profile override of 07 §5.2: opt-in, one click, and it maps only
    /// the three latency-sensitive surfaces.
    #[must_use]
    pub fn fast_profile(mut self, surface: Surface) -> Self {
        if matches!(
            surface,
            Surface::QuickFix | Surface::NextStep | Surface::GhostCompletion
        ) {
            self.model = Some(ModelId::from(
                crate::provider::backends::anthropic::FAST_MODEL,
            ));
        }
        self
    }
}

/// One row of 07 §5.3's allocation table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CategoryCap {
    /// Which context source.
    pub source: ContextSource,
    /// Fill order; lower is placed first.
    pub priority: u8,
    /// Ceiling as a percentage of the surface budget. The caps deliberately sum
    /// to more than 100: they are ceilings, not reservations, and the fill
    /// simply stops at the budget.
    pub cap_pct: u8,
    /// Below this many tokens the category is **dropped entirely**, not
    /// truncated. Three of 3,127 variables is not context, it is misleading
    /// context.
    pub min_useful: u32,
    /// True for the one category that is reserved before anything else is
    /// placed: the focus. If the focus alone exceeds the budget it is centred on
    /// the cursor and elided in the middle, and every other category is dropped.
    pub hard_reserve: bool,
}

/// 07 §5.3, transcribed. Category 0 (the task instruction) is not here: it is
/// the cached prefix, it is never trimmed, and it is outside the budget.
pub const CATEGORIES: &[CategoryCap] = &[
    CategoryCap {
        source: ContextSource::Focus,
        priority: 1,
        cap_pct: 35,
        min_useful: 1,
        hard_reserve: true,
    },
    CategoryCap {
        source: ContextSource::Errors,
        priority: 2,
        cap_pct: 10,
        min_useful: 1,
        hard_reserve: false,
    },
    CategoryCap {
        source: ContextSource::Variables,
        priority: 3,
        cap_pct: 30,
        min_useful: 40,
        hard_reserve: false,
    },
    CategoryCap {
        source: ContextSource::Estimates,
        priority: 4,
        cap_pct: 15,
        min_useful: 1,
        hard_reserve: false,
    },
    CategoryCap {
        source: ContextSource::Macros,
        priority: 5,
        cap_pct: 5,
        min_useful: 1,
        hard_reserve: false,
    },
    CategoryCap {
        source: ContextSource::Block,
        priority: 6,
        cap_pct: 25,
        min_useful: 1,
        hard_reserve: false,
    },
    CategoryCap {
        source: ContextSource::Files,
        priority: 7,
        cap_pct: 15,
        min_useful: 0,
        hard_reserve: false,
    },
    // Session metadata is a handful of tokens and is never worth dropping; it is
    // what stops the model asserting that a variable does not exist.
    CategoryCap {
        source: ContextSource::Session,
        priority: 0,
        cap_pct: 5,
        min_useful: 1,
        hard_reserve: false,
    },
];

/// The cap for a source, in tokens.
#[must_use]
pub fn cap_tokens(cap: CategoryCap, budget: u32) -> u32 {
    (u64::from(budget) * u64::from(cap.cap_pct) / 100) as u32
}

/// The row for a source, if it has one.
#[must_use]
pub fn category(source: ContextSource) -> Option<CategoryCap> {
    CATEGORIES.iter().copied().find(|c| c.source == source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghost_completion_has_no_default_model_and_no_retry() {
        // ADR D-AI-11. Enabling it forces an explicit model choice, because an
        // 800 ms deadline is not achievable with a thinking model.
        let b = Budget::for_surface(Surface::GhostCompletion, CommentScope::Block);
        assert!(b.model.is_none());
        assert_eq!(b.retry_budget, Duration::ZERO);
        assert_eq!(b.thinking, Thinking::Off);
        assert_eq!(b.deadline, Some(Duration::from_millis(800)));
    }

    #[test]
    fn every_other_surface_defaults_to_opus_and_is_never_silently_downgraded() {
        // ADR D-AI-10: the default model is the same everywhere; a cheaper model
        // is the user's call, through the opt-in Fast profile.
        for s in Surface::ALL {
            if s == Surface::GhostCompletion {
                continue;
            }
            let b = Budget::for_surface(s, CommentScope::Block);
            assert_eq!(
                b.model.as_ref().map(ModelId::as_str),
                Some("claude-opus-5"),
                "{s}"
            );
        }
    }

    #[test]
    fn the_fast_profile_maps_exactly_three_surfaces() {
        for s in Surface::ALL {
            let base = Budget::for_surface(s, CommentScope::Block);
            let fast = base.clone().fast_profile(s);
            let changed = base.model != fast.model;
            let expected = matches!(
                s,
                Surface::QuickFix | Surface::NextStep | Surface::GhostCompletion
            );
            assert_eq!(changed, expected, "{s}");
        }
    }

    #[test]
    fn file_scope_auto_comment_is_an_order_of_magnitude_larger_than_block_scope() {
        let block = Budget::for_surface(Surface::AutoComment, CommentScope::Block);
        let file = Budget::for_surface(Surface::AutoComment, CommentScope::File);
        assert_eq!(block.context_tokens, 4_000);
        assert_eq!(file.context_tokens, 40_000);
        assert_eq!(file.max_output, 16_000);
    }

    #[test]
    fn chat_is_the_only_surface_with_no_deadline() {
        for s in Surface::ALL {
            let b = Budget::for_surface(s, CommentScope::Block);
            assert_eq!(b.deadline.is_none(), s == Surface::Chat, "{s}");
        }
    }

    #[test]
    fn the_focus_is_the_only_hard_reserve() {
        let reserved: Vec<_> = CATEGORIES.iter().filter(|c| c.hard_reserve).collect();
        assert_eq!(reserved.len(), 1);
        assert_eq!(reserved[0].source, ContextSource::Focus);
        assert_eq!(reserved[0].cap_pct, 35);
    }

    #[test]
    fn the_caps_deliberately_sum_to_more_than_one_hundred_percent() {
        // They are ceilings, not reservations, and the fill stops at the budget.
        let total: u32 = CATEGORIES.iter().map(|c| u32::from(c.cap_pct)).sum();
        assert!(total > 100, "{total}");
    }

    #[test]
    fn cap_tokens_is_a_percentage_of_the_surface_budget() {
        let variables = category(ContextSource::Variables).unwrap();
        assert_eq!(cap_tokens(variables, 6_000), 1_800);
    }
}
