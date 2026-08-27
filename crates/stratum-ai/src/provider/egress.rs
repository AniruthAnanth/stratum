//! 07 §4.7 / ADR-012 — offline mode, enforced twice.
//!
//! Layer 1 is [`crate::service::runtime`]: `provider_for(surface)` only returns
//! providers whose [`ProviderCaps::requires_network`] is `false`. Layer 2 is
//! this module, and it exists precisely because layer 1 could have a bug: a
//! mistyped or maliciously rewritten `base_url` in a committed config file must
//! not be able to exfiltrate anything even if provider selection let it through.
//!
//! Two independent mechanisms here, not one:
//!
//! * a **pre-flight** ([`EgressPolicy::guard`]) that refuses any URL whose host
//!   is not a loopback address, before a socket is created; and
//! * a **loopback-only DNS resolver** ([`loopback_only_resolver`]) installed on
//!   the `reqwest::Client` itself, which returns no addresses for every name
//!   except the `localhost` family.
//!
//! [`ProviderCaps::requires_network`]: super::types::ProviderCaps::requires_network

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

use super::error::ProviderError;

/// Whether network AI is permitted at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    /// The default: any configured provider may be reached.
    #[default]
    Enabled,
    /// `ai.network = "off"`, or a project policy with `require_offline = true`.
    /// Only a provider on this machine's loopback interface may be reached.
    Offline,
}

/// Counters, not durations (ADR-017). Every decision this module makes
/// increments exactly one of these, so a test can assert that the guard is what
/// stopped a request rather than inferring it from an error string.
#[derive(Debug, Default)]
pub struct EgressLedger {
    permitted: AtomicU64,
    blocked: AtomicU64,
}

impl EgressLedger {
    /// Requests the guard let through.
    #[must_use]
    pub fn permitted(&self) -> u64 {
        self.permitted.load(Ordering::Relaxed)
    }

    /// Requests the guard refused.
    #[must_use]
    pub fn blocked(&self) -> u64 {
        self.blocked.load(Ordering::Relaxed)
    }
}

/// The egress decision, plus the ledger that records it.
#[derive(Clone, Debug)]
pub struct EgressPolicy {
    mode: NetworkMode,
    ledger: Arc<EgressLedger>,
}

impl EgressPolicy {
    /// Build a policy for `mode`.
    #[must_use]
    pub fn new(mode: NetworkMode) -> Self {
        Self {
            mode,
            ledger: Arc::new(EgressLedger::default()),
        }
    }

    /// The configured mode.
    #[must_use]
    pub const fn mode(&self) -> NetworkMode {
        self.mode
    }

    /// The counters.
    #[must_use]
    pub fn ledger(&self) -> &Arc<EgressLedger> {
        &self.ledger
    }

    /// Pre-flight. Called by the transport before a socket exists.
    ///
    /// # Errors
    /// [`ProviderError::EgressBlocked`] when offline mode is on and the URL's
    /// host is not loopback; [`ProviderError::network`] when the URL has no host
    /// at all, which no legitimate provider base URL does.
    pub fn guard(&self, url: &reqwest::Url) -> Result<(), ProviderError> {
        let Some(host) = url.host_str() else {
            self.ledger.blocked.fetch_add(1, Ordering::Relaxed);
            return Err(ProviderError::network("provider URL has no host"));
        };
        match self.mode {
            NetworkMode::Enabled => {
                self.ledger.permitted.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            NetworkMode::Offline if is_loopback_host(host) => {
                self.ledger.permitted.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            NetworkMode::Offline => {
                self.ledger.blocked.fetch_add(1, Ordering::Relaxed);
                Err(ProviderError::EgressBlocked(host.to_owned()))
            }
        }
    }
}

/// Whether a URL host component denotes this machine.
///
/// Deliberately does **not** consult DNS: a name that resolves to `127.0.0.1`
/// today can resolve elsewhere tomorrow, and "the answer depends on the
/// network" is not a property a privacy guarantee can be built on. Only the
/// literal `localhost` label and loopback IP literals pass.
#[must_use]
pub fn is_loopback_host(host: &str) -> bool {
    // `Url::host_str` strips the brackets from `[::1]`, but a hand-written
    // config string may still carry them.
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if bare.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match bare.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        // A subdomain of localhost (`foo.localhost`) is loopback by RFC 6761 on
        // every OS we ship on, but we do not accept it: the resolver below will
        // not answer for it either, and two rules that disagree is how a guard
        // develops a hole.
        Err(_) => false,
    }
}

/// A `reqwest` DNS resolver that answers only for the `localhost` family.
///
/// Every other name gets an empty address iterator, which `hyper` surfaces as a
/// connect error. This is layer 2's second half: even if [`EgressPolicy::guard`]
/// had a bug, a hostile `base_url` would still not resolve.
#[derive(Debug, Default)]
pub struct LoopbackOnlyResolver;

impl Resolve for LoopbackOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let answer: Addrs = if name.as_str().eq_ignore_ascii_case("localhost") {
            Box::new(
                [
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
                    SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0),
                ]
                .into_iter(),
            )
        } else {
            Box::new(std::iter::empty())
        };
        Box::pin(async move { Ok(answer) })
    }
}

