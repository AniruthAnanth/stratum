//! **Tier 2 — real WebDriver input.** Windows and Linux only (Q16, ADR-011).
//!
//! `tauri-driver` fronts Edge WebDriver on Windows and `WebKitWebDriver` on
//! Linux; `fantoccini` is the client, so the scripts stay Rust and are literally
//! the same [`crate::Scenario`] values Tier 1 runs. Real keystrokes, real
//! clicks, real focus — which is the coverage Tier 1 cannot have, because Tier 1
//! dispatches commands and therefore cannot see a broken key binding, a
//! `pointer-events: none` regression or a focus trap.
//!
//! # macOS is Tier-1 only, and this file says so out loud
//!
//! WKWebView exposes no WebDriver endpoint. There is no flag, no entitlement and
//! no `webkit2gtk` fallback that changes that; `tauri-driver` cannot attach to a
//! macOS Tauri app at all. That is recorded as **Q16** in ARCHITECTURE and as
//! **ADR-011** in DECISIONS, and it is the reason [`supported_here`] refuses on
//! macOS with a reason instead of failing later with a connection error that
//! reads like a broken environment. Spec §31's "UI automation eventually on all
//! major platforms" is, today, two of three — and the docs say two of three
//! rather than claiming automation we do not have.
//!
//! # Why this compiles on macOS anyway
//!
//! macOS is the primary development platform. A tier that only typechecks on
//! CI's Linux runner is a tier that is broken for a week at a time before anyone
//! notices. So the whole module — including the fantoccini client — builds
//! everywhere under `--features tier2` (which is what ci.yml's `cargo clippy
//! --all-features` compiles), and only *connecting* is refused.

use crate::actions::Target;

/// Where the WebDriver endpoint lives. `tauri-driver`'s default.
pub const DEFAULT_WEBDRIVER_URL: &str = "http://127.0.0.1:4444";

