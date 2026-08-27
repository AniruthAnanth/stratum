//! The one implementation of [`AiService`] that talks to a provider.
//!
//! # Offline mode, layer 1
//!
//! [`LocalAiService::provider_for`] is the layer `provider/egress.rs` names in
//! its header: under [`NetworkMode::Offline`] it returns only backends whose
//! [`ProviderCaps::requires_network`] is `false`. Layer 2 — the pre-flight host
//! guard and the loopback-only DNS resolver — lives on the [`Transport`] inside
//! each backend and runs whatever this function decides. Two layers because
//! this one could have a bug: a committed config with a rewritten `base_url`
//! must not exfiltrate anything even if selection let it through.
//!
//! [`Transport`]: crate::provider::http::Transport
//! [`ProviderCaps::requires_network`]: crate::provider::types::ProviderCaps::requires_network
//!
//! # Why the whole run is assembled before the first byte
//!
//! `run` packs, gates, budgets and builds the wire body *before* it awaits
//! anything. Everything that can refuse a request refuses it synchronously, so
//! the returned stream has exactly two terminal events and a caller never has
//! to distinguish "failed to start" from "failed midway" in two places.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::stream::{BoxStream, StreamExt as _};
use stratum_proto::introspect::SessionIntrospect;
use tokio_util::sync::CancellationToken;

use super::availability::{compute, Availability, AvailabilityInputs};
use super::session::{SessionCounters, SurfaceCancellation};
use super::store::UiStore;
use super::surface::Surface;
use super::{AiError, AiService};
use crate::context::audit::{AuditLog, Outcome, SentRecord, TimeRange};
use crate::context::budget::Budget;
use crate::context::packer::{pack, Packed, PromptPreview};
use crate::context::policy::TierInputs;
use crate::context::tiers::PrivacyTier;
use crate::provider::egress::NetworkMode;
use crate::provider::types::{ChatEvent, ChatRequest, StopReason, TokenUsage};
use crate::provider::{ChatProvider, ProviderError};
use crate::tasks::cost::{self, Budgets, CostSummary, ModelPrice, PriceTable};
use crate::tasks::{parse, prompt, AiTask, Intent, TaskEvent};

/// A clock, injected so the audit log, the cache and the counters share one and
/// tests can pin it. Unix milliseconds (A2).
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The wall clock.
#[must_use]
pub fn system_clock() -> Clock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    })
}

/// Everything the service needs that is not a collaborator.
#[derive(Clone, PartialEq, Debug)]
pub struct ServiceConfig {
    /// The backend the user chose. When offline mode is on and this one needs
    /// the network, [`LocalAiService::provider_for`] falls back to a local one
    /// rather than failing — a configured Ollama is the point of offline mode.
    pub preferred: crate::provider::ProviderId,
    /// The global network mode, after any project override.
    pub network: NetworkMode,
    /// `07` §11.2's three caps.
    pub budgets: Budgets,
    /// The shipped price table (`07` §11.1 — never fetched).
    pub prices: PriceTable,
    /// `Some` when a committed `.stratum/ai-policy.toml` forbids the configured
    /// provider outright. A policy can only ever *lower* what is permitted.
    pub policy_block: Option<String>,
    /// Whether a credential resolved for the preferred provider.
    ///
    /// A `bool`, never the key: this struct is `Debug` and a key must never be
    /// printable. The caller resolves it through [`crate::provider::keys::resolve`]
    /// when it constructs the backend, which is the only place that ever holds
    /// the `SecretString`.
    pub has_credential: bool,
    /// The four tier inputs at a surface with no ceiling of its own, for
    /// [`Availability::Configured`]'s `tier` field.
    pub tier_inputs: TierInputs,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            preferred: crate::provider::ProviderId::Anthropic,
            network: NetworkMode::default(),
            budgets: Budgets::default(),
            prices: PriceTable::shipped(),
            policy_block: None,
            has_credential: false,
            tier_inputs: TierInputs::default(),
        }
    }
}

