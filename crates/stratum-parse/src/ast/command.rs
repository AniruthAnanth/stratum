//! `CommandAst` — one parsed Stata command. Design 02 §6.2, transcribed.
//!
//! The universal syntax (02 §6.1) is
//!
//! ```text
//! [prefix [args] :] command [varlist] [=exp] [if exp] [in range] [weight] [using file] [, options]
//! ```
//!
//! Two things in that grammar are load-bearing and easy to get wrong, so they
//! are recorded here next to the types that encode them:
//!
//! * `if` and `in` may appear in EITHER order [V].
//! * **Options need not be contiguous and may be re-entered.**
//!   `summarize a b, detail, if x==1, noformat` is legal — a second comma
//!   returns to the command line ([U] 11.1.7). [`Options`] is therefore a list
//!   built by a loop that can re-enter the qualifier grammar, not the result of
//!   splitting on the first comma.
//!
//! Design 02 §6.2 calls this type `Stmt`; CONTRACTS §1.2 and ARCHITECTURE §5
//! call it `CommandAst`. It is one type with both names — see [`Stmt`].
//!
//! Design 02 §6.2 spells the small owned strings `compact_str::CompactString`.
//! They are `String` here because `compact_str` is not in the workspace
//! dependency table (root `Cargo.toml`, W00's file) and a member crate taking a
//! dependency outside that table is how the workspace ends up resolving two
//! versions of one crate. Swapping the alias later is mechanical.

use smallvec::SmallVec;
use stratum_proto::{DirectiveKind, Span};

use crate::ast::{Expr, Format, NumList, VarList};
use crate::cmdsig::CmdId;

/// One parsed command, with its prefix chain.
///
/// Design 02 §6.2 names this `Stmt`; CONTRACTS §13 names it `CommandAst` in
/// `EffectTable::effects`. [`Stmt`] is an alias so code written from either
/// document compiles.
#[derive(Clone, PartialEq, Debug)]
pub struct CommandAst {
    /// Span in the macro-EXPANDED text.
    pub span: Span,
    /// Span in the ORIGINAL source, through the composed
    /// [`crate::SpanMap`](crate::spanmap::SpanMap).
    pub src: Span,
    /// The prefix chain, outermost first.
    pub prefixes: SmallVec<[Prefix; 2]>,
    /// The command itself.
    pub cmd: Command,
}

/// Design 02 §6.2's name for [`CommandAst`].
pub type Stmt = CommandAst;

/// One prefix in the chain. `capture`, `noisily`, `quietly` and `version` may
/// omit the colon; every other prefix requires it ([U] 11.1.10).
#[derive(Clone, PartialEq, Debug)]
pub enum Prefix {
    /// `by`/`bysort`.
    By(ByPrefix),
    /// `quietly:`.
    Quietly {
        /// Extent of the prefix word.
        span: Span,
    },
    /// `noisily:`.
    Noisily {
        /// Extent of the prefix word.
        span: Span,
    },
    /// `capture:`.
    Capture {
        /// Extent of the prefix word.
        span: Span,
    },
    /// `version 17:`.
    Version {
        /// The version as typed.
        ver: String,
        /// Extent of the whole prefix.
        span: Span,
    },
    /// `frame default:` (v2).
    Frame {
        /// Frame name as typed.
        name: String,
        /// Extent of the whole prefix.
        span: Span,
    },
    /// `statsby`, `rolling`, `bootstrap`, `jackknife`, `permute`, `simulate`,
    /// `svy`, `mi estimate`, `bayes`, `fmm`, `nestreg`, `stepwise`, `xi`, `fp`,
    /// `mfp` — all v2.
    Generic {
        /// Prefix command name as typed.
        name: String,
        /// Extent of the prefix's own arguments.
        args: Span,
        /// Extent of the whole prefix.
        span: Span,
    },
}

/// The prefix kinds, without payloads — what a region head records.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PrefixKind {
    /// `by`/`bysort`.
    By,
    /// `quietly`.
    Quietly,
    /// `noisily`.
    Noisily,
    /// `capture`.
    Capture,
    /// `version`.
    Version,
    /// `frame`.
    Frame,
    /// Any other prefix command.
    Generic,
}

