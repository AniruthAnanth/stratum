//! CONTRACTS.md §9 — the completion environment.
//!
//! Pushed to every webview on `StateChanged` so `stratum-wasm` can complete
//! synchronously with NO IPC on the keystroke path.
//!
//! **AMENDED (A11). This type is BOUNDED and the bound is a test.** The pre-audit
//! version pushed `varnames` + `var_labels` for every variable on every
//! `StateChanged`. A 32 767-variable dataset is ~1.5 MB per push, down the same
//! broadcast channel C23 protects with a 16 ms / 64 KB budget by refusing to
//! inline a 1.5 MB SVG — so the completion snapshot would have been doing exactly
//! what the graph payload is forbidden from doing, on every command.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// First N variables in storage order. Beyond this the popup offers "more…",
/// which issues `CompletionEnvPage` — an explicit interaction, never a keystroke.
pub const COMPLETION_ENV_MAX_VARS: usize = 2048;

/// Cap for every other list.
pub const COMPLETION_ENV_MAX_OTHER: usize = 512;

/// Hard ceiling on the msgpack encoding of a [`CompletionEnv`].
///
/// This is C23's broadcast budget. The env is pushed to every webview on every
/// `StateChanged`, down the same channel that refuses to inline a 1.5 MB SVG, so
/// exceeding it here would be the graph payload's forbidden behaviour committed
/// on every command instead of once per plot.
///
/// **The ceiling is enforced by construction, not by cap arithmetic.** See
/// [`CompletionEnv::enforce_bounds`]. W00 correctly reported that the count caps
/// and this ceiling cannot both hold as *independent* declarations — at
/// `COMPLETION_ENV_MAX_VARS` variables with Stata's longest legal 32-byte names,
/// `varnames` alone encodes to 2048 × 34 = 69 632 bytes, and the ten
/// `COMPLETION_ENV_MAX_OTHER` lists would add 174 080 more. The architect's
/// ruling was that they are not independent: they are two bounds on one value,
/// and whichever binds first wins. The counts cap what is *offered*; this ceiling
/// caps what is *sent*.
///
/// Consequences, both intended:
/// - A realistic session (Stata-typical names) is nowhere near the ceiling, so
///   all 2048 variables ship and nothing changes.
/// - A pathological dataset sheds entries, sets `truncated`, and the popup pages
///   for the rest through `CompletionEnvPage` — an explicit interaction, never a
///   keystroke.
pub const COMPLETION_ENV_MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CompletionEnv {
    pub generation: u64,
    pub frame: String,
    pub frames: Vec<String>,
    /// Storage order, truncated at [`COMPLETION_ENV_MAX_VARS`].
    pub varnames: Vec<String>,
    /// True variable count, so the popup can say "2048 of 32767".
    pub var_total: u32,
    pub truncated: bool,
    // NOTE: `var_labels` is deliberately ABSENT. Labels decorate at most the 12
    // visible popup rows; the popup fetches those through `variables_list`, off
    // the keystroke path. Shipping 32 767 labels to decorate 12 rows was the
    // single largest item in this struct.
    pub locals: Vec<String>,
    pub globals: Vec<String>,
    pub scalars: Vec<String>,
    pub matrices: Vec<String>,
    pub programs: Vec<String>,
    /// e(N), e(r2), … plus e(b) colnames.
    pub e_names: Vec<String>,
    pub r_names: Vec<String>,
    pub value_labels: Vec<String>,
    pub stored_estimates: Vec<String>,
    pub cwd: Utf8PathBuf,
}

/// Wire cost of a msgpack string of `len` bytes: the length prefix plus the
/// payload. Deliberately an upper bound — `enforce_bounds` must never
/// under-estimate, or the ceiling it guarantees would not hold.
const fn str_wire_len(len: usize) -> usize {
    let header = if len < 32 {
        1 // fixstr
    } else if len < 256 {
        2 // str8
    } else if len < 65_536 {
        3 // str16
    } else {
        5 // str32
    };
    header + len
}

/// Wire cost of an array header holding `n` elements.
const fn array_header_len(n: usize) -> usize {
    if n < 16 {
        1 // fixarray
    } else if n < 65_536 {
        3 // array16
    } else {
        5 // array32
    }
}

fn list_wire_len(items: &[String]) -> usize {
    array_header_len(items.len()) + items.iter().map(|s| str_wire_len(s.len())).sum::<usize>()
}