/// The orchestrator.
///
/// Every collaborator is behind an `Arc` because the stream returned by
/// [`AiService::run`] is `'static`: it outlives the borrow of `&self` and has to
/// own what it needs to record the request when it ends.
pub struct LocalAiService {
    providers: Vec<Arc<dyn ChatProvider>>,
    sources: Arc<dyn SessionIntrospect>,
    config: ServiceConfig,
    cancellation: Arc<SurfaceCancellation>,
    counters: Arc<Mutex<SessionCounters>>,
    store: Option<Arc<UiStore>>,
    audit: Option<Arc<AuditLog>>,
    /// `Some(at, detail)` when the last attempt failed to reach the provider.
    health: Arc<Mutex<Option<(u64, String)>>>,
    seq: Arc<AtomicU64>,
    clock: Clock,
}

impl LocalAiService {
    /// Build a service over a provider registry.
    #[must_use]
    pub fn new(
        providers: Vec<Arc<dyn ChatProvider>>,
        sources: Arc<dyn SessionIntrospect>,
        config: ServiceConfig,
    ) -> Self {
        Self {
            providers,
            sources,
            config,
            cancellation: Arc::new(SurfaceCancellation::default()),
            counters: Arc::new(Mutex::new(SessionCounters::default())),
            store: None,
            audit: None,
            health: Arc::new(Mutex::new(None)),
            seq: Arc::new(AtomicU64::new(0)),
            clock: system_clock(),
        }
    }

    /// Attach the **only** store this crate writes (A4).
    #[must_use]
    pub fn with_store(mut self, store: UiStore) -> Self {
        self.store = Some(Arc::new(store));
        self
    }

    /// Attach the audit log.
    #[must_use]
    pub fn with_audit(mut self, audit: AuditLog) -> Self {
        self.audit = Some(Arc::new(audit));
        self
    }

    /// Pin the clock. Tests do this so an audit record is byte-predictable.
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// The cancellation registry, so the shell can supersede a surface's
    /// in-flight request when the user types again.
    #[must_use]
    pub fn cancellation(&self) -> &Arc<SurfaceCancellation> {
        &self.cancellation
    }

    /// The **only** store this crate writes (A4).
    ///
    /// Exposed rather than hidden so the test that asserts we never open
    /// `engine/session.redb` can read the path we did open. The engine process
    /// holds an exclusive lock on that file; a second opener would either block
    /// the engine or corrupt it.
    #[must_use]
    pub fn store(&self) -> Option<&Arc<UiStore>> {
        self.store.as_ref()
    }