/// The resolver an offline client installs.
#[must_use]
pub fn loopback_only_resolver() -> Arc<dyn Resolve> {
    Arc::new(LoopbackOnlyResolver)
}

/// The OS resolver, used when network AI is permitted.
#[derive(Debug, Default)]
pub struct SystemResolver;

impl Resolve for SystemResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            let collected: Vec<SocketAddr> = addrs.collect();
            Ok(Box::new(collected.into_iter()) as Addrs)
        })
    }
}

/// Build the HTTP client a transport will use.
///
/// `resolver` is a parameter rather than a constant for one reason, and it is
/// the reason the offline test is worth anything: layer 2's job is to refuse a
/// request that DNS would *otherwise happily resolve*. A test that can only
/// supply a resolver returning NXDOMAIN proves the resolver, not the guard. The
/// product always passes [`loopback_only_resolver`] or [`SystemResolver`]; the
/// offline test passes a resolver that points every name at a listening socket
/// it owns, and then asserts that socket never accepted a connection.
///
/// # Errors
/// Propagates `reqwest`'s TLS/backend construction failures.
pub fn build_client(
    timeout: Duration,
    resolver: Arc<dyn Resolve>,
) -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        // 07 §2.7: cancellation drops the body stream, which closes the
        // connection. A pool that keeps it alive would defeat that.
        .timeout(timeout)
        .dns_resolver(resolver)
        .build()
        .map_err(|e| ProviderError::network(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_literals_and_localhost_pass_and_nothing_else_does() {
        for host in [
            "localhost",
            "LOCALHOST",
            "127.0.0.1",
            "127.5.5.5",
            "::1",
            "[::1]",
        ] {
            assert!(is_loopback_host(host), "{host} should be loopback");
        }
        for host in [
            "api.anthropic.com",
            "198.51.100.7",
            "0.0.0.0",
            "10.0.0.1",
            // RFC 6761 says this IS loopback; we refuse it anyway, because the
            // resolver below refuses it and two rules that disagree is a hole.
            "gateway.localhost",
            "localhost.evil.example",
        ] {
            assert!(!is_loopback_host(host), "{host} should NOT be loopback");
        }
    }

    #[test]
    fn offline_blocks_a_public_host_and_counts_it() {
        let policy = EgressPolicy::new(NetworkMode::Offline);
        let url = reqwest::Url::parse("https://api.anthropic.com/v1/messages").unwrap();
        assert_eq!(
            policy.guard(&url),
            Err(ProviderError::EgressBlocked("api.anthropic.com".to_owned()))
        );
        assert_eq!(policy.ledger().blocked(), 1);
        assert_eq!(policy.ledger().permitted(), 0);
    }

    #[test]
    fn offline_permits_a_local_ollama() {
        let policy = EgressPolicy::new(NetworkMode::Offline);
        let url = reqwest::Url::parse("http://127.0.0.1:11434/api/chat").unwrap();
        assert!(policy.guard(&url).is_ok());
        assert_eq!(policy.ledger().permitted(), 1);
        assert_eq!(policy.ledger().blocked(), 0);
    }

    #[tokio::test]
    async fn the_loopback_resolver_answers_for_localhost_and_for_nothing_else() {
        let r = LoopbackOnlyResolver;
        let local: Vec<_> = r
            .resolve("localhost".parse().unwrap())
            .await
            .unwrap()
            .collect();
        assert!(!local.is_empty());
        assert!(local.iter().all(|a| a.ip().is_loopback()));

        let public: Vec<_> = r
            .resolve("api.anthropic.com".parse().unwrap())
            .await
            .unwrap()
            .collect();
        assert!(
            public.is_empty(),
            "a public name must resolve to nothing in offline mode"
        );
    }
}
