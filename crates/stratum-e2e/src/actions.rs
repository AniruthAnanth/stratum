//! The action vocabulary — what a scenario is allowed to *do*.
//!
//! # The one design decision in this file
//!
//! A step names **both** a command id and the keystroke that is supposed to be
//! bound to it:
//!
//! ```text
//! Action::verb("run.blockAndAdvance", Chord::new("Shift+Enter"))
//! ```
//!
//! Tier 1 dispatches the *command*, through the same registry the palette and
//! the native menus go through (plan W25: "a test presses `run.blockAndAdvance`,
//! not a pixel"). Tier 2 sends the *keystroke* to a real webview through
//! WebDriver. One script, two tiers, no second copy of the scenario — which is
//! the property that makes it worth having two tiers at all, because a script
//! that exists twice diverges and then only one of them is true.
//!
//! Tier 1 does not simply throw the chord away, though. `e2e_dispatch` also
//! reports what the *live keymap trie* resolves that chord to, and the runner
//! fails the step if it is not the command the script named. That is a genuinely
//! different assertion from Tier 2's: Tier 2 proves the listener and the focus
//! rules work, Tier 1 proves `resources/keymaps/*.json` still says what the
//! scenario assumes. Neither subsumes the other, and the plan's honest claim —
//! "Tier 1 cannot catch a broken key binding" — stays true of the *listener*.

use serde::{Deserialize, Serialize};
use stratum_proto::engine::EngineEvent;

/// A keystroke in the repo's own accelerator spelling (`apps/desktop/src/keys/
/// trie.ts::parseKeystroke`): `Mod+Shift+K`, `Shift+Enter`, `Mod+Alt+1`.
///
/// `Mod` is Cmd on macOS and Ctrl elsewhere, and the harness never expands it:
/// the frontend's own parser does, so a scenario cannot drift from the app's
/// idea of what `Mod` means.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Chord(pub String);

impl Chord {
    /// A chord from its accelerator spelling.
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self(text.to_owned())
    }
}

/// What a click lands on. Ids, never pixels — Tier 2 turns these into CSS
/// selectors in exactly one place ([`crate::tier2`]) so a renamed class breaks
/// one file rather than five scenarios.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// A dock pane by `PaneComponentId`.
    Pane(String),
    /// A row of the Review history, zero-based, oldest first.
    HistoryRow(usize),
    /// The card attached to a block index.
    Card(u32),
}

/// One thing a scenario does.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Run a registered command. `chord`, when present, must resolve to
    /// `command` in the live keymap.
    Verb {
        /// Command id, e.g. `run.blockAndAdvance`.
        command: String,
        /// Command arguments, exactly as the keymap's `args` would supply them.
        args: serde_json::Value,
        /// The keystroke Tier 2 actually presses.
        chord: Option<Chord>,
        /// The `KeyContext` the scenario's premise implies — `{"editorFocus":
        /// true}` for a run verb, because "the cursor is in the editor" is part
        /// of what the scenario says. Tier 1 resolves the chord against the live
        /// trie *under this context*, so a `when` clause is honoured rather than
        /// read as "bound to nothing". Tier 2 does not use it: there the focus
        /// is real, which is the point of tier 2.
        #[serde(default)]
        context: serde_json::Value,
    },
    /// Open one of `tests/e2e/fixtures/*.do`.
    OpenDoc {
        /// File name inside `tests/e2e/fixtures/`.
        fixture: String,
        /// The file's text, read by the harness so the host never has to agree
        /// with us about where the repo root is.
        text: String,
        /// The events a real engine emits on open — the block map and the
        /// initial per-block statuses. Delivered only by a host that has no
        /// engine of its own; see [`Action::Run`] for why that is not a fake.
        feed: Vec<EngineEvent>,
    },
    /// Run a block, by whatever means this host actually has.
    ///
    /// # Why one action and not two
    ///
    /// The same scenario has to be true of three different worlds: a webview
    /// with no engine behind it (today), the packaged app with a real engine
    /// (after W09/W17), and a real browser being typed into (Tier 2). Writing
    /// three scripts is how the three drift until only one of them is true, so
    /// this action names all three mechanisms and the DRIVER picks the one it
    /// has:
    ///
    /// * a host with [`crate::Capability::Engine`] dispatches `verb`;
    /// * Tier 2 presses `chord` into a real webview;
    /// * a host with only [`crate::Capability::EventInjection`] delivers
    ///   `feed` — W07's committed `EngineEvent` stream, whose every figure is
    ///   copied from `tests/golden/stata18/core_surface.log`.
    ///
    /// The third is not a second fake engine: it is the *only* engine artifact
    /// that exists in the tree, replayed rather than reinvented. The driver
    /// reports which mechanism it used in [`Dispatched::via`], and the report
    /// prints it, so an injected run can never be mistaken for a real one.
    Run {
        /// For the report, e.g. `"summarize price mpg"`.
        label: String,
        /// The command a real host dispatches.
        verb: String,
        /// Its arguments.
        args: serde_json::Value,
        /// The keystroke Tier 2 presses.
        chord: Option<Chord>,
        /// The `KeyContext` the scenario's premise implies. See
        /// [`Action::Verb::context`].
        #[serde(default)]
        context: serde_json::Value,
        /// The canned events an engineless host replays instead.
        feed: Vec<EngineEvent>,
    },
    /// Round-trip without doing anything, so the next snapshot is taken after
    /// the app has quiesced. Spec §38-A's closing assertion — "no source-code
    /// corruption" — is an observation, not an act.
    Observe {
        /// For the report.
        label: String,
    },
    /// Put the caret at a UTF-16 offset.
    PlaceCaret {
        /// Offset into `doc.toString()`.
        offset: u32,
    },
    /// Replace a byte range of the document. Scenario B changes a transformation.
    Edit {
        /// Half-open range in the current text.
        span: (u32, u32),
        /// Replacement text.
        text: String,
    },
    /// Type into whatever has focus, then press Enter. The Command pane path.
    Submit {
        /// The command line, e.g. `summarize price`.
        text: String,
    },
    /// A click, or a double click, on a named target.
    Click {
        /// What to click.
        target: Target,
        /// 1 or 2. Scenario C turns on the difference (§38-C: "use Review
        /// history" — single click loads the line, double click runs it).
        clicks: u8,
    },
}

