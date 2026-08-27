//! This crate's rows of the static effect table — design 03 §5.1–5.3, A1.
//!
//! [`EffectTable`] is declared in `stratum-effects` and has **no default
//! method**, so a command cannot exist in this engine without someone writing
//! down what it does. `stratum-stats` declares its own seven rows; everything
//! else in `stratum_parse`'s command table is declared here.
//!
//! # The one rule, and it only points one way
//!
//! Every set is a MAY-set biased toward "yes". Under-approximating a read or
//! write set leaves a downstream block marked `Current` after its input changed
//! — INV-1 violated, a wrong number in a paper. Over-approximating costs a
//! spurious re-run. So an unresolvable varlist becomes [`VarSet::unknown`] and
//! never an empty set, and every `_ =>` arm in this file falls to
//! [`EffectSet::unknown_all`] rather than to `EffectSet::new()`.
//!
//! # Why the rows are a two-step lookup and not one big `match`
//!
//! [`shape`] maps a canonical name to a behavioural category; [`row`] turns a
//! category plus the parsed slots into an `EffectSet`. That split is what makes
//! [`EffectTable::is_known_command`] and `effects` answer from the **same**
//! list. A second `const COMMANDS: &[&str]` would be a list someone has to
//! remember to update, and the failure when they forget is silent: the command
//! runs, the table says "unknown", every downstream block goes permanently
//! stale, and nothing is red. `every_command_in_the_table_has_a_row` below is
//! the test that keeps it honest as `data/commands.ron` grows.
//!
//! # The one thing this vocabulary cannot say
//!
//! `EffectSet` has `reads_metadata` but no `writes_metadata`. `label variable x`
//! and `format x %9.2f` change what `describe` prints without changing a single
//! value, and there is no field for that. The sound encoding available is to put
//! the touched variable in `writes` — a superset of the truth, so a downstream
//! block that reads `x` goes stale on a pure relabel. That is a spurious re-run,
//! which is the direction the rule permits. Escalated in W06a's return; the fix
//! is a `writes_metadata: VarSet` on a crate this unit does not own.

use camino::Utf8PathBuf;
use stratum_effects::{
    Atomicity, CwdEffect, EffectSet, EffectTable, FileSet, FrameEffect, Name, NameSet, RngEffect,
    RwEffect, StaticCtx, VarPattern as SetPattern, VarSet,
};
use stratum_parse::ast::command::{BlockCommand, Command, OptionArg, Slots};
use stratum_parse::ast::expr::{Expr, StoredClass, SysVar};
use stratum_parse::ast::varlist::{VarItem, VarItemKind, VarList, VarPattern};
use stratum_parse::CommandAst;
use stratum_proto::{Confidence, Taint, Tri};

/// The canonical names whose rows belong to **`stratum-stats`** (A1).
///
/// Listed so that [`shape`] can refuse them explicitly rather than by omission:
/// an omission is indistinguishable from a forgotten command, and the coverage
/// test below could not tell the two apart.
pub const OWNED_BY_STATS: &[&str] = &[
    "correlate",
    "predict",
    "pwcorr",
    "regress",
    "summarize",
    "tabulate",
    "ttest",
];

/// `stratum-runtime`'s rows of the static effect table.
#[derive(Clone, Copy, Debug, Default)]
pub struct BuiltinEffects;

impl EffectTable for BuiltinEffects {
    fn effects(&self, cmd: &CommandAst, ctx: &StaticCtx<'_>) -> EffectSet {
        let Command::Known(k) = &cmd.cmd else {
            // `Command::Block` is control flow, and its effects are the union of
            // its body's — which needs the body TEXT, which a `CommandAst` alone
            // does not carry. `crate::extract` is the entry point that has it.
            // `Command::Unknown` is design 03 §5.3 rule 7's first bullet.
            let mut e = EffectSet::unknown_all();
            // The one thing a `Block` says about itself without its body:
            // `mata:` and `python:` leave the engine. That is the difference
            // between a block that goes Stale and one that can only ever be
            // CurrentUnverifiable, and losing it here would make a caller that
            // goes straight to the table disagree with `crate::extract`.
            if matches!(
                &cmd.cmd,
                Command::Block(b)
                    if matches!(**b, BlockCommand::Mata { .. } | BlockCommand::Python { .. })
            ) {
                e.taint |= Taint::EXTERNAL;
            }
            return e;
        };
        let name = stratum_parse::cmdtable::command(k.id).canonical;
        let Some(shape) = shape(name) else {
            return EffectSet::unknown_all();
        };
        let mut e = row(shape, &k.slots, ctx);
        apply_prefixes(cmd, &mut e, ctx);
        e
    }

    fn is_known_command(&self, name: &str) -> bool {
        shape(name).is_some()
    }
}

/// Two effect tables as one, first match wins.
///
/// The composition `stratum-exec` needs: this crate declares the built-in
/// surface and `stratum-stats` declares its own, and neither may depend on the
/// other (A1). Chaining is the only place the two meet, and it lives here
/// because it is trivial and because a copy in every consumer would be three
/// copies of one precedence rule.
pub struct Chain<'a> {
    tables: &'a [&'a dyn EffectTable],
}

impl<'a> Chain<'a> {
    /// Chain, in priority order.
    #[must_use]
    pub fn new(tables: &'a [&'a dyn EffectTable]) -> Self {
        Chain { tables }
    }
}

impl EffectTable for Chain<'_> {
    fn effects(&self, cmd: &CommandAst, ctx: &StaticCtx<'_>) -> EffectSet {
        if let Command::Known(k) = &cmd.cmd {
            let name = stratum_parse::cmdtable::command(k.id).canonical;
            for t in self.tables {
                if t.is_known_command(name) {
                    return t.effects(cmd, ctx);
                }
            }
        }
        EffectSet::unknown_all()
    }

    fn is_known_command(&self, name: &str) -> bool {
        self.tables.iter().any(|t| t.is_known_command(name))
    }
}

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// What a command does with its slots, at the granularity the staleness model
/// can tell apart.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Shape {
    /// `use`, `sysuse`, `webuse`, `import` — the frame is replaced from a file.
    Load,
    /// `save`, `export` — bytes leave the engine.
    Store,
    /// `append`, `merge` — rows and columns arrive from a file.
    Combine,
    /// `clear`.
    Clear,
    /// `describe`, `ds`, `codebook`, `inspect` — read values and metadata.
    Report,
    /// `list`, `browse` — the same, plus the answer depends on row order.
    ListRows,
    /// `count`.
    Count,
    /// `tabstat`.
    TabStat,
    /// `generate`.
    Create,
    /// `egen` — the egen function name is not in the expression grammar, so
    /// what it reads is not statically visible.
    CreateOpaque,
    /// `replace`.
    Replace,
    /// `recode`, `encode`, `decode`, `destring`, `tostring`.
    Transform,
    /// `compress` — storage types only, never a value.
    Compress,
    /// `drop` (`false`) and `keep` (`true`).
    DropKeep(bool),
    /// `rename`.
    Rename,
    /// `order`.
    Reorder,
    /// `sort`, `gsort`.
    Sort,
    /// `label`, `format`, `notes`, `char` — see the module header on metadata.
    MetaWrite,
    /// `expand`, `contract`, `collapse`, `reshape`.
    Reshape,
    /// `display`.
    Display,
    /// `local`, `global`, `macro`, `tempvar`, `tempname`, `tempfile`, `args`,
    /// `syntax`.
    MacroWrite,
    /// `scalar`.
    ScalarWrite,
    /// `matrix`.
    MatrixWrite,
    /// `return`, `ereturn`, `sreturn`.
    ReturnCmd,
    /// `set`.
    SetCmd,
    /// `log`, `cmdlog`.
    LogCmd,
    /// `do`, `run`, `include` — arbitrary source, from a file.
    RunFile,
    /// `program`.
    ProgramDef,
    /// `preserve`, `restore`.
    Preserve,
    /// `discard`.
    Discard,
    /// `exit`, `error`, `continue`, `help`, `which`, `version`, `confirm`.
    Inert,
    /// The e-class commands this build parses but `stratum-stats` has not
    /// claimed a row for.
    Estimation,
    /// `test`, `testparm`, `estat`, `estimates`.
    PostEst,
    /// `graph`, `histogram`, `twoway`, `scatter`, `line`.
    Graph,
    /// `assert`.
    Assert,
    /// `foreach`, `forvalues`, `while` reaching the table as a KNOWN command
    /// rather than a `Command::Block` — a malformed loop head. Nothing is known
    /// about a body that did not parse.
    Loop,
    /// `mata`, `python`, `java` — outside the engine entirely.
    External,
    /// `frame`.
    FrameCmd,
    /// The prefix words, when one reaches the table as a command in its own
    /// right (`capture` on its own line opening a brace block).
    PrefixWord,
    /// `input`.
    Input,
    /// `cd`, `pwd`, `erase` — the working directory and the filesystem.
    Cwd,
    /// `creturn` — reads `c()` and prints. It writes nothing, which is what
    /// separates it from `return`/`ereturn`/`sreturn`.
    CReturn,
}