/// Whether Tier 2 can run on this platform, and why not when it cannot.
///
/// # Errors
/// On macOS, with the Q16/ADR-011 explanation.
pub fn supported_here() -> Result<(), String> {
    if cfg!(target_os = "macos") {
        return Err(
            "tier 2 does not run on macOS: WKWebView exposes no WebDriver endpoint, so \
             tauri-driver cannot attach (Q16, ADR-011). macOS is covered by tier 1 only, \
             and we say so rather than claiming UI automation we do not have."
                .to_owned(),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Keystrokes
// ---------------------------------------------------------------------------

/// WebDriver's control characters (W3C WebDriver §17.4.2, the U+E0xx block).
const NUL: char = '\u{E000}';
const SHIFT: char = '\u{E008}';
const CONTROL: char = '\u{E009}';
const ALT: char = '\u{E00A}';
const META: char = '\u{E03D}';

/// Translate one named key into the WebDriver code point for it.
fn named_key(name: &str) -> Option<char> {
    Some(match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => '\u{E007}',
        "escape" | "esc" => '\u{E00C}',
        "tab" => '\u{E004}',
        "space" => '\u{E00D}',
        "backspace" => '\u{E003}',
        "delete" => '\u{E017}',
        "up" | "arrowup" => '\u{E013}',
        "down" | "arrowdown" => '\u{E015}',
        "left" | "arrowleft" => '\u{E012}',
        "right" | "arrowright" => '\u{E014}',
        "pageup" => '\u{E00E}',
        "pagedown" => '\u{E00F}',
        "home" => '\u{E011}',
        "end" => '\u{E010}',
        "f1" => '\u{E031}',
        "f2" => '\u{E032}',
        "f3" => '\u{E033}',
        "f4" => '\u{E034}',
        "f5" => '\u{E035}',
        "f6" => '\u{E036}',
        "f7" => '\u{E037}',
        "f8" => '\u{E038}',
        _ => return None,
    })
}

/// Turn an accelerator (`Shift+Enter`, `Mod+Alt+2`) into a WebDriver key string.
///
/// `Mod` becomes Control, never Meta: Tier 2 runs on Windows and Linux only, and
/// `parseKeystroke` in `apps/desktop/src/keys/trie.ts` maps `Mod` to Ctrl
/// everywhere but macOS. Encoding that here rather than asking the app keeps the
/// *input* side independent of the app's own parser, which is the whole point of
/// driving real keys.
///
/// The trailing NUL releases every modifier, so one step cannot leave Shift held
/// down for the next.
///
/// # Errors
/// On an unknown modifier or an unknown key name.
pub fn chord_to_keys(chord: &str) -> Result<String, String> {
    let mut parts: Vec<&str> = chord.split('+').map(str::trim).collect();
    let key = parts
        .pop()
        .filter(|k| !k.is_empty())
        .ok_or_else(|| format!("empty keystroke in {chord:?}"))?;

    let mut out = String::new();
    for m in &parts {
        let c = match m.to_ascii_lowercase().as_str() {
            "mod" | "ctrl" | "control" => CONTROL,
            "cmd" | "meta" | "super" | "win" => META,
            "alt" | "option" => ALT,
            "shift" => SHIFT,
            other => return Err(format!("unknown modifier {other:?} in {chord:?}")),
        };
        out.push(c);
    }

    if let Some(c) = named_key(key) {
        out.push(c);
    } else if key.chars().count() == 1 {
        out.push_str(&key.to_lowercase());
    } else {
        return Err(format!("unknown key {key:?} in {chord:?}"));
    }

    out.push(NUL);
    Ok(out)
}

/// The CSS selector for a click target.
///
/// **This is the tier-2 contract with the pane units.** Tier 2 cannot click an
/// id that the DOM does not carry, so W12/W14/W16 must keep these `data-*`
/// attributes on the elements named here. They are collected in one function on
/// purpose: when a renderer changes, exactly one file needs editing, and a
/// missing attribute fails as "no element matched `[data-pane-id="history"]`"
/// rather than as a mysterious scenario failure three steps later.
#[must_use]
pub fn selector(target: &Target) -> String {
    match target {
        Target::Pane(id) => format!("[data-pane-id=\"{id}\"]"),
        Target::HistoryRow(n) => {
            format!("[data-pane-id=\"history\"] [data-history-row=\"{n}\"]")
        }
        Target::Card(block) => format!("[data-block-index=\"{block}\"] [data-card]"),
    }
}

/// The JavaScript entry point the e2e bridge installs on `window`.
///
/// Tier 2 reaches the same `dispatch`/`snapshot` the Tier-1 socket reaches, so
/// the two tiers cannot drift into asking the app different questions. What
/// differs between the tiers is how the *input* arrives, which is the only thing
/// that should differ.
pub const BRIDGE_GLOBAL: &str = "__STRATUM_E2E__";

#[cfg(feature = "tier2")]
mod client {
    //! The live client. Behind `--features tier2` because `fantoccini` pulls an
    //! async HTTP stack that a default `cargo build --workspace` has no business
    //! compiling (this crate is meant to be out of `default-members` — see the
    //! manifest header).

    use std::collections::HashMap;

    use fantoccini::{Client, ClientBuilder, Locator};
    use hyper_util::client::legacy::connect::HttpConnector;
    use serde_json::{json, Value};

    use super::{chord_to_keys, selector, supported_here, BRIDGE_GLOBAL, DEFAULT_WEBDRIVER_URL};
    use crate::actions::{Action, Chord, Dispatched};
    use crate::snapshot::{Snapshot, What};
    use crate::{Capabilities, Counters, Driver, DriverError, Tier};

    /// How to reach `tauri-driver`.
    #[derive(Clone, PartialEq, Eq, Debug)]
    pub struct WebDriverSpec {
        /// WebDriver endpoint. `tauri-driver` listens on 4444 by default.
        pub url: String,
        /// The packaged application `tauri-driver` should launch.
        pub application: String,
    }

    /// A Tier-2 connection to a real webview.
    pub struct Tier2Driver {
        rt: tokio::runtime::Runtime,
        client: Client,
        caps: Capabilities,
        host: String,
        counters: Counters,
    }

    impl Tier2Driver {
        /// Start `tauri-driver`'s session and complete the bridge handshake.
        ///
        /// # Errors
        /// On macOS (Q16), and on any WebDriver or handshake failure.
        pub fn connect(spec: &WebDriverSpec) -> Result<Self, DriverError> {
            supported_here().map_err(DriverError::Unsupported)?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| DriverError::Transport(format!("tokio: {e}")))?;

            // `tauri:options.application` is how tauri-driver is told what to
            // launch; everything else is ordinary W3C capabilities.
            let mut caps = serde_json::Map::new();
            caps.insert(
                "tauri:options".to_owned(),
                json!({ "application": spec.application }),
            );

            let url = if spec.url.is_empty() {
                DEFAULT_WEBDRIVER_URL
            } else {
                &spec.url
            };
            // Plain HTTP over loopback — see the manifest for why there is no
            // TLS stack in this dependency at all.
            let client = rt
                .block_on(
                    ClientBuilder::new(HttpConnector::new())
                        .capabilities(caps)
                        .connect(url),
                )
                .map_err(|e| DriverError::Transport(format!("tauri-driver at {url}: {e}")))?;

            let mut driver = Self {
                rt,
                client,
                caps: Capabilities::default(),
                host: format!("tauri-driver ({})", spec.application),
                counters: Counters::default(),
            };
            driver.caps = driver.hello()?;
            Ok(driver)
        }

        fn hello(&mut self) -> Result<Capabilities, DriverError> {
            let script = format!("return window.{BRIDGE_GLOBAL}.capabilities();");
            let v = self.eval(&script, Vec::new())?;
            serde_json::from_value(v).map_err(|e| DriverError::Host(format!("capabilities(): {e}")))
        }

        fn eval(&mut self, script: &str, args: Vec<Value>) -> Result<Value, DriverError> {
            self.counters.round_trips += 1;
            let fut = self.client.execute(script, args);
            self.rt
                .block_on(fut)
                .map_err(|e| DriverError::Transport(format!("execute: {e}")))
        }

        fn press(&mut self, chord: &Chord) -> Result<(), DriverError> {
            let keys = chord_to_keys(&chord.0).map_err(DriverError::Unsupported)?;
            self.counters.round_trips += 1;
            // Sent to the focused element via the active element, which is what
            // makes this a REAL keystroke rather than a synthesised event: the
            // app's own listener, its own focus rules and its own `when`
            // clauses all get a vote.
            let fut = async {
                let el = self.client.active_element().await?;
                el.send_keys(&keys).await
            };
            self.rt
                .block_on(fut)
                .map_err(|e| DriverError::Transport(format!("send_keys: {e}")))
        }

        fn click(&mut self, css: &str, clicks: u8) -> Result<(), DriverError> {
            self.counters.round_trips += 1;
            let fut = async {
                let el = self.client.find(Locator::Css(css)).await?;
                if clicks >= 2 {
                    // No double-click primitive in fantoccini; two clicks inside
                    // the platform's double-click interval is what the DOM sees
                    // as a dblclick, and W16's row handler is written against
                    // the DOM event, not against a WebDriver command.
                    el.clone().click().await?;
                    el.click().await
                } else {
                    el.click().await
                }
            };
            self.rt
                .block_on(fut)
                .map(|_| ())
                .map_err(|e| DriverError::Transport(format!("click {css}: {e}")))
        }

        fn bridge_call(&mut self, method: &str, arg: Value) -> Result<Value, DriverError> {
            let script = format!("return window.{BRIDGE_GLOBAL}.{method}(arguments[0]);");
            self.eval(&script, vec![arg])
        }
    }

    impl Driver for Tier2Driver {
        fn tier(&self) -> Tier {
            Tier::Two
        }

        fn host(&self) -> String {
            self.host.clone()
        }

        fn capabilities(&self) -> Capabilities {
            self.caps.clone()
        }

        fn dispatch(&mut self, action: &Action) -> Result<Dispatched, DriverError> {
            self.counters.dispatches += 1;
            let via = match action {
                // The whole reason tier 2 exists: press the key, do not call the
                // command. Everything the key has to survive on its way to the
                // handler — the listener, the trie, the `when` clause, focus —
                // is under test here and nowhere else.
                Action::Verb {
                    chord: Some(chord), ..
                }
                | Action::Run {
                    chord: Some(chord), ..
                } => {
                    self.press(chord)?;
                    "chord"
                }
                Action::Click { target, clicks } => {
                    self.click(&selector(target), *clicks)?;
                    "click"
                }
                Action::Submit { text } => {
                    let keys = format!("{text}\u{E007}");
                    self.counters.round_trips += 1;
                    let fut = async {
                        let el = self.client.active_element().await?;
                        el.send_keys(&keys).await
                    };
                    self.rt
                        .block_on(fut)
                        .map_err(|e| DriverError::Transport(format!("typing: {e}")))?;
                    "typing"
                }
                // No keystroke for it: go through the bridge, which is the same
                // entry point tier 1 uses.
                other => {
                    let arg = serde_json::to_value(other)
                        .map_err(|e| DriverError::Transport(e.to_string()))?;
                    self.bridge_call("dispatch", arg)?;
                    "bridge"
                }
            };

            // A real keystroke tells us nothing about what it reached, so ask.
            let settled = self.bridge_call("settle", Value::Null)?;
            let mut dispatched: Dispatched = serde_json::from_value(settled)
                .map_err(|e| DriverError::Host(format!("settle(): {e}")))?;
            dispatched.via = via.to_owned();
            Ok(dispatched)
        }

        fn snapshot(&mut self, what: &What) -> Result<Snapshot, DriverError> {
            self.counters.snapshots += 1;
            let arg =
                serde_json::to_value(what).map_err(|e| DriverError::Transport(e.to_string()))?;
            let v = self.bridge_call("snapshot", arg)?;
            serde_json::from_value(v).map_err(|e| DriverError::Host(format!("snapshot(): {e}")))
        }

        fn counters(&self) -> Counters {
            self.counters
        }
    }

    impl Drop for Tier2Driver {
        fn drop(&mut self) {
            // A leaked session keeps tauri-driver holding a window open, and the
            // next scenario attaches to the previous scenario's app.
            let client = self.client.clone();
            let _ = self.rt.block_on(client.close());
        }
    }

    /// Unused-import guard: `HashMap` is not needed, and clippy would say so.
    #[allow(dead_code)]
    type Unused = HashMap<String, String>;
}

#[cfg(feature = "tier2")]
pub use client::{Tier2Driver, WebDriverSpec};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_is_tier_1_only_and_the_refusal_names_the_decision() {
        let got = supported_here();
        if cfg!(target_os = "macos") {
            let why = got.expect_err("macOS has no WebDriver endpoint for WKWebView");
            assert!(why.contains("Q16"), "{why}");
            assert!(why.contains("ADR-011"), "{why}");
            assert!(why.contains("tier 1"), "{why}");
        } else {
            assert!(got.is_ok(), "windows and linux run tier 2");
        }
    }

    #[test]
    fn a_chord_becomes_real_modifier_key_codes_and_releases_them() {
        let keys = chord_to_keys("Shift+Enter").expect("a known chord");
        assert_eq!(keys, "\u{E008}\u{E007}\u{E000}");

        // `Mod` is Ctrl on the two platforms tier 2 runs on.
        let keys = chord_to_keys("Mod+Alt+2").expect("a known chord");
        assert_eq!(keys, "\u{E009}\u{E00A}2\u{E000}");

        let keys = chord_to_keys("Mod+Shift+K").expect("a known chord");
        assert_eq!(keys, "\u{E009}\u{E008}k\u{E000}");
    }

    #[test]
    fn an_unknown_key_is_an_error_rather_than_a_keystroke_nobody_pressed() {
        assert!(chord_to_keys("Mod+Frobnicate").is_err());
        assert!(chord_to_keys("Hyper+K").is_err());
        assert!(chord_to_keys("").is_err());
    }

    #[test]
    fn every_chord_the_scenarios_use_translates() {
        // A scenario naming a chord tier 2 cannot type would fail on Windows and
        // Linux only, which is the worst place to find out.
        for scenario in crate::fixtures::all().expect("the scenarios") {
            for step in &scenario.steps {
                let chord = match &step.action {
                    crate::Action::Verb { chord, .. } | crate::Action::Run { chord, .. } => {
                        chord.clone()
                    }
                    _ => None,
                };
                if let Some(c) = chord {
                    chord_to_keys(&c.0).unwrap_or_else(|e| panic!("scenario {}: {e}", scenario.id));
                }
            }
        }
    }

    #[test]
    fn click_targets_have_stable_selectors() {
        assert_eq!(
            selector(&Target::HistoryRow(3)),
            "[data-pane-id=\"history\"] [data-history-row=\"3\"]"
        );
        assert_eq!(
            selector(&Target::Pane("results".to_owned())),
            "[data-pane-id=\"results\"]"
        );
    }
}
