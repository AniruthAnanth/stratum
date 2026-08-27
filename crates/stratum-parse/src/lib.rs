//! Stratum's Stata language front end.
//!
//! W04 ships the scanner half: the logical-line reader (design 02 §§2–3), the
//! logical-executable-region segmenter (02 §5 / spec §2), the canonical token
//! stream behind `CodeHash` (CONTRACTS §1.2), and the `CommandAst` /
//! `CommandSig` declarations wave 2 codes against (A29).
//!
//! W04b ships the dynamic half, in pipeline order (02 §1):
//! [`macros::expand`] → [`lex`] → [`parse::parse_command`] → [`varlist`], plus
//! the generated [`cmdtable`] the parser is driven by and the deterministic
//! [`lints`].
//!
//! **PARTITION NOTE (W04b).** The `pub mod` declarations for those six modules
//! are the ONLY change W04b makes to this file, and R0 gives it to W04. A
//! module cannot be declared anywhere else in Rust, and `ast/mod.rs` carries a
//! W04-written note sanctioning the equivalent edit there — this is the same
//! edit, in the file the note forgot. Escalated in W04b's return; nothing else
//! here is touched.
//!
//! # The one thing to know before using this crate
//!
//! [`segment`] must be entered at byte 0 of a file, or resumed with
//! [`resegment`]. The delimiter mode at any byte depends on all preceding text
//! (`#delimit ;` is file-scoped), so segmenting a fragment without passing its
//! [`Region::entry_delimiter`] through [`SegmentOptions::initial_delimiter`]
//! silently mis-parses every `;`-mode block. This is the mistake design 02 §13.2
//! calls out by name, and it is why every region carries the mode it started in.
//!
//! # Layering
//!
//! ARCHITECTURE §8.4: this crate builds for `wasm32-unknown-unknown`, reaches no
//! filesystem, no clock, no locale and no async runtime, and is pure — the same
//! input gives the same output on every platform, which is what lets the editor
//! segment in wasm and the engine re-segment natively and compare the two
//! (CONTRACTS §14's parity gate).

pub mod ast;
pub mod canon;
pub mod cmdsig;
pub mod cmdtable;
pub mod lex;
pub mod lineindex;
pub mod lints;
pub mod macros;
pub mod parse;
pub mod scan;
pub mod spanmap;
pub mod varlist;

/// **AMENDED (A10).** `Span` is declared in `stratum-proto` and NOWHERE else.
/// A structurally identical twin with no conversion between them is a
/// silent-at-compile-time class of bug, so this crate re-exports rather than
/// redeclaring — there is deliberately no `src/span.rs`.
pub use stratum_proto::Span;

pub use ast::{CommandAst, Stmt};
pub use canon::{canonical_tokens, code_hash, text_hash};
pub use cmdsig::{CmdId, CommandLookup, CommandSig, CommandTable, OptionSpec};
pub use lineindex::LineIndex;
pub use scan::{
    resegment, resegment_with_stats, segment, segment_with, Derived, DerivedText, HeadInfo,
    IdxRange, LogicalLine, PrefixChain, Region, RegionShape, ResegmentStats, ScanState,
    SegmentOptions, Segmentation, SourceEdit,
};
pub use spanmap::SpanMap;

pub use cmdtable::{all_commands, all_functions, function, resolve_command, table, FnRet, FnSig};
pub use lex::{lex, tokens, LexMode, Tok, TokKind};
pub use lints::{lint, LintCtx, StataError};
pub use macros::{expand, ExpandHost, ExpandStats, Expansion, MacroEnv, NoHost};
pub use parse::{
    parse_command, parse_command_counted, parse_expr, parse_speculative, ParseMode, SyntaxSpec,
};
pub use varlist::{expand_varlist, VarIndex, VarlistCtx, VarlistMode};