    /// **Offline mode, layer 1.** The backend that may serve `surface`.
    ///
    /// Under [`NetworkMode::Offline`] a backend that needs the network is not
    /// selected at all, whatever its configuration says. The surface parameter
    /// is taken (and not yet branched on) because `07` §5.2 gives each surface
    /// its own model row and a per-surface provider override is the next thing
    /// that lands here; routing it through one function is what keeps the
    /// offline check from having to be repeated at each call site.
    ///
    /// # Errors
    /// [`ProviderError::ProviderNotPermitted`] when nothing in the registry may
    /// serve this surface under the active network mode.
    pub fn provider_for(&self, surface: Surface) -> Result<Arc<dyn ChatProvider>, ProviderError> {
        let _ = surface;
        let offline = self.config.network == NetworkMode::Offline;
        let usable =
            |p: &Arc<dyn ChatProvider>| !offline || !p.caps(&p.default_model()).requires_network;
        self.providers
            .iter()
            .find(|p| p.id() == self.config.preferred && usable(p))
            .or_else(|| self.providers.iter().find(|p| usable(p)))
            .cloned()
            .ok_or(ProviderError::ProviderNotPermitted(self.config.preferred))
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    /// Fold the current state into the six-variant type. No network, no I/O.
    fn compute_availability(&self) -> Availability {
        let provider = self.provider_for(Surface::Chat).ok();
        let Some(provider) = provider else {
            // Nothing may serve any surface. Under offline mode that is a policy
            // answer, not a credential answer: the key may be right there.
            return Availability::DisabledByProjectPolicy {
                reason: format!(
                    "Offline AI is on, and {} needs the network. Point the assistant at a \
                     local model, or turn offline mode off in Settings › AI.",
                    self.config.preferred
                ),
            };
        };
        let model = provider.default_model();
        let caps = provider.caps(&model);
        let (requests, spent) = {
            let c = self.counters.lock().expect("counters poisoned");
            (c.cost.requests, c.cost.est_cost_usd)
        };
        compute(&AvailabilityInputs {
            provider: provider.id(),
            model,
            // A backend that needs no network has no credential to be missing:
            // a daemon on loopback takes none, and passing one would be a way to
            // hand a cloud key to a local process.
            has_credential: !caps.requires_network || self.config.has_credential,
            requires_network: caps.requires_network,
            network: self.config.network,
            tier: crate::context::policy::effective_tier(self.config.tier_inputs),
            policy_block: self.config.policy_block.clone(),
            budget: cost::check(self.config.budgets, 0, requests, spent),
            unreachable: self.health.lock().expect("health poisoned").clone(),
        })
    }

    /// Record the outcome of a health probe. Called off the interaction path.
    pub fn note_health(&self, failure: Option<String>) {
        let now = self.now();
        let mut h = self.health.lock().expect("health poisoned");
        *h = failure.map(|d| (now, crate::provider::redact::scrub(&d)));
    }

    /// Pack, gate and budget-check one task. Shared by `preview` and `run` so
    /// the bytes a preview shows are produced by the same code that sends.
    fn plan(&self, task: &AiTask) -> Result<Plan, AiError> {
        let availability = self.compute_availability();
        if !availability.is_usable() {
            return Err(AiError::Unavailable(availability));
        }
        let provider = self.provider_for(task.surface()).map_err(|e| {
            let detail = e.user_message();
            AiError::Unavailable(match e {
                // A policy answer and a transport answer send the user to two
                // different panes, so they must not collapse into one banner.
                ProviderError::ProviderNotPermitted(_) => {
                    Availability::DisabledByProjectPolicy { reason: detail }
                }
                _ => Availability::ProviderUnreachable {
                    since_unix_ms: self.now(),
                    detail,
                },
            })
        })?;

        let surface = task.surface();
        let mut budget = Budget::for_surface(surface, task.intent.comment_scope());
        if task.fast_profile {
            budget = budget.fast_profile(surface);
        }
        let framing = prompt::framing(surface);
        let packed = pack(&task.request, self.sources.as_ref(), &budget, &framing);

        let (requests, spent) = {
            let c = self.counters.lock().expect("counters poisoned");
            (c.cost.requests, c.cost.est_cost_usd)
        };
        let verdict = cost::check(
            self.config.budgets,
            packed.preview.est_input_tokens,
            requests,
            spent,
        );
        if !verdict.allowed() {
            return Err(AiError::Unavailable(Availability::OverBudget { verdict }));
        }

        Ok(Plan {
            provider,
            budget,
            packed,
        })
    }
}

/// Everything decided before a byte is sent.
struct Plan {
    provider: Arc<dyn ChatProvider>,
    budget: Budget,
    packed: Packed,
}

/// What the stream needs, after `&self` is gone, to finish the request.
struct Finish {
    intent: Intent,
    anchors: Vec<crate::tasks::CommentAnchor>,
    cited_lines: Vec<u32>,
    pseudonyms: std::collections::BTreeMap<String, String>,
    record: SentRecord,
    price: ModelPrice,
    started_ms: u64,
    counters: Arc<Mutex<SessionCounters>>,
    audit: Option<Arc<AuditLog>>,
    clock: Clock,
}

/// The `unfold` state machine behind the returned stream.
enum RunState {
    Streaming {
        inner: BoxStream<'static, Result<ChatEvent, ProviderError>>,
        finish: Box<Finish>,
        text: String,
        usage: TokenUsage,
    },
    /// Terminal events queued by the finaliser, drained one per poll.
    Draining(VecDeque<TaskEvent>),
    Ended,
}

#[async_trait::async_trait]
impl AiService for LocalAiService {
    fn availability(&self) -> Availability {
        self.compute_availability()
    }