/// `by a b:` → `group = [a, b]`, `extra_sort = []`.
/// `bysort a (b):` → `group = [a]`, `extra_sort = [b]` (sort-only keys).
#[derive(Clone, PartialEq, Debug)]
pub struct ByPrefix {
    /// Grouping variables.
    pub group: VarList,
    /// Sort-only keys, from the parenthesised tail.
    pub extra_sort: VarList,
    /// `bysort`, or `by …, sort`.
    pub sort: bool,
    /// Extent of the whole prefix.
    pub span: Span,
}

/// The command in a [`CommandAst`].
#[derive(Clone, PartialEq, Debug)]
pub enum Command {
    /// Resolved against the command table.
    Known(Box<KnownCommand>),
    /// Structural — the executor handles it itself.
    Block(Box<BlockCommand>),
    /// `#delimit cr|;`.
    Directive(DirectiveKind),
    /// Unresolved: an ado-file, a typo, or a v2 command.
    Unknown {
        /// The word as typed.
        name: String,
        /// Extent of the command word.
        name_span: Span,
        /// Everything after it, verbatim.
        rest: RawArgs,
    },
}

/// A command that resolved against the table.
#[derive(Clone, PartialEq, Debug)]
pub struct KnownCommand {
    /// Row in the command table.
    pub id: CmdId,
    /// Extent of the command word as typed.
    pub name_span: Span,
    /// The universal-syntax slots.
    pub slots: Slots,
}

/// The universal-syntax slots of 02 §6.1.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Slots {
    /// `varlist`.
    pub varlist: Option<VarList>,
    /// The `= exp` slot (`generate`, `replace`, `egen`, `scalar`, …).
    pub assign: Option<Expr>,
    /// `if exp`.
    pub if_: Option<Expr>,
    /// `in range`.
    pub in_: Option<InRange>,
    /// `[weight]`.
    pub weight: Option<Weight>,
    /// `using filename`.
    pub using: Option<FileSpec>,
    /// Options, in the order typed.
    pub options: Options,
    /// Command-specific positional tail the universal grammar cannot classify:
    /// `label define lbl 1 "a" 2 "b"`, `matrix M = (1,2\3,4)`, `graph twoway …`.
    /// Kept verbatim with its span so each command impl runs its own mini-parser.
    pub rest: Option<RawArgs>,
}

/// Verbatim text plus its extent.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RawArgs {
    /// Text as typed.
    pub text: String,
    /// Extent in the text this was parsed from.
    pub span: Span,
}

/// `in 1/10`, `in -5/l`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InRange {
    /// Lower bound.
    pub from: ObsRef,
    /// Upper bound.
    pub to: ObsRef,
    /// Extent of the qualifier.
    pub span: Span,
}

/// An observation reference in an `in` range. Negative numbers count from the
/// end ([U] 11.1.4).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ObsRef {
    /// `f` / `F`.
    First,
    /// `l` / `L`.
    Last,
    /// A literal observation number.
    Num(i64),
}

/// `[fweight = n]`.
#[derive(Clone, PartialEq, Debug)]
pub struct Weight {
    /// Which weight kind.
    pub kind: WeightKind,
    /// The weight expression.
    pub expr: Expr,
    /// Extent of the whole bracketed clause.
    pub span: Span,
}

/// The weight kinds.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum WeightKind {
    /// `[= exp]` — the command's default kind.
    Default,
    /// `fweight`.
    FWeight,
    /// `pweight`.
    PWeight,
    /// `aweight`.
    AWeight,
    /// `iweight`.
    IWeight,
}

/// `using "file.dta"`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileSpec {
    /// The path as typed, quotes included.
    pub raw: String,
    /// Extent of the qualifier.
    pub span: Span,
}

/// The option list. Not a map: an option may legally be re-entered, and the
/// LAST spelling wins in Stata, so order is part of the meaning.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Options {
    /// Options in the order typed.
    pub items: Vec<OptionItem>,
}

