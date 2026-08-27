//! Stratum's LLM stack — `07`, ARCHITECTURE §5, ADR-012.
//!
//! # This crate is optional, and the product is designed for its absence
//!
//! `07` §0 is the governing decision and it is not a hedge: most of the
//! perceived intelligence of the product is deterministic and lives in
//! `stratum-intel`. With no API key the product loses **10** of `07` §0.1's 27
//! intelligence surfaces and keeps **17**, plus 100 % of execution, editing and
//! reproducibility. [`service::surface::INTELLIGENCE_SURFACES`] is that table as
//! data, so the claim is a test rather than a sentence.
//!
//! Nothing here is on the path of a core interaction. Every request is
//! cancellable, every surface has a defined unconfigured state, and the one
//! thing this crate must never do is make a keystroke wait.
//!
//! # The four seams, and why they are four
//!
//! ```text
//!   provider/   HTTP, credentials, streaming, retries, cancellation.
//!               Knows nothing about Stata, prompts, or privacy tiers.
//!   context/    What leaves the machine — and nothing else. Typed
//!               `ContextItem`s with a `min_tier`, one `filter` gate, the
//!               budget fill, the renderer, the audit log.
//!   tasks/      Versioned prompts, strict output parsers, cost, cache.
//!   service/    The orchestrator the UI and the CLI both talk to.
//! ```
//!
//! Exactly one type crosses from `context` into `provider`
//! ([`provider::ChatRequest`]), and by the time it exists the privacy gate has
//! already run. That is what makes the gate auditable: there is one seam to
//! read, not a codebase to trust.
//!
//! # What this crate deliberately does not contain
//!
//! * **The auto-comment safety proof.** `07` §8's three equivalence checks live
//!   in `stratum-intel::comment_safety` (W20) and are run by W26's edit gate.
//!   They need the runtime's own lexer, and ARCHITECTURE §5 gives this crate two
//!   dependencies — proto and platform — precisely so that the desktop, which
//!   links it, cannot reach the parser (C24). What lives here is the request
//!   that produces a comment **proposal** ([`tasks::CommentProposal`]), which is
//!   inert text until something else verifies it.
//! * **Any tool definition.** v1 ships zero (ADR-012, `07` §0.2). The model
//!   cannot execute a command, read a file it was not given, or mutate session
//!   state. `tests/zero_tools.rs` asserts the request body of all three backends
//!   contains no `tools` key.
//! * **Telemetry.** None, of any kind, on AI content. In this product category
//!   the toggle existing at all is a procurement blocker (D-AI-12).

#![forbid(unsafe_code)]

pub mod context;
pub mod provider;
pub mod service;
pub mod tasks;

pub use context::tiers::PrivacyTier;
pub use context::{gate, ContextItem, ContextSource, PackedPrompt};
pub use provider::{ChatProvider, ProviderError, ProviderId};
pub use service::availability::Availability;
pub use service::surface::Surface;
pub use service::{AiError, AiService};
pub use tasks::{AiTask, Intent, TaskEvent};