/// Canonical name → behavioural category, or `None` when this table has no
/// business answering for it.
///
/// The `None` arms are the two honest ones: a name `stratum-stats` owns, and a
/// name nobody has declared. Both send the caller to
/// [`EffectSet::unknown_all`], which is the only sound answer.
fn shape(name: &str) -> Option<Shape> {
    Some(match name {
        "use" | "sysuse" | "webuse" | "import" => Shape::Load,
        "save" | "export" => Shape::Store,
        "append" | "merge" => Shape::Combine,
        "clear" => Shape::Clear,
        "describe" | "ds" | "codebook" | "inspect" => Shape::Report,
        "list" | "browse" => Shape::ListRows,
        "count" => Shape::Count,
        "tabstat" => Shape::TabStat,
        "generate" => Shape::Create,
        "egen" => Shape::CreateOpaque,
        "replace" => Shape::Replace,
        "recode" | "encode" | "decode" | "destring" | "tostring" => Shape::Transform,
        "compress" => Shape::Compress,
        "drop" => Shape::DropKeep(false),
        "keep" => Shape::DropKeep(true),
        "rename" => Shape::Rename,
        "order" => Shape::Reorder,
        "sort" | "gsort" => Shape::Sort,
        "label" | "format" | "notes" | "char" => Shape::MetaWrite,
        "expand" | "contract" | "collapse" | "reshape" => Shape::Reshape,
        "display" => Shape::Display,
        "local" | "global" | "macro" | "tempvar" | "tempname" | "tempfile" | "args" | "syntax" => {
            Shape::MacroWrite
        }
        "scalar" => Shape::ScalarWrite,
        "matrix" => Shape::MatrixWrite,
        "return" | "ereturn" | "sreturn" => Shape::ReturnCmd,
        "creturn" => Shape::CReturn,
        "set" => Shape::SetCmd,
        "log" | "cmdlog" => Shape::LogCmd,
        "do" | "run" | "include" => Shape::RunFile,
        "program" => Shape::ProgramDef,
        "preserve" | "restore" => Shape::Preserve,
        "discard" => Shape::Discard,
        "exit" | "error" | "continue" | "help" | "which" | "version" | "confirm" => Shape::Inert,
        "anova" | "areg" | "ivregress" | "logit" | "probit" | "mlogit" | "ologit" | "oprobit"
        | "poisson" | "nbreg" | "tobit" | "xtreg" => Shape::Estimation,
        "test" | "testparm" | "estat" | "estimates" => Shape::PostEst,
        "graph" | "histogram" | "twoway" | "scatter" | "line" => Shape::Graph,
        "assert" => Shape::Assert,
        "foreach" | "forvalues" | "while" => Shape::Loop,
        "mata" | "python" | "java" => Shape::External,
        "frame" => Shape::FrameCmd,
        "by" | "bysort" | "quietly" | "noisily" | "capture" | "bootstrap" | "jackknife"
        | "statsby" | "svy" | "xi" => Shape::PrefixWord,
        "input" => Shape::Input,
        "cd" | "pwd" | "erase" => Shape::Cwd,
        _ => return None,
    })
}