/// One option.
#[derive(Clone, PartialEq, Debug)]
pub struct OptionItem {
    /// As typed, e.g. `d`.
    pub name: String,
    /// Resolved spelling, e.g. `detail`.
    pub canonical: Option<&'static str>,
    /// `nodetail`.
    pub negated: bool,
    /// The argument, if any.
    pub arg: Option<OptionArg>,
    /// Extent of the option.
    pub span: Span,
}

/// An option's argument.
///
/// The generic parser produces [`OptionArg::Raw`] (paren-balanced, quote-aware);
/// the command signature then re-parses each recognised option into a typed
/// variant. Unknown options on a known command become an r(198) diagnostic;
/// unknown options on [`Command::Unknown`] stay `Raw` and pass through, which is
/// what lets an ado-file work without a table row.
#[derive(Clone, PartialEq, Debug)]
pub enum OptionArg {
    /// Shallow parse: paren-balanced text.
    Raw(RawArgs),
    /// Integer.
    Int(i64),
    /// Real.
    Real(f64),
    /// String.
    Str(String),
    /// Numlist.
    Numlist(NumList),
    /// Expression list.
    Exprs(Vec<Expr>),
    /// Varlist.
    VarList(VarList),
    /// Display format.
    Fmt(Format),
}

/// Structural commands the executor handles itself.
///
/// **Loop bodies are a `Span` into the PRE-EXPANSION logical-line text, never a
/// parsed AST.** Stata re-expands the body on every iteration — that is how
/// `` `x' `` picks up the new loop value — and it is what makes `foreach` cheap
/// [V]. The executor re-runs expansion and parsing per iteration.
#[derive(Clone, PartialEq, Debug)]
pub enum BlockCommand {
    /// `foreach x of varlist a b { … }`.
    Foreach {
        /// The loop variable name.
        loopvar: String,
        /// What is being looped over.
        source: ForeachSource,
        /// Body extent, pre-expansion.
        body: Span,
    },
    /// `forvalues i = 1/10 { … }`.
    Forvalues {
        /// The loop variable name.
        loopvar: String,
        /// The numeric range.
        range: NumRange,
        /// Body extent, pre-expansion.
        body: Span,
    },
    /// `while cond { … }`.
    While {
        /// Loop condition.
        cond: Expr,
        /// Body extent, pre-expansion.
        body: Span,
    },
    /// `if … { } else if … { } else { }`. The last arm with `None` is `else`.
    IfElse {
        /// One `(condition, body)` per arm.
        arms: Vec<(Option<Expr>, Span)>,
    },
    /// `program define name … end`.
    Program {
        /// Program name.
        name: String,
        /// Everything after the name, verbatim.
        opts: RawArgs,
        /// Body extent, captured verbatim.
        body: Span,
    },
    /// `input a b … end`.
    Input {
        /// The variables being input.
        spec: VarList,
        /// The data lines.
        data: Span,
    },
    /// `mata: … end` (v2: handed to the Mata front end).
    Mata {
        /// Body extent.
        body: Span,
    },
    /// `python: … end` (v2).
    Python {
        /// Body extent.
        body: Span,
    },
    /// `capture { … }`.
    Capture {
        /// Body extent.
        body: Span,
    },
    /// `quietly { … }`.
    Quietly {
        /// Body extent.
        body: Span,
    },
    /// `noisily { … }`.
    Noisily {
        /// Body extent.
        body: Span,
    },
    /// A bare `{ … }`.
    Anonymous {
        /// Body extent.
        body: Span,
    },
}

/// What a `foreach` loops over.
#[derive(Clone, PartialEq, Debug)]
pub enum ForeachSource {
    /// `foreach x in a b c`.
    In(RawArgs),
    /// `foreach x of local L`.
    OfLocal(String),
    /// `foreach x of global G`.
    OfGlobal(String),
    /// `foreach x of varlist a b`.
    OfVarlist(VarList),
    /// `foreach x of newlist a b`.
    OfNewlist(VarList),
    /// `foreach x of numlist 1/10`.
    OfNumlist(NumList),
}

/// `forvalues i = from(step)to`.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct NumRange {
    /// First value.
    pub from: f64,
    /// Step, when given.
    pub step: Option<f64>,
    /// Last value.
    pub to: f64,
}