impl CompletionEnv {
    /// The order in which lists are shed when the byte ceiling binds, least
    /// valuable first.
    ///
    /// `programs` leads because it is the longest list and the least specific to
    /// the user's session — it is largely the ado command index, which the
    /// deterministic command table already covers. `locals` and `globals` are
    /// last because they are the user's own macros, are tiny, and are the one
    /// thing no other completion source can reconstruct. `varnames` sits between:
    /// it is the most-completed list in a statistics IDE, so it is shed only
    /// after the bulk low-value lists are exhausted.
    fn shed_order(&mut self) -> [&mut Vec<String>; 11] {
        [
            &mut self.programs,
            &mut self.value_labels,
            &mut self.stored_estimates,
            &mut self.matrices,
            &mut self.scalars,
            &mut self.r_names,
            &mut self.e_names,
            &mut self.frames,
            &mut self.varnames,
            &mut self.globals,
            &mut self.locals,
        ]
    }

    /// Every list bounded by [`COMPLETION_ENV_MAX_OTHER`].
    ///
    /// `varnames` is deliberately absent: it has its own, larger cap.
    fn capped_lists(&mut self) -> [&mut Vec<String>; 10] {
        [
            &mut self.frames,
            &mut self.locals,
            &mut self.globals,
            &mut self.scalars,
            &mut self.matrices,
            &mut self.programs,
            &mut self.e_names,
            &mut self.r_names,
            &mut self.value_labels,
            &mut self.stored_estimates,
        ]
    }

    /// An upper bound on this value's `rmp_serde::to_vec_named` encoding.
    ///
    /// Computed analytically rather than by encoding, because `rmp-serde` is a
    /// dev-dependency here: `stratum-proto` defines the wire types but does not
    /// link a codec. `tests/roundtrip.rs` asserts this bound is never below the
    /// real encoded length.
    pub fn encoded_len_upper_bound(&self) -> usize {
        // 16 fields, so a map16 header, plus each field's key.
        const MAP_HEADER: usize = 3;
        const KEYS: usize = 1 + 10   // generation
            + 1 + 5                  // frame
            + 1 + 6                  // frames
            + 1 + 8                  // varnames
            + 1 + 9                  // var_total
            + 1 + 9                  // truncated
            + 1 + 6                  // locals
            + 1 + 7                  // globals
            + 1 + 7                  // scalars
            + 1 + 8                  // matrices
            + 1 + 8                  // programs
            + 1 + 7                  // e_names
            + 1 + 7                  // r_names
            + 1 + 12                 // value_labels
            + 1 + 16                 // stored_estimates
            + 1 + 3; // cwd
                     // Scalars at their widest msgpack encoding.
        const SCALARS: usize = 9 /* u64 */ + 5 /* u32 */ + 1 /* bool */;

        MAP_HEADER
            + KEYS
            + SCALARS
            + str_wire_len(self.frame.len())
            + str_wire_len(self.cwd.as_str().len())
            + list_wire_len(&self.frames)
            + list_wire_len(&self.varnames)
            + list_wire_len(&self.locals)
            + list_wire_len(&self.globals)
            + list_wire_len(&self.scalars)
            + list_wire_len(&self.matrices)
            + list_wire_len(&self.programs)
            + list_wire_len(&self.e_names)
            + list_wire_len(&self.r_names)
            + list_wire_len(&self.value_labels)
            + list_wire_len(&self.stored_estimates)
    }

    /// Apply both bounds, and set [`CompletionEnv::truncated`] if either bit.
    ///
    /// Producers call this before the env leaves the engine. It is idempotent.
    /// `var_total` is left alone: it reports the true variable count so the popup
    /// can say "1 203 of 32 767" regardless of how much was shed.
    pub fn enforce_bounds(&mut self) {
        if self.varnames.len() > COMPLETION_ENV_MAX_VARS {
            self.varnames.truncate(COMPLETION_ENV_MAX_VARS);
            self.truncated = true;
        }
        let mut clamped = false;
        for list in self.capped_lists() {
            if list.len() > COMPLETION_ENV_MAX_OTHER {
                list.truncate(COMPLETION_ENV_MAX_OTHER);
                clamped = true;
            }
        }
        self.truncated |= clamped;

        let mut size = self.encoded_len_upper_bound();
        if size <= COMPLETION_ENV_MAX_BYTES {
            return;
        }
        self.truncated = true;
        for list in self.shed_order() {
            while size > COMPLETION_ENV_MAX_BYTES {
                match list.pop() {
                    Some(name) => size -= str_wire_len(name.len()),
                    None => break,
                }
            }
            if size <= COMPLETION_ENV_MAX_BYTES {
                break;
            }
        }
    }
}