#[allow(clippy::too_many_lines)] // one arm per category; splitting it hides the table
fn row(shape: Shape, s: &Slots, ctx: &StaticCtx<'_>) -> EffectSet {
    let mut e = EffectSet::new();

    // Rules 1 and 3, applied once for every command that has the slots: the
    // `if` and `weight` expressions are read, and `in` makes the answer depend
    // on which observations are where.
    if let Some(x) = &s.if_ {
        expr_effects(x, ctx, &mut e);
    }
    if let Some(w) = &s.weight {
        expr_effects(&w.expr, ctx, &mut e);
    }
    if s.in_.is_some() {
        e.order_sensitive = true;
    }

    let vars = varset(s.varlist.as_ref(), ctx);
    // Rule 7's first bullet, recorded once for every shape rather than in each
    // of them: an unexpanded macro in a varlist position is why this answer is
    // weaker than exact, and `Taint` is where "why" lives.
    if varlist_dynamic(s.varlist.as_ref()) {
        e.taint |= Taint::MACRO_VARLIST;
        e.confidence = Confidence::Speculative;
    }

    match shape {
        Shape::Load => {
            // Everything about the frame changes, including which variables
            // exist, so `creates`/`drops` are `unknown` and not "the file's
            // variables" — that would need to read the file.
            e.frame = FrameEffect::ReplaceCurrent;
            e.creates = VarSet::unknown();
            e.drops = VarSet::unknown();
            e.row_membership = Tri::Yes;
            e.row_order = Tri::Yes;
            e.reads_metadata = true;
            e.file_reads = file_of(s, ctx);
            e.cwd = CwdEffect::Reads;
            e.rclass = RwEffect::Write;
        }
        Shape::Store => {
            e.reads = if vars.is_empty() {
                VarSet::unknown()
            } else {
                vars
            };
            e.reads_metadata = true;
            e.file_writes = file_of(s, ctx);
            e.cwd = CwdEffect::Reads;
            // The bytes on disk are not in the undo journal (ARCHITECTURE §7.6).
            e.atomicity = Atomicity::External;
        }
        Shape::Combine => {
            e.frame = FrameEffect::Modify;
            e.reads = VarSet::unknown();
            e.writes = VarSet::unknown();
            e.creates = VarSet::unknown();
            e.row_membership = Tri::Yes;
            e.row_order = Tri::Yes;
            e.reads_metadata = true;
            e.file_reads = file_of(s, ctx);
            e.cwd = CwdEffect::Reads;
            e.rclass = RwEffect::Write;
        }
        Shape::Clear => {
            e.frame = FrameEffect::ReplaceCurrent;
            e.drops = VarSet::unknown();
            e.row_membership = Tri::Yes;
            e.row_order = Tri::Yes;
            // `clear all` takes macros, scalars, matrices, programs and
            // estimates with it. The argument is in `rest`, so the narrow
            // answer is only available when we can read it.
            let all = rest_text(s).is_some_and(|t| {
                let w = t.split_whitespace().next().unwrap_or_default();
                w == "all" || w == "_all" || w == "mata" || w == "programs"
            });
            if all || rest_text(s).is_none() {
                e.macro_writes = NameSet::unknown();
                e.scalar_writes = NameSet::unknown();
                e.matrix_writes = NameSet::unknown();
                e.program_writes = NameSet::unknown();
                e.estimates = RwEffect::Write;
            }
            e.rclass = RwEffect::Write;
        }
        Shape::Report => {
            e.reads = if vars.is_empty() {
                VarSet::unknown()
            } else {
                vars
            };
            e.reads_metadata = true;
            e.rclass = RwEffect::Write;
        }
        Shape::ListRows => {
            e.reads = if vars.is_empty() {
                VarSet::unknown()
            } else {
                vars
            };
            e.reads_metadata = true;
            // `list` prints observations in their current order; a `sort`
            // upstream changes its output without changing a value.
            e.order_sensitive = true;
        }
        Shape::Count => {
            // `count` has no varlist slot; everything it reads came through the
            // `if` expression, already walked above.
            e.rclass = RwEffect::Write;
        }
        Shape::TabStat => {
            e.reads.union(&vars);
            e.reads.union(&option_varset(s, "by", ctx));
            e.reads_metadata = true;
            e.rclass = RwEffect::Write;
        }
        Shape::Create => {
            e.creates = vars;
            if let Some(x) = &s.assign {
                expr_effects(x, ctx, &mut e);
            } else {
                // `generate` with no `= exp` did not parse; do not guess.
                e.reads = VarSet::unknown();
            }
        }
        Shape::CreateOpaque => {
            e.creates = vars;
            // `egen y = rowmean(a b c)` puts a varlist inside what the
            // expression grammar sees as a function call, and `egen y =
            // group(`x')` puts a macro there. Neither is statically readable
            // from `Expr`, so egen reads whatever it likes.
            e.reads = VarSet::unknown();
            e.reads.union(&option_varset(s, "by", ctx));
            e.order_sensitive = true;
        }
        Shape::Replace => {
            e.writes = vars;
            if let Some(x) = &s.assign {
                expr_effects(x, ctx, &mut e);
            } else {
                e.reads = VarSet::unknown();
            }
        }
        Shape::Transform => {
            e.reads.union(&vars);
            let gen = option_varset(s, "generate", ctx);
            if gen.is_empty() {
                // In place.
                e.writes.union(&vars);
            } else {
                e.creates = gen;
            }
            e.reads_metadata = true;
        }
        Shape::Compress => {
            // Storage types, never values: nothing downstream that reads a
            // value can see a difference.
            e.reads = if vars.is_empty() {
                VarSet::unknown()
            } else {
                vars
            };
            e.reads_metadata = true;
        }
        Shape::DropKeep(keep) => {
            if s.if_.is_some() || s.in_.is_some() {
                e.row_membership = Tri::Yes;
                e.frame = FrameEffect::Modify;
            }
            if !vars.is_empty() {
                if keep {
                    // `keep price mpg` drops every OTHER variable, and which
                    // those are is a runtime fact.
                    e.drops = VarSet::unknown();
                } else {
                    e.drops = vars;
                }
            } else if keep && s.if_.is_none() && s.in_.is_none() {
                e.drops = VarSet::unknown();
            }
        }
        Shape::Rename => {
            match rename_pair(s) {
                Some((from, to)) => {
                    e.renames.push((from, to));
                    e.reads_metadata = true;
                }
                None => {
                    // Group rename (`rename (a b) (x y)`, `rename *_1 *`): the
                    // pairing needs the live layout, so nothing is claimed.
                    e.drops = VarSet::unknown();
                    e.creates = VarSet::unknown();
                    e.taint |= Taint::MACRO_VARLIST;
                    e.confidence = Confidence::Speculative;
                }
            }
        }
        Shape::Reorder => {
            // Storage position only. Everything that reads by NAME is
            // unaffected; `ds`, `describe` and a positional `a-z` are not.
            e.reads = if vars.is_empty() {
                VarSet::unknown()
            } else {
                vars
            };
            e.reads_metadata = true;
        }
        Shape::Sort => {
            // `gsort` puts its keys in `rest` (they carry `+`/`-`), so the
            // varlist slot is empty and the honest answer is "any variable".
            e.reads = if vars.is_empty() {
                VarSet::unknown()
            } else {
                vars
            };
            e.row_order = Tri::Yes;
            e.order_sensitive = true;
            e.frame = FrameEffect::Modify;
        }
        Shape::MetaWrite => {
            // See the module header: there is no `writes_metadata`, so the
            // touched variables go in `writes`, which is a superset.
            let touched = rest_names(s, ctx);
            e.reads_metadata = true;
            e.writes = touched;
            e.rclass = RwEffect::Write;
        }
        Shape::Reshape => {
            e.frame = FrameEffect::Modify;
            e.reads = VarSet::unknown();
            e.writes = VarSet::unknown();
            e.creates = VarSet::unknown();
            e.drops = VarSet::unknown();
            e.row_membership = Tri::Yes;
            e.row_order = Tri::Yes;
            e.order_sensitive = true;
            e.reads_metadata = true;
            e.rclass = RwEffect::Write;
        }
        Shape::Display => {
            // `display` takes a list of items in `rest`, not an `Expr`: format
            // directives, string literals and expressions all mixed. Scanning
            // for a bare identifier outside quotes is what separates
            // `display "done"` — which depends on nothing — from
            // `display price[1]`, which depends on the dataset.
            if rest_has_identifier(s) {
                e.reads = VarSet::unknown();
                e.scalar_reads = NameSet::unknown();
                e.rclass = RwEffect::Read;
                e.estimates = RwEffect::Read;
            }
        }
        Shape::MacroWrite => {
            // The name is the first word of `rest`; the value may name
            // anything, so the READ side stays open.
            match rest_first_word(s) {
                Some(n) => e.macro_writes.insert(n),
                None => e.macro_writes = NameSet::unknown(),
            }
            e.macro_reads = NameSet::unknown();
        }
        Shape::ScalarWrite => {
            match scalar_target(s) {
                Some(n) => e.scalar_writes.insert(&n),
                None => e.scalar_writes = NameSet::unknown(),
            }
            e.reads = VarSet::unknown();
            e.scalar_reads = NameSet::unknown();
            e.rclass = RwEffect::Read;
            e.estimates = RwEffect::Read;
        }
        Shape::MatrixWrite => {
            e.matrix_writes = NameSet::unknown();
            e.matrix_reads = NameSet::unknown();
            e.estimates = RwEffect::Read;
        }
        Shape::ReturnCmd => {
            // `return list` reads, `return scalar x = …` writes, and which one
            // it is lives in `rest`. Both, therefore.
            e.rclass = RwEffect::ReadWrite;
            e.estimates = RwEffect::ReadWrite;
            e.scalar_reads = NameSet::unknown();
        }
        Shape::SetCmd => match rest_first_word(s) {
            Some(n) => e.settings_write.push(n.into()),
            None => e.confidence = Confidence::Speculative,
        },
        Shape::LogCmd => {
            e.file_writes = file_of(s, ctx);
            e.cwd = CwdEffect::Reads;
            e.atomicity = Atomicity::External;
        }
        Shape::RunFile => {
            // Arbitrary source from a file we have not read. Rule 7.
            let file = file_of(s, ctx);
            e = EffectSet::unknown_all();
            e.file_reads.union(&file);
            e.taint |= Taint::FILE_DYNAMIC;
        }
        Shape::ProgramDef => {
            // `program define p` writes the program; `program drop p` unwrites
            // it. Both are a write to that name.
            match program_target(s) {
                Some(n) => e.program_writes.insert(&n),
                None => e.program_writes = NameSet::unknown(),
            }
        }
        Shape::Preserve => {
            // `restore` puts back a whole dataset; `preserve` copies one aside.
            // Neither can be described by a variable set.
            e.frame = FrameEffect::Unknown;
            e.reads = VarSet::unknown();
            e.writes = VarSet::unknown();
            e.creates = VarSet::unknown();
            e.drops = VarSet::unknown();
            e.row_membership = Tri::Unknown;
            e.row_order = Tri::Unknown;
        }
        Shape::Discard => {
            e.estimates = RwEffect::Write;
            e.program_writes = NameSet::unknown();
        }
        Shape::Inert => {}
        Shape::Estimation => {
            e.reads.union(&vars);
            e.reads.union(&option_varset(s, "vce", ctx));
            e.reads.union(&option_varset(s, "cluster", ctx));
            e.reads.union(&option_varset(s, "absorb", ctx));
            e.estimates = RwEffect::Write;
            e.rclass = RwEffect::Write;
            e.reads_metadata = true;
        }
        Shape::PostEst => {
            e.estimates = RwEffect::ReadWrite;
            e.rclass = RwEffect::Write;
            e.reads.union(&vars);
            e.matrix_reads = NameSet::unknown();
        }
        Shape::Graph => {
            e.reads = if vars.is_empty() {
                VarSet::unknown()
            } else {
                vars
            };
            e.reads_metadata = true;
            // `saving()` / `export()` put a file on disk; without one the graph
            // is engine state and rolls back with everything else.
            if has_option(s, "saving") || has_option(s, "export") {
                e.file_writes = FileSet::unknown();
                e.atomicity = Atomicity::External;
            }
        }
        Shape::Assert => {
            // Everything came through `if`; a bare `assert` with the expression
            // in `rest` did not parse into the slot.
            if s.if_.is_none() {
                e.reads = VarSet::unknown();
            }
        }
        Shape::Loop => {
            // A loop head that reached the table as a plain command means the
            // body never parsed. Nothing about it is known.
            e = EffectSet::unknown_all();
            e.taint |= Taint::UNBOUNDED_LOOP;
        }
        Shape::External => {
            e = EffectSet::unknown_all();
            e.taint |= Taint::EXTERNAL;
        }
        Shape::FrameCmd => {
            e = EffectSet::unknown_all();
            e.frame = FrameEffect::Unknown;
        }
        Shape::PrefixWord => {
            // A prefix word standing alone wraps a command we cannot see from
            // here — `crate::extract` is what unwraps prefix chains.
            e = EffectSet::unknown_all();
        }
        Shape::Input => {
            e.frame = FrameEffect::Modify;
            e.creates = if vars.is_empty() {
                VarSet::unknown()
            } else {
                vars
            };
            e.row_membership = Tri::Yes;
        }
        Shape::CReturn => {
            // `creturn list` prints the c() values and writes nothing — which
            // is what separates it from `return`/`ereturn`/`sreturn`, whose row
            // declares a write.
            //
            // It reads settings wholesale: c() exposes every `set` value, plus
            // the dataset's own shape. `settings_read` is a list of NAMES with
            // no "all" spelling, and enumerating every setting here would go
            // stale the day one is added — so the row is marked Speculative,
            // which is this table's existing way of saying the dependency is
            // real and not precisely known.
            e.confidence = Confidence::Speculative;
            e.reads_metadata = true;
        }
        Shape::Cwd => {
            // `cd` changes what every later relative path means; `erase`
            // removes a file. Neither is in the undo journal.
            e.cwd = CwdEffect::Changes;
            e.file_writes = FileSet::unknown();
            e.atomicity = Atomicity::External;
        }
    }
    e
}