impl Action {
    /// A command dispatch with no arguments and no keystroke.
    #[must_use]
    pub fn command(command: &str) -> Self {
        Self::Verb {
            command: command.to_owned(),
            args: serde_json::Value::Null,
            chord: None,
            context: serde_json::Value::Null,
        }
    }

    /// A command dispatch and the keystroke that must be bound to it.
    #[must_use]
    pub fn verb(command: &str, chord: Chord) -> Self {
        Self::Verb {
            command: command.to_owned(),
            args: serde_json::Value::Null,
            chord: Some(chord),
            context: serde_json::Value::Null,
        }
    }

    /// A command dispatch whose chord is resolved under an implied context.
    #[must_use]
    pub fn verb_when(command: &str, chord: Chord, context: serde_json::Value) -> Self {
        Self::Verb {
            command: command.to_owned(),
            args: serde_json::Value::Null,
            chord: Some(chord),
            context,
        }
    }

    /// A command dispatch with the keymap's own `args`.
    #[must_use]
    pub fn verb_with(command: &str, args: serde_json::Value, chord: Chord) -> Self {
        Self::Verb {
            command: command.to_owned(),
            args,
            chord: Some(chord),
            context: serde_json::Value::Null,
        }
    }

    /// A short label for the report.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Verb { command, chord, .. } => match chord {
                Some(Chord(c)) => format!("{command} ({c})"),
                None => command.clone(),
            },
            Self::OpenDoc { fixture, .. } => format!("open {fixture}"),
            Self::Run { label, chord, .. } => match chord {
                Some(Chord(c)) => format!("run {label} ({c})"),
                None => format!("run {label}"),
            },
            Self::Observe { label } => format!("observe {label}"),
            Self::PlaceCaret { offset } => format!("caret to {offset}"),
            Self::Edit { span, .. } => format!("edit {}..{}", span.0, span.1),
            Self::Submit { text } => format!("submit {text:?}"),
            Self::Click { target, clicks } => format!("{clicks}x click {target:?}"),
        }
    }
}

/// What the host did with an [`Action`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Dispatched {
    /// How the host actually carried the action out: `verb`, `chord`,
    /// `injection`, or `observe`. Printed in the report — an injected run and a
    /// real run must never look the same in a summary.
    #[serde(default)]
    pub via: String,
    /// `ran` | `unknown` | `disabled`, straight from `runCommand`'s
    /// `CommandResult`. `unknown` is not an error at the IPC layer — a keymap
    /// may name a verb no pane has registered — so the *runner* is what decides
    /// whether `unknown` is a blocked step or a failure.
    pub result: String,
    /// What the live keymap trie resolves the step's chord to, when it named one.
    #[serde(default)]
    pub chord_resolves_to: Option<String>,
    /// How many engine events the frontend actually consumed, for [`Action::Feed`].
    #[serde(default)]
    pub events_applied: u32,
}