    fn preview(&self, task: &AiTask) -> Result<PromptPreview, AiError> {
        // No `await` anywhere on this path, and `pack` takes no client: the
        // preview physically cannot send.
        Ok(self.plan(task)?.packed.preview)
    }

    async fn run(
        &self,
        task: AiTask,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, TaskEvent>, AiError> {
        let Plan {
            provider,
            budget,
            packed,
        } = self.plan(&task)?;
        let surface = task.surface();
        let model = budget
            .model
            .clone()
            .unwrap_or_else(|| provider.default_model());

        let req = ChatRequest {
            model: model.clone(),
            system: packed.prompt.system().to_vec(),
            messages: {
                let mut m = crate::tasks::compact_history(
                    &task.history,
                    crate::tasks::HISTORY_COMPACT_THRESHOLD,
                );
                m.extend_from_slice(packed.prompt.messages());
                m
            },
            max_output_tokens: budget.max_output,
            effort: budget.effort,
            thinking: budget.thinking,
            json_schema: None,
            temperature: None,
            stop: Vec::new(),
            deadline: budget.deadline.unwrap_or(budget.retry_budget),
        };

        // One token for two reasons to stop: the caller's, and a newer request
        // on the same surface superseding this one (07 §2.7).
        let token = self.cancellation.issue(surface);
        let child = token.clone();
        let caller = cancel.clone();
        tokio::spawn(async move {
            caller.cancelled().await;
            child.cancel();
        });

        let started_ms = self.now();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let record = SentRecord {
            id: SentRecord::derive_id(surface, started_ms, seq),
            at_unix_ms: started_ms,
            surface,
            provider: provider.id(),
            endpoint_host: String::new(),
            model: model.clone(),
            effective_tier: packed.preview.effective_tier,
            tier_inputs: packed.preview.tier_inputs,
            bound_by: packed.preview.bound_by,
            prompt_bytes: packed.preview.transcript.clone(),
            response_bytes: None,
            usage: TokenUsage::default(),
            est_cost_usd: 0.0,
            latency_ms: 0,
            outcome: Outcome::Ok,
            pseudonym_map_size: packed.preview.pseudonymised,
        };
        let finish = Box::new(Finish {
            intent: task.intent.clone(),
            anchors: task.anchors.clone(),
            cited_lines: task.cited_lines.clone(),
            pseudonyms: packed.pseudonyms.clone(),
            record,
            price: self.config.prices.price(provider.id(), &model),
            started_ms,
            counters: Arc::clone(&self.counters),
            audit: self.audit.clone(),
            clock: Arc::clone(&self.clock),
        });

        let inner = match provider.stream(req, token).await {
            Ok(s) => s,
            Err(e) => {
                // A failure before the first byte is still a request the user
                // made: record it, then report it through the stream so the
                // caller has one place to handle failure.
                let events = finalise_error(*finish, &e);
                return Ok(futures::stream::iter(events).boxed());
            }
        };

        Ok(futures::stream::unfold(
            RunState::Streaming {
                inner,
                finish,
                text: String::new(),
                usage: TokenUsage::default(),
            },
            step,
        )
        .boxed())
    }

    fn audit(&self, range: TimeRange) -> Vec<SentRecord> {
        self.audit
            .as_ref()
            .map(|a| a.range(range))
            .unwrap_or_default()
    }

    fn cost_summary(&self) -> CostSummary {
        self.counters.lock().expect("counters poisoned").cost
    }
}

/// One poll of the run state machine.
async fn step(state: RunState) -> Option<(TaskEvent, RunState)> {
    match state {
        RunState::Streaming {
            mut inner,
            finish,
            mut text,
            mut usage,
        } => loop {
            match inner.next().await {
                Some(Ok(ChatEvent::TextDelta(d))) => {
                    text.push_str(&d);
                    // Structured intents parse the whole reply; streaming half a
                    // JSON object to a caller that will apply it is how a
                    // truncated edit gets written to somebody's do-file.
                    if finish.intent.is_structured() {
                        continue;
                    }
                    return Some((
                        TaskEvent::Text(d),
                        RunState::Streaming {
                            inner,
                            finish,
                            text,
                            usage,
                        },
                    ));
                }
                Some(Ok(ChatEvent::ThinkingDelta(d))) => {
                    return Some((
                        TaskEvent::Progress(d),
                        RunState::Streaming {
                            inner,
                            finish,
                            text,
                            usage,
                        },
                    ));
                }
                Some(Ok(ChatEvent::Started { .. })) => continue,
                Some(Ok(ChatEvent::Finished { stop, usage: u })) => {
                    usage.merge(u);
                    let events = finalise_ok(*finish, &text, usage, stop);
                    return drain(events);
                }
                Some(Err(e)) => {
                    let events = finalise_error(*finish, &e);
                    return drain(events);
                }
                None => {
                    // The provider closed without a terminal frame. Treat it as
                    // a protocol failure rather than success: a partial answer
                    // reported as `Done` is a partial answer somebody trusts.
                    let events = finalise_error(
                        *finish,
                        &ProviderError::protocol("stream ended without a terminal event"),
                    );
                    return drain(events);
                }
            }
        },
        RunState::Draining(mut q) => {
            let ev = q.pop_front()?;
            Some((
                ev,
                if q.is_empty() {
                    RunState::Ended
                } else {
                    RunState::Draining(q)
                },
            ))
        }
        RunState::Ended => None,
    }
}

fn drain(events: Vec<TaskEvent>) -> Option<(TaskEvent, RunState)> {
    let mut q: VecDeque<TaskEvent> = events.into();
    let first = q.pop_front()?;
    Some((
        first,
        if q.is_empty() {
            RunState::Ended
        } else {
            RunState::Draining(q)
        },
    ))
}

/// Turn a completed reply into the surface's typed events, and record it.
fn finalise_ok(mut f: Finish, text: &str, usage: TokenUsage, stop: StopReason) -> Vec<TaskEvent> {
    if stop == StopReason::Refusal {
        return finalise_error(
            f,
            &ProviderError::refused("the provider declined this request"),
        );
    }
    if stop == StopReason::Cancelled {
        return finalise_cancelled(f);
    }

    // `07` §4.4: the pseudonym map is inverted on the way back, so the user
    // reads their own variable names in an answer the provider never saw them in.
    let reply = if f.pseudonyms.is_empty() {
        text.to_owned()
    } else {
        crate::context::redact::Pseudonymiser::from_mapping(&f.pseudonyms).unmap(text)
    };

    let mut events = Vec::new();
    let parsed = match &f.intent {
        Intent::Comment { .. } => parse::comments(&reply, &f.anchors)
            .map(|(kept, dropped)| TaskEvent::CommentProposal(kept, dropped)),
        Intent::Repro { draft_fixes: true } => {
            parse::patch(&reply, &f.cited_lines).map(TaskEvent::Diff)
        }
        Intent::HistoryCleanup => parse::history(&reply).map(|h| {
            TaskEvent::Structured(serde_json::to_value(h).unwrap_or(serde_json::Value::Null))
        }),
        Intent::Complete => Ok(TaskEvent::Text(parse::ghost(&reply))),
        // Prose intents already streamed their text.
        _ => Ok(TaskEvent::Text(String::new())),
    };

    match parsed {
        Ok(TaskEvent::Text(t)) if t.is_empty() => {}
        Ok(ev) => events.push(ev),
        Err(e) => return finalise_contract(f, &reply, usage, &e.to_string()),
    }

    let cost_usd = f.price.cost_usd(usage);
    f.record.response_bytes = Some(reply);
    f.record.usage = usage;
    f.record.est_cost_usd = cost_usd;
    f.record.latency_ms = (f.clock)().saturating_sub(f.started_ms);
    f.record.outcome = Outcome::Ok;
    commit(&f, usage);
    events.push(TaskEvent::Done { usage, cost_usd });
    events
}

fn finalise_contract(
    mut f: Finish,
    reply: &str,
    usage: TokenUsage,
    detail: &str,
) -> Vec<TaskEvent> {
    f.record.response_bytes = Some(reply.to_owned());
    f.record.usage = usage;
    f.record.est_cost_usd = f.price.cost_usd(usage);
    f.record.latency_ms = (f.clock)().saturating_sub(f.started_ms);
    f.record.outcome = Outcome::Error {
        detail: detail.to_owned(),
    };
    commit(&f, usage);
    vec![TaskEvent::Failed(AiError::Contract(detail.to_owned()))]
}

fn finalise_cancelled(mut f: Finish) -> Vec<TaskEvent> {
    f.record.latency_ms = (f.clock)().saturating_sub(f.started_ms);
    f.record.outcome = Outcome::Cancelled;
    write_audit(&f);
    vec![TaskEvent::Failed(AiError::Cancelled)]
}

fn finalise_error(mut f: Finish, e: &ProviderError) -> Vec<TaskEvent> {
    if matches!(e, ProviderError::Cancelled) {
        return finalise_cancelled(f);
    }
    f.record.latency_ms = (f.clock)().saturating_sub(f.started_ms);
    f.record.outcome = Outcome::Error {
        detail: e.user_message(),
    };
    write_audit(&f);
    vec![TaskEvent::Failed(AiError::Provider(e.clone()))]
}

/// Count it, then record it.
fn commit(f: &Finish, usage: TokenUsage) {
    if let Ok(mut c) = f.counters.lock() {
        c.record(f.record.effective_tier, usage, f.price);
    }
    write_audit(f);
}

/// A failed audit write never fails the request: an answer the user can read
/// but that we could not record is better than no answer. It is logged, not
/// swallowed.
fn write_audit(f: &Finish) {
    if let Some(log) = &f.audit {
        if let Err(e) = log.append(&f.record) {
            tracing::warn!(error = %e, "AI audit record not written");
        }
    }
}

/// The tier a caller with no policy and no dataset marking gets. Exposed so the
/// desktop's unconfigured panel can name it without duplicating the fold.
#[must_use]
pub fn default_tier() -> PrivacyTier {
    crate::context::policy::effective_tier(TierInputs::default())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use stratum_proto::introspect::AiContextSnapshot;

    use super::*;
    use crate::context::adapter::SnapshotIntrospect;
    use crate::provider::backends::{anthropic, ollama};
    use crate::provider::http::Transport;
    use crate::provider::{ProviderConfig, ProviderId};
    use crate::service::availability::AvailabilityKind;

    fn sources() -> Arc<dyn SessionIntrospect> {
        Arc::new(SnapshotIntrospect::new(AiContextSnapshot::default()))
    }

    fn cloud() -> Arc<dyn ChatProvider> {
        Arc::new(anthropic::AnthropicProvider::new(
            ProviderConfig::anthropic_default(),
            Transport::new(
                crate::provider::egress::EgressPolicy::new(NetworkMode::Enabled),
                std::time::Duration::from_secs(5),
            )
            .expect("client"),
            None,
        ))
    }

    fn local() -> Arc<dyn ChatProvider> {
        Arc::new(ollama::OllamaProvider::new(
            ProviderConfig::ollama_default(),
            Transport::new(
                crate::provider::egress::EgressPolicy::new(NetworkMode::Offline),
                std::time::Duration::from_secs(5),
            )
            .expect("client"),
        ))
    }

    #[test]
    fn offline_mode_layer_1_will_not_even_select_a_backend_that_needs_the_network() {
        let svc = LocalAiService::new(
            vec![cloud(), local()],
            sources(),
            ServiceConfig {
                preferred: ProviderId::Anthropic,
                network: NetworkMode::Offline,
                ..ServiceConfig::default()
            },
        );
        // The preferred provider is the cloud one and it is *not* what comes
        // back: selection, not the transport, is what refused it.
        assert_eq!(
            svc.provider_for(Surface::Chat)
                .expect("a local backend")
                .id(),
            ProviderId::Ollama
        );
    }

    #[test]
    fn with_offline_on_and_no_local_backend_the_state_is_a_policy_answer_not_a_key_answer() {
        let svc = LocalAiService::new(
            vec![cloud()],
            sources(),
            ServiceConfig {
                network: NetworkMode::Offline,
                ..ServiceConfig::default()
            },
        );
        assert!(svc.provider_for(Surface::Chat).is_err());
        assert_eq!(
            svc.availability().kind(),
            AvailabilityKind::DisabledByProjectPolicy,
            "a missing key is not the reason, and saying so sends the user to the wrong pane"
        );
    }

    #[test]
    fn with_no_credential_the_state_is_no_credential_and_nothing_can_run() {
        let svc = LocalAiService::new(vec![cloud()], sources(), ServiceConfig::default());
        assert_eq!(svc.availability().kind(), AvailabilityKind::NoCredential);
        let task = crate::tasks::AiTask::new(
            Intent::FreeForm,
            crate::context::packer::PackRequest::default(),
        );
        // And the failure carries the state, not a string, so the caller renders
        // the same panel it would have rendered without asking.
        match svc.preview(&task) {
            Err(AiError::Unavailable(Availability::NoCredential)) => {}
            other => panic!("expected Unavailable(NoCredential), got {other:?}"),
        }
    }

    #[test]
    fn a_project_policy_block_beats_every_other_reason() {
        let svc = LocalAiService::new(
            vec![local()],
            sources(),
            ServiceConfig {
                preferred: ProviderId::Ollama,
                policy_block: Some("This project forbids AI on restricted data.".to_owned()),
                ..ServiceConfig::default()
            },
        );
        assert_eq!(
            svc.availability().kind(),
            AvailabilityKind::DisabledByProjectPolicy
        );
    }

    #[test]
    fn a_local_daemon_has_no_credential_to_be_missing() {
        let svc = LocalAiService::new(
            vec![local()],
            sources(),
            ServiceConfig {
                preferred: ProviderId::Ollama,
                ..ServiceConfig::default()
            },
        );
        assert_eq!(svc.availability().kind(), AvailabilityKind::Configured);
    }

    #[test]
    fn the_session_request_cap_is_reported_as_over_budget_not_as_a_failure() {
        let svc = LocalAiService::new(
            vec![local()],
            sources(),
            ServiceConfig {
                preferred: ProviderId::Ollama,
                budgets: Budgets {
                    per_session_request_cap: 0,
                    ..Budgets::default()
                },
                ..ServiceConfig::default()
            },
        );
        assert_eq!(svc.availability().kind(), AvailabilityKind::OverBudget);
    }

    #[test]
    fn preview_is_the_same_pack_run_would_have_sent() {
        let svc = LocalAiService::new(
            vec![local()],
            sources(),
            ServiceConfig {
                preferred: ProviderId::Ollama,
                ..ServiceConfig::default()
            },
        );
        let task = crate::tasks::AiTask::new(
            Intent::FreeForm,
            crate::context::packer::PackRequest {
                user_text: "why is price skewed?".to_owned(),
                ..crate::context::packer::PackRequest::default()
            },
        );
        let a = svc.preview(&task).expect("configured");
        let b = svc.preview(&task).expect("configured");
        assert_eq!(a.transcript, b.transcript, "packing is deterministic");
        assert!(a.transcript.contains("why is price skewed?"));
    }
}