/// Rule 2, applied where a caller that reaches [`EffectTable::effects`] with a
/// prefixed AST would otherwise lose it.
///
/// `by foreign: summarize price` reads `foreign`. A driver that applied
/// prefixes itself and a table that did not would agree; a caller going
/// straight to the table would not, and the failure is silent — the whole
/// reason this is here rather than only in `crate::extract`. Unioning it twice
/// is idempotent, which is what makes both callers correct.
fn apply_prefixes(cmd: &CommandAst, e: &mut EffectSet, ctx: &StaticCtx<'_>) {
    use stratum_parse::ast::command::Prefix;
    for p in &cmd.prefixes {
        match p {
            Prefix::By(by) => {
                e.reads.union(&varset(Some(&by.group), ctx));
                e.reads.union(&varset(Some(&by.extra_sort), ctx));
                // `by` requires the data sorted on its groups, and `bysort`
                // sorts it.
                e.order_sensitive = true;
                if by.sort {
                    e.row_order = Tri::Yes;
                }
            }
            // Transparent: they change what is printed and how errors are
            // reported, not what is read or written.
            Prefix::Quietly { .. } | Prefix::Noisily { .. } | Prefix::Version { .. } => {}
            // `capture` swallows the error, which is lint R016's business, not
            // an effect. Nothing about the wrapped command's reads changes.
            Prefix::Capture { .. } => {}
            Prefix::Frame { .. } => e.frame = FrameEffect::Unknown,
            Prefix::Generic { .. } => {
                // `bootstrap:`, `svy:` and friends re-run the wrapped command
                // an unknown number of times against resampled data.
                let wrapped = std::mem::replace(e, EffectSet::unknown_all());
                e.union(&wrapped);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Expressions — design 03 §5.3 rule 1
// ---------------------------------------------------------------------------

/// Walk an expression, adding everything it can observe.
///
/// Public because `crate::extract` walks the same expressions for `if`
/// qualifiers on commands whose rows live in another crate, and two walkers
/// would be two answers to "does this expression read `price`".
pub fn expr_effects(e: &Expr, ctx: &StaticCtx<'_>, into: &mut EffectSet) {
    match e {
        Expr::Num(..) | Expr::Missing(..) | Expr::Str(..) => {}
        Expr::Name(n, _) => {
            // [U] 13.3: a bare name is a VARIABLE first and a scalar second. If
            // the layout is known the question is settled exactly; if it is not,
            // it may be either, and a may-set says both.
            match ctx.known_vars {
                Some(vars) => {
                    if vars.iter().any(|v| v.as_ref() == n.as_str()) {
                        into.reads.insert(n);
                    } else {
                        into.scalar_reads.insert(n);
                    }
                }
                None => {
                    into.reads.insert(n);
                    into.scalar_reads.insert(n);
                }
            }
        }
        Expr::Sys(v, _) => match v {
            // `_n` and `_N` make the answer depend on which observations exist
            // and where they are.
            SysVar::NLower | SysVar::NUpper => {
                into.order_sensitive = true;
            }
            SysVar::Pi | SysVar::Rc => {}
        },
        Expr::Index { base, idx, .. } => {
            // `x[_n-1]` is the canonical order-sensitive expression.
            into.order_sensitive = true;
            expr_effects(base, ctx, into);
            expr_effects(idx, ctx, into);
        }
        Expr::Unary { rhs, .. } => expr_effects(rhs, ctx, into),
        Expr::Binary { lhs, rhs, .. } => {
            expr_effects(lhs, ctx, into);
            expr_effects(rhs, ctx, into);
        }
        Expr::Paren(inner, _) => expr_effects(inner, ctx, into),
        Expr::Call { name, args, .. } => {
            // The function table already records which functions are not
            // deterministic; that bit IS the RNG effect, and reading it here
            // rather than keeping a second list is what keeps the two in step.
            if stratum_parse::function(name).is_some_and(|f| !f.deterministic) {
                into.rng = RngEffect::Consumes;
            }
            for a in args {
                expr_effects(a, ctx, into);
            }
        }
        Expr::Stored { class, key, .. } => {
            match class {
                StoredClass::R => into.rclass = join_read(into.rclass),
                StoredClass::E => into.estimates = join_read(into.estimates),
                StoredClass::S => {}
                StoredClass::C => {
                    // `c(k)`, `c(N)`, `c(linesize)` — a settings read when the
                    // key is a literal, and unresolvable when it is computed.
                    if let Expr::Name(n, _) = key.as_ref() {
                        let n: Name = n.as_str().into();
                        if !into.settings_read.contains(&n) {
                            into.settings_read.push(n);
                        }
                    } else {
                        into.confidence = Confidence::Speculative;
                    }
                }
            }
            expr_effects(key, ctx, into);
        }
        Expr::Coef { key, .. } => {
            into.estimates = join_read(into.estimates);
            expr_effects(key, ctx, into);
        }
        Expr::MatElem { name, i, j, .. } => {
            into.matrix_reads.insert(name);
            expr_effects(i, ctx, into);
            expr_effects(j, ctx, into);
        }
        Expr::Term(atom, _) => {
            let mut vs = VarSet::new();
            insert_pattern(&mut vs, &atom.base, ctx);
            into.reads.union(&vs);
            if atom.ts.is_some() {
                // A lag reads a neighbouring observation.
                into.order_sensitive = true;
            }
        }
        Expr::Hole { .. } => {
            // Rule 7, first bullet: an unexpanded macro where a name belongs.
            into.reads = VarSet::unknown();
            into.macro_reads = NameSet::unknown();
            into.scalar_reads = NameSet::unknown();
            into.taint |= Taint::MACRO_VARLIST;
            into.confidence = Confidence::Speculative;
        }
    }
}

fn join_read(a: RwEffect) -> RwEffect {
    match a {
        RwEffect::None => RwEffect::Read,
        RwEffect::Write | RwEffect::ReadWrite => RwEffect::ReadWrite,
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Varlists — rule 7's first bullet, and the narrowing `StaticCtx` exists for
// ---------------------------------------------------------------------------

/// A parsed varlist as a [`VarSet`], expanded exactly against a known layout
/// when there is one.
///
/// `StaticCtx::known_vars` exists precisely so `_all`, `inc*` and `a-z` become
/// the names they actually mean instead of "any variable". A block whose reads
/// are `{price, mpg}` rather than `unknown` is a block the staleness sweep can
/// leave `Current`; that is the difference between a 200 ms edit-run loop and
/// re-running everything.
pub fn varset(list: Option<&VarList>, ctx: &StaticCtx<'_>) -> VarSet {
    let Some(list) = list else {
        return VarSet::new();
    };
    if list.has_hole() {
        return VarSet::unknown();
    }
    let mut out = VarSet::new();
    for item in &list.items {
        let atoms = match &item.kind {
            VarItemKind::Single(a) => std::slice::from_ref(a),
            VarItemKind::Interact { atoms, .. } => atoms.as_slice(),
        };
        for a in atoms {
            insert_pattern(&mut out, &a.base, ctx);
            if out.unknown {
                return out;
            }
        }
    }
    out
}

/// A varlist atom that still carries a macro reference.
///
/// `ParseMode::Speculative` hands back `` `vars' `` as a `VarPattern::Name`
/// whose text is `` `vars' `` — a Hole is produced only where the grammar
/// expects an expression. Inserting that as a variable name would be design 03
/// §5.3 rule 7's exact failure: a block whose read set is the literal string
/// `` `vars' `` intersects nothing, so it stays Current after its real input
/// changed.
fn is_dynamic(n: &str) -> bool {
    n.contains('`') || n.contains('$')
}

/// True when a varlist cannot be resolved without expanding a macro first.
fn varlist_dynamic(list: Option<&VarList>) -> bool {
    let Some(list) = list else {
        return false;
    };
    if list.has_hole() {
        return true;
    }
    list.items.iter().any(|i| {
        let atoms = match &i.kind {
            VarItemKind::Single(a) => std::slice::from_ref(a),
            VarItemKind::Interact { atoms, .. } => atoms.as_slice(),
        };
        atoms.iter().any(|a| match &a.base {
            VarPattern::Name(n) => is_dynamic(n),
            VarPattern::Glob(g) | VarPattern::Tilde(g) => is_dynamic(g),
            VarPattern::Range { lo, hi } => is_dynamic(lo) || is_dynamic(hi),
            _ => false,
        })
    })
}

fn insert_pattern(out: &mut VarSet, p: &VarPattern, ctx: &StaticCtx<'_>) {
    match p {
        VarPattern::Name(n) if is_dynamic(n) => out.unknown = true,
        VarPattern::Name(n) => out.insert(n),
        VarPattern::Glob(g) | VarPattern::Tilde(g) if is_dynamic(g) => out.unknown = true,
        VarPattern::Glob(g) | VarPattern::Tilde(g) => {
            // `~` is `*` that must match exactly one variable ([U] 11.4.1). A
            // may-set that admits several is a superset of one, so the same
            // matcher answers both.
            let g = &g.replace('~', "*");
            match ctx.known_vars {
                Some(vars) => {
                    for v in vars {
                        if glob_match(g, v) {
                            out.insert(v);
                        }
                    }
                }
                // Without a layout the pattern still narrows: `inc*` cannot match a
                // name that does not start with `inc`.
                None => match split_glob(g) {
                    Some(Glob::Prefix(p)) => out.insert_pattern(SetPattern::Prefix(p.into())),
                    Some(Glob::Suffix(s)) => out.insert_pattern(SetPattern::Suffix(s.into())),
                    None => out.unknown = true,
                },
            }
        }
        VarPattern::All => match ctx.known_vars {
            Some(vars) => {
                for v in vars {
                    out.insert(v);
                }
            }
            None => out.insert_pattern(SetPattern::All),
        },
        VarPattern::Range { lo, hi } => match ctx.known_vars {
            // A range is a STORAGE-order slice, so it needs the order, which is
            // exactly what `known_vars` is.
            Some(vars) => {
                let a = vars.iter().position(|v| v.as_ref() == lo.as_str());
                let b = vars.iter().position(|v| v.as_ref() == hi.as_str());
                match (a, b) {
                    // A range whose endpoints are both in the layout, in order,
                    // is the only case that narrows. Every other one — an
                    // endpoint that is not a variable, or `hi` before `lo` —
                    // widens, because guessing which names lie between them is
                    // exactly the narrowing-on-a-guess rule 7 forbids.
                    (Some(a), Some(b)) if a <= b => {
                        for v in &vars[a..=b] {
                            out.insert(v);
                        }
                    }
                    _ => out.unknown = true,
                }
            }
            None => out.insert_pattern(SetPattern::Range(lo.as_str().into(), hi.as_str().into())),
        },
        // `str8 *` and `v:lblname` filter on storage type and value label, and
        // `known_vars` carries neither.
        VarPattern::Typed { .. } | VarPattern::Labeled { .. } | VarPattern::Hole { .. } => {
            out.unknown = true;
        }
    }
}

enum Glob {
    Prefix(String),
    Suffix(String),
}

/// The one-sided globs that narrow without a layout: `inc*` and `*inc`.
fn split_glob(g: &str) -> Option<Glob> {
    if g.contains('?') {
        return None;
    }
    let stem = g.strip_suffix('*');
    if let Some(stem) = stem {
        if !stem.contains('*') {
            return Some(Glob::Prefix(stem.to_owned()));
        }
    }
    if let Some(stem) = g.strip_prefix('*') {
        if !stem.contains('*') {
            return Some(Glob::Suffix(stem.to_owned()));
        }
    }
    None
}

/// Stata's varlist glob: `*` is any run, `?` is exactly one character.
///
/// Iterative with one backtrack point rather than recursive: a pathological
/// pattern like `a*a*a*a*b` against a long name is quadratic in the recursive
/// form and this runs over every variable in the dataset.
fn glob_match(pat: &str, name: &str) -> bool {
    let (p, n) = (pat.as_bytes(), name.as_bytes());
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut resume) = (usize::MAX, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = pi;
            pi += 1;
            resume = ni;
        } else if star != usize::MAX {
            pi = star + 1;
            resume += 1;
            ni = resume;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

// ---------------------------------------------------------------------------
// Slot readers
// ---------------------------------------------------------------------------

fn rest_text(s: &Slots) -> Option<&str> {
    s.rest.as_ref().map(|r| r.text.trim())
}

fn rest_first_word(s: &Slots) -> Option<&str> {
    rest_text(s)?.split_whitespace().next()
}

fn has_option(s: &Slots, canonical: &str) -> bool {
    s.options
        .items
        .iter()
        .any(|o| o.canonical == Some(canonical))
}

/// The `using` path, or the first word of `rest`, as a resolved literal.
///
/// A path built from a macro arrives as text containing a backtick or a `$`,
/// which is rule 7's "filename position" bullet: `FileSet::unknown`, never a
/// guess at what the macro held.
fn file_of(s: &Slots, ctx: &StaticCtx<'_>) -> FileSet {
    let raw = match &s.using {
        Some(f) => Some(f.raw.as_str()),
        None => rest_first_word(s),
    };
    let Some(raw) = raw else {
        return FileSet::unknown();
    };
    let raw = raw.trim_matches('"');
    if raw.is_empty() || raw.contains('`') || raw.contains('$') {
        return FileSet::unknown();
    }
    let mut out = FileSet::new();
    let p = Utf8PathBuf::from(raw);
    out.insert(if p.is_absolute() { p } else { ctx.cwd.join(p) });
    out
}

/// `rename old new` as a pair, or `None` for every form that needs the layout.
fn rename_pair(s: &Slots) -> Option<(Name, Name)> {
    // BOTH names land in the varlist slot: the parser sees `rename old new` as
    // a two-item varlist, and `rest` carries the same two words as raw text.
    // Reading the pair out of `rest` would work for `rename price cost` and
    // break the moment either name needed the varlist grammar.
    let list = s.varlist.as_ref()?;
    let [a, b] = list.items.as_slice() else {
        return None;
    };
    Some((plain_atom(a)?, plain_atom(b)?))
}

/// A varlist item that is exactly one literal name.
fn plain_atom(item: &VarItem) -> Option<Name> {
    let VarItemKind::Single(atom) = &item.kind else {
        return None;
    };
    let VarPattern::Name(n) = &atom.base else {
        return None;
    };
    // `rename (a b) (x y)` reaches here as two atoms whose NAMES are the
    // literal text `(a b)` and `(x y)`: a group rename needs the live layout to
    // pair up, so it is not a static rename at all.
    is_plain_name(n).then(|| n.as_str().into())
}

/// `scalar x = …` / `scalar define x = …` — the name being written.
fn scalar_target(s: &Slots) -> Option<String> {
    let rest = rest_text(s)?;
    let rest = rest.strip_prefix("define ").map_or(rest, str::trim_start);
    let name = rest.split(['=', ' ']).next()?.trim();
    is_plain_name(name).then(|| name.to_owned())
}

/// `program define p` / `program drop p` / `program p` — the name being written.
fn program_target(s: &Slots) -> Option<String> {
    let rest = rest_text(s)?;
    let mut words = rest.split_whitespace();
    let first = words.next()?;
    let name = match first {
        "define" | "drop" | "dir" | "list" => words.next()?,
        other => other,
    };
    is_plain_name(name).then(|| name.to_owned())
}

/// The variables a metadata command names, from its `rest` tail.
///
/// `label variable price "Price"`, `format price mpg %9.2f`. Anything that is
/// not a plain word list widens to `unknown`, because `label define` and
/// `label values` name a VALUE LABEL rather than a variable and confusing the
/// two would drop a real dependency.
fn rest_names(s: &Slots, ctx: &StaticCtx<'_>) -> VarSet {
    let mut out = varset(s.varlist.as_ref(), ctx);
    let Some(rest) = rest_text(s) else {
        return if out.is_empty() {
            VarSet::unknown()
        } else {
            out
        };
    };
    let mut words = rest.split_whitespace();
    let Some(head) = words.next() else {
        return if out.is_empty() {
            VarSet::unknown()
        } else {
            out
        };
    };
    match head {
        // `label variable x "…"` / `label values x lbl` — the next word is the
        // variable.
        "variable" | "values" | "var" | "val" => match words.next() {
            Some(w) if is_plain_name(w) => out.insert(w),
            _ => out.unknown = true,
        },
        // `label define`, `label dir`, `label list`, `label drop` name value
        // labels, not variables, and touch no column's values.
        "define" | "dir" | "list" | "drop" | "copy" | "save" | "language" => {}
        // `format price %9.2f` and `notes x: …`.
        w if is_plain_name(w.trim_end_matches(':')) => out.insert(w.trim_end_matches(':')),
        _ => out.unknown = true,
    }
    if out.is_empty() {
        return VarSet::new();
    }
    out
}

/// The variable(s) named inside an option argument — `by(foreign)`,
/// `generate(x)`, `vce(cluster rep78)`.
fn option_varset(s: &Slots, canonical: &str, ctx: &StaticCtx<'_>) -> VarSet {
    let mut out = VarSet::new();
    for item in &s.options.items {
        if item.canonical != Some(canonical) {
            continue;
        }
        let Some(arg) = &item.arg else { continue };
        match arg {
            OptionArg::VarList(vl) => out.union(&varset(Some(vl), ctx)),
            OptionArg::Str(t)
            | OptionArg::Raw(stratum_parse::ast::command::RawArgs { text: t, .. }) => {
                if t.contains('`') || t.contains('$') {
                    return VarSet::unknown();
                }
                // `vce(cluster rep78)`: the variable is the last word.
                match t.split_whitespace().next_back() {
                    Some(w) if is_plain_name(w) => out.insert(w),
                    _ => return VarSet::unknown(),
                }
            }
            _ => return VarSet::unknown(),
        }
    }
    out
}

/// Does `rest` contain a bare identifier outside string literals?
///
/// The cheapest sound separator between `display "done"` and `display price`.
/// Format directives (`%9.2f`) and style words in braces (`{txt}`) are not
/// identifiers; a name is.
fn rest_has_identifier(s: &Slots) -> bool {
    let Some(t) = rest_text(s) else { return false };
    let b = t.as_bytes();
    let mut i = 0usize;
    let mut in_str = false;
    let mut in_brace = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            in_str = c != b'"';
            i += 1;
            continue;
        }
        if in_brace {
            in_brace = c != b'}';
            i += 1;
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => in_brace = true,
            b'%' => {
                // Skip a format directive whole: `%9.2fc` must not read as the
                // identifier `fc`.
                i += 1;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                continue;
            }
            b'_' => return true,
            c if c.is_ascii_alphabetic() => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

fn is_plain_name(w: &str) -> bool {
    !w.is_empty()
        && w.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_hash::FxHashMap;
    use stratum_parse::{parse_command, ParseMode};

    fn empty() -> FxHashMap<Name, Name> {
        FxHashMap::default()
    }

    fn bare(m: &FxHashMap<Name, Name>) -> StaticCtx<'_> {
        StaticCtx::bare(camino::Utf8Path::new("/w"), m)
    }

    fn with_vars<'a>(m: &'a FxHashMap<Name, Name>, vars: &'a [Name]) -> StaticCtx<'a> {
        let mut c = StaticCtx::bare(camino::Utf8Path::new("/w"), m);
        c.known_vars = Some(vars);
        c
    }

    fn names(v: &[&str]) -> Vec<Name> {
        v.iter().map(|s| (*s).into()).collect()
    }

    fn eff(src: &str, ctx: &StaticCtx<'_>) -> EffectSet {
        let (ast, _) = parse_command(src, ParseMode::Execute);
        BuiltinEffects.effects(&ast, ctx)
    }

    fn eff_spec(src: &str, ctx: &StaticCtx<'_>) -> EffectSet {
        let (ast, _) = parse_command(src, ParseMode::Speculative);
        BuiltinEffects.effects(&ast, ctx)
    }

    fn named(s: &VarSet) -> Vec<&str> {
        s.named.iter().map(AsRef::as_ref).collect()
    }

    #[test]
    fn every_command_in_the_table_has_a_row_somewhere() {
        // THE anti-drift test. A command added to `data/commands.ron` with no
        // effect row does not fail loudly: it runs, the table answers "unknown",
        // every block downstream of it goes permanently stale, and nothing is
        // red. This is what makes that loud.
        let missing: Vec<&str> = stratum_parse::all_commands()
            .iter()
            .map(|c| c.canonical)
            .filter(|n| shape(n).is_none() && !OWNED_BY_STATS.contains(n))
            .collect();
        assert!(
            missing.is_empty(),
            "commands with no EffectTable row in this crate and none in stratum-stats: {missing:?}"
        );
    }

    #[test]
    fn the_rows_stratum_stats_owns_are_refused_here() {
        // A1: two tables answering for one command is two answers, and the
        // wrong one wins by chaining order rather than by being right.
        for n in OWNED_BY_STATS {
            assert!(
                !BuiltinEffects.is_known_command(n),
                "{n} belongs to stratum-stats"
            );
        }
        assert!(BuiltinEffects.is_known_command("generate"));
    }

    #[test]
    fn an_unrecognized_command_is_unknown_all_and_not_empty() {
        let m = empty();
        let e = eff("frobnicate price", &bare(&m));
        assert!(e.reads.unknown, "an unknown command may read anything");
        assert!(e.writes.unknown);
        assert_eq!(e.atomicity, Atomicity::External);
        assert!(e.taint.contains(Taint::UNKNOWN_COMMAND));
    }

    #[test]
    fn generate_creates_its_target_and_reads_its_expression() {
        let m = empty();
        let vars = names(&["price", "mpg"]);
        let e = eff("generate lnp = log(price)", &with_vars(&m, &vars));
        assert_eq!(named(&e.creates), vec!["lnp"]);
        assert_eq!(named(&e.reads), vec!["price"]);
        // With the layout known, `price` is provably a variable and not a
        // scalar — the whole point of `StaticCtx::known_vars`.
        assert!(e.scalar_reads.is_empty());
        assert_eq!(e.rng, RngEffect::None);
    }

    #[test]
    fn without_a_layout_a_bare_name_may_be_either_a_variable_or_a_scalar() {
        let m = empty();
        let e = eff("generate lnp = log(price)", &bare(&m));
        assert_eq!(named(&e.reads), vec!["price"]);
        assert_eq!(
            e.scalar_reads
                .names
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<&str>>(),
            vec!["price"]
        );
    }

    #[test]
    fn replace_writes_rather_than_creates() {
        let m = empty();
        let vars = names(&["x"]);
        let e = eff("replace x = x + 1", &with_vars(&m, &vars));
        assert_eq!(named(&e.writes), vec!["x"]);
        assert!(e.creates.is_empty());
        assert_eq!(named(&e.reads), vec!["x"]);
    }

    #[test]
    fn an_unexpanded_macro_in_a_varlist_widens_to_unknown() {
        // Rule 7, first bullet. Guessing what `` `vars' `` held is how a block
        // stays Current after its real input changed.
        let m = empty();
        let e = eff_spec("list `vars'", &bare(&m));
        assert!(e.reads.unknown);
        assert!(e.taint.contains(Taint::MACRO_VARLIST));
        assert_eq!(e.confidence, Confidence::Speculative);
    }

    #[test]
    fn a_glob_expands_exactly_against_a_known_layout() {
        let m = empty();
        let vars = names(&["income", "incorp", "price"]);
        let e = eff("list inc*", &with_vars(&m, &vars));
        // `VarSet::named` is sorted, not in storage order: `may_intersect` is a
        // sorted-slice walk, and that is the operation the staleness sweep runs
        // millions of times.
        assert_eq!(named(&e.reads), vec!["income", "incorp"]);
        assert!(!e.reads.unknown, "a known layout resolves the glob");
    }

    #[test]
    fn a_glob_without_a_layout_still_narrows_to_a_prefix() {
        let m = empty();
        let e = eff("list inc*", &bare(&m));
        assert!(!e.reads.unknown);
        assert_eq!(e.reads.patterns.len(), 1);
        // The narrowing is real: a set of `inc*` cannot intersect `{price}`.
        let mut other = VarSet::new();
        other.insert("price");
        assert!(!e.reads.may_intersect(&other));
    }

    #[test]
    fn a_positional_range_needs_the_storage_order_and_says_so_without_it() {
        let m = empty();
        let vars = names(&["a", "b", "c", "d"]);
        let e = eff("list b-c", &with_vars(&m, &vars));
        assert_eq!(named(&e.reads), vec!["b", "c"]);

        let e = eff("list b-c", &bare(&m));
        // No layout: the range stays a pattern, and `VarSet` treats a range as
        // overlapping everything, which is the sound answer.
        let mut other = VarSet::new();
        other.insert("zzz");
        assert!(e.reads.may_intersect(&other));
    }

    #[test]
    fn display_of_a_literal_depends_on_nothing() {
        let m = empty();
        let e = eff(r#"display "done""#, &bare(&m));
        assert!(e.reads.is_empty(), "a string literal reads no data");
        assert_eq!(e.rclass, RwEffect::None);
    }

    #[test]
    fn display_of_a_name_depends_on_the_dataset() {
        let m = empty();
        let e = eff("display price[1]", &bare(&m));
        assert!(e.reads.unknown);
    }

    #[test]
    fn a_display_format_is_not_an_identifier() {
        // `display %9.2f 1/3` must not read as the identifier `f`.
        let m = empty();
        let e = eff("display %9.2f 1/3", &bare(&m));
        assert!(e.reads.is_empty());
    }

    #[test]
    fn the_by_prefix_reads_its_grouping_variables() {
        // Rule 2. A caller that goes straight to the table rather than through
        // the driver must still see `foreign`.
        let m = empty();
        let e = eff("by foreign: count", &bare(&m));
        assert_eq!(named(&e.reads), vec!["foreign"]);
        assert!(e.order_sensitive);
    }

    #[test]
    fn bysort_also_reorders_the_observations() {
        let m = empty();
        let e = eff("bysort foreign: count", &bare(&m));
        assert_eq!(e.row_order, Tri::Yes);
    }

    #[test]
    fn a_random_generator_marks_the_stream_consumed() {
        // The bit comes off the function table's `deterministic` column, so a
        // generator added there is covered without editing this file.
        let m = empty();
        let e = eff("generate u = runiform()", &bare(&m));
        assert_eq!(e.rng, RngEffect::Consumes);
    }

    #[test]
    fn a_subscript_makes_the_answer_order_sensitive() {
        let m = empty();
        let e = eff("generate lag = x[_n-1]", &bare(&m));
        assert!(e.order_sensitive, "x[_n-1] depends on which row is where");
    }

    #[test]
    fn use_replaces_the_frame_and_records_the_file() {
        let m = empty();
        let e = eff("use auto.dta, clear", &bare(&m));
        assert_eq!(e.frame, FrameEffect::ReplaceCurrent);
        assert_eq!(e.row_membership, Tri::Yes);
        assert_eq!(e.file_reads.paths.len(), 1);
        // As a PATH, not a string: `file_of` anchors a bare `auto.dta` with
        // `Utf8Path::join`, which spells the separator the host's way — `/` here
        // and `\` on Windows. Which slash `join` picked is not the property
        // under test; that the file was anchored at `ctx.cwd` is. camino
        // compares component-wise, so both spellings satisfy this.
        assert_eq!(e.file_reads.paths[0], Utf8PathBuf::from("/w/auto.dta"));
        assert!(!e.file_reads.unknown);
    }

    #[test]
    fn a_relative_filename_is_anchored_at_the_ctx_cwd_whatever_the_separator() {
        // `file_of` resolves the same file on every host; only the SPELLING
        // differs, because `Utf8Path::join` writes the host's separator. macOS
        // cannot make `join` emit a `\`, so what this pins instead are the
        // facts that hold on both: the anchor is `ctx.cwd` and never the
        // process cwd, the leaf survives, and a multi-component relative path
        // keeps its interior structure.
        let m = empty();
        let e = eff("use data/auto.dta, clear", &bare(&m));
        let p = &e.file_reads.paths[0];
        assert!(p.starts_with("/w"), "anchored at ctx.cwd, got {p}");
        assert_eq!(p.file_name(), Some("auto.dta"));
        assert_eq!(p, &Utf8PathBuf::from("/w/data/auto.dta"));
    }

    #[test]
    fn a_macro_built_filename_is_never_guessed_at() {
        let m = empty();
        let e = eff_spec("use `f', clear", &bare(&m));
        assert!(e.file_reads.unknown);
    }

    #[test]
    fn save_leaves_the_engine_and_says_so() {
        let m = empty();
        let e = eff("save out.dta, replace", &bare(&m));
        assert_eq!(e.atomicity, Atomicity::External);
        // A path compare: the host's `join` separator is not the property here
        // (see `use_replaces_the_frame_and_records_the_file`).
        assert_eq!(e.file_writes.paths[0], Utf8PathBuf::from("/w/out.dta"));
    }

    #[test]
    fn sort_changes_the_order_and_nothing_else() {
        let m = empty();
        let e = eff("sort price", &bare(&m));
        assert_eq!(e.row_order, Tri::Yes);
        assert_eq!(e.row_membership, Tri::No);
        assert!(e.writes.is_empty(), "sort does not change a value");
    }

    #[test]
    fn rename_records_the_pair_rather_than_a_drop_and_a_create() {
        // Design 03 §4.3: identity survives a rename. Encoding it as
        // drop+create would lose the `VarId` and make every downstream block
        // Broken instead of one of them.
        let m = empty();
        let e = eff("rename price cost", &bare(&m));
        assert_eq!(e.renames.len(), 1);
        assert_eq!(e.renames[0].0.as_ref(), "price");
        assert_eq!(e.renames[0].1.as_ref(), "cost");
        assert!(e.drops.is_empty());
        assert!(e.creates.is_empty());
    }

    #[test]
    fn a_group_rename_cannot_be_paired_statically() {
        let m = empty();
        let e = eff("rename (a b) (x y)", &bare(&m));
        assert!(e.drops.unknown || e.creates.unknown);
    }

    #[test]
    fn drop_with_a_qualifier_changes_which_rows_exist() {
        let m = empty();
        let e = eff("drop if price > 10000", &bare(&m));
        assert_eq!(e.row_membership, Tri::Yes);
        assert_eq!(named(&e.reads), vec!["price"]);
    }

    #[test]
    fn keep_of_a_varlist_drops_everything_it_does_not_name() {
        let m = empty();
        let e = eff("keep price mpg", &bare(&m));
        assert!(e.drops.unknown, "which variables go is a runtime fact");
    }

    #[test]
    fn local_writes_the_name_it_defines() {
        let m = empty();
        let e = eff("local vars price mpg", &bare(&m));
        assert_eq!(
            e.macro_writes
                .names
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<&str>>(),
            vec!["vars"]
        );
    }

    #[test]
    fn set_records_which_setting_it_writes() {
        let m = empty();
        let e = eff("set varabbrev off", &bare(&m));
        assert_eq!(e.settings_write.len(), 1);
        assert_eq!(e.settings_write[0].as_ref(), "varabbrev");
    }

    #[test]
    fn do_of_another_file_is_the_conservative_floor() {
        let m = empty();
        let e = eff("do setup.do", &bare(&m));
        assert!(e.reads.unknown && e.writes.unknown);
        assert!(e.taint.contains(Taint::FILE_DYNAMIC));
        // A path compare: the host's `join` separator is not the property here
        // (see `use_replaces_the_frame_and_records_the_file`).
        assert_eq!(e.file_reads.paths[0], Utf8PathBuf::from("/w/setup.do"));
    }

    #[test]
    fn python_is_outside_the_engine() {
        let m = empty();
        let e = eff("python: print(1)", &bare(&m));
        assert!(e.taint.contains(Taint::EXTERNAL));
        assert_eq!(e.atomicity, Atomicity::External);
    }

    #[test]
    fn an_estimation_command_writes_the_estimates() {
        let m = empty();
        let e = eff("logit foreign price mpg", &bare(&m));
        assert_eq!(e.estimates, RwEffect::Write);
        assert_eq!(named(&e.reads), vec!["foreign", "mpg", "price"]);
    }

    #[test]
    fn a_stored_result_reference_reads_that_namespace() {
        let m = empty();
        let e = eff("generate z = r(mean)", &bare(&m));
        assert_eq!(e.rclass, RwEffect::Read);
    }

    #[test]
    fn a_c_reference_records_the_setting_it_read() {
        let m = empty();
        let e = eff("generate z = c(N)", &bare(&m));
        assert_eq!(e.settings_read.len(), 1);
        assert_eq!(e.settings_read[0].as_ref(), "N");
    }

    #[test]
    fn a_chain_asks_the_first_table_that_knows_the_command() {
        // The composition `stratum-exec` needs: neither crate may depend on the
        // other, so chaining is the only place the two tables meet.
        struct Fake;
        impl EffectTable for Fake {
            fn effects(&self, _c: &CommandAst, _x: &StaticCtx<'_>) -> EffectSet {
                let mut e = EffectSet::new();
                e.reads.insert("marker");
                e
            }
            fn is_known_command(&self, name: &str) -> bool {
                name == "summarize"
            }
        }
        let m = empty();
        let ctx = bare(&m);
        let fake = Fake;
        let builtin = BuiltinEffects;
        let tables: [&dyn EffectTable; 2] = [&builtin, &fake];
        let chain = Chain::new(&tables);
        let (ast, _) = parse_command("summarize price", ParseMode::Execute);
        assert_eq!(named(&chain.effects(&ast, &ctx).reads), vec!["marker"]);
        let (ast, _) = parse_command("count", ParseMode::Execute);
        assert_eq!(chain.effects(&ast, &ctx).rclass, RwEffect::Write);
        assert!(chain.is_known_command("summarize"));
        assert!(chain.is_known_command("count"));
        assert!(!chain.is_known_command("frobnicate"));
    }

    #[test]
    fn the_glob_matcher_agrees_with_stata_on_the_awkward_ones() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("inc*", "income"));
        assert!(!glob_match("inc*", "price"));
        assert!(glob_match("*e", "price"));
        assert!(glob_match("p?ice", "price"));
        assert!(!glob_match("p?ice", "prrice"));
        assert!(glob_match("a*a*b", "aXaYb"));
        assert!(!glob_match("a*a*b", "aXY"));
    }
}
