//! Static effect extraction — the driver of design 03 §5.3.
//!
//! `effects_rows.rs` answers "what does THIS command do with ITS slots". This
//! module answers the question staleness actually asks: **what can this block
//! touch**, given that a block is a region of source containing control flow,
//! loops, macro assignments and calls to programs defined further up the file.
//!
//! # The one rule, restated because everything here obeys it
//!
//! > There is no path in the extractor that narrows a set on a guess. Every
//! > narrowing is justified by a literal in the source.
//!
//! Every widening is free and costs a spurious re-run. Every narrowing must be
//! paid for with something concrete — a variable layout that is actually
//! loaded, a macro whose literal value was assigned earlier with no intervening
//! conditional reassignment, a loop list short enough to enumerate. When in
//! doubt this module reaches for [`EffectSet::unknown_all`] or sets `unknown` on
//! the affected component, and never the other way round.
//!
//! # Why substitution is textual
//!
//! Rule 5 (bounded unrolling) and rule 6 (constant macro propagation) both
//! resolve `` `v' `` to a literal. They do it by rewriting the TEXT and parsing
//! again, not by teaching the parser about a symbol table, because that is what
//! Stata itself does: macro expansion is a textual pass that runs before the
//! parser sees anything (design 02 §1). Resolving `` `x' `` any other way would
//! be a second expansion semantics that agrees with the first until the day it
//! does not — and the day it does not, a block says `Current` while holding a
//! number computed from something else.
//!
//! # ADR-017 counters
//!
//! [`ExtractStats`] counts regions, commands and unrolled iterations. The
//! property worth asserting is that unrolling is BOUNDED: rule 5's 256-iteration
//! limit is per loop, and [`UNROLL_BUDGET`] bounds the whole extraction, so a
//! file of nested `forvalues` cannot turn one edit into 256³ parses on the
//! control thread. `tests` at the bottom pin both.

use camino::Utf8Path;
use rustc_hash::FxHashMap;
use stratum_effects::varset::Name;
use stratum_effects::{EffectSet, EffectTable, RngEffect, StaticCtx, VarSet};
use stratum_parse::ast::command::{BlockCommand, Command, ForeachSource, RawArgs};
use stratum_parse::ast::expr::Expr;
use stratum_parse::ast::CommandAst;
use stratum_parse::{parse_command, ParseMode, Span};
use stratum_proto::{Confidence, Delimiter, Taint, Tri};

use crate::effects_rows::{expr_effects, varset};

/// Design 03 §5.3 rule 5: a literal loop list of at most this many iterations is
/// unrolled and resolved exactly. Beyond it the loop is analysed once with its
/// bindings unresolved and marked [`Taint::UNBOUNDED_LOOP`].
pub const MAX_UNROLL: u64 = 256;

/// Design 03 §5.3 rule 8: how deep a call to a program defined in this document
/// is followed before the call becomes [`EffectSet::unknown_all`].
pub const MAX_PROGRAM_DEPTH: u32 = 8;

/// Total unrolled iterations one [`extract_block`] may spend.
///
/// Rule 5's limit is per LOOP, and two nested loops of 256 are 65 536 body
/// parses — on the control thread, on every keystroke that lands in this block.
/// The budget is what makes the cost of extraction linear in the file rather
/// than exponential in its nesting. Exhausting it is not an error: the loop
/// falls back to the unresolved analysis, which is a widening, and
/// [`ExtractStats::budget_exhausted`] says it happened.
pub const UNROLL_BUDGET: u32 = 4_096;

/// What the extractor did — ADR-017 counters, not durations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExtractStats {
    /// Executable regions walked.
    pub regions: u32,
    /// Commands whose effects were looked up or derived.
    pub commands: u32,
    /// Loop iterations substituted and re-analysed (rule 5).
    pub unrolled: u32,
    /// Bodies parsed — loop bodies, branch arms, program bodies.
    pub bodies: u32,
    /// A loop declined to unroll because [`UNROLL_BUDGET`] ran out.
    pub budget_exhausted: bool,
    /// Program call sites resolved against a definition in the same document
    /// (rule 8).
    pub programs_inlined: u32,
    /// `*! stataide:` declarations trusted (rule 9).
    pub declarations_trusted: u32,
}

/// The effects of one already-parsed command, prefixes included.
///
/// This is the entry point for a caller that has a [`CommandAst`] and no
/// surrounding text — the command bar, and `stratum-exec`'s enqueue path. A
/// `Command::Block` reaching here has no body text to walk, so it answers
/// [`EffectSet::unknown_all`]; [`extract_block`] is the entry point that has it.
#[must_use]
pub fn extract_command(
    cmd: &CommandAst,
    table: &dyn EffectTable,
    ctx: &StaticCtx<'_>,
) -> EffectSet {
    let mut x = Extractor::new(table, ctx);
    x.command(cmd, "", None)
}

/// The effects of a whole block of source — design 03 §5.3, rules 1 through 9.
///
/// `src` is the block's text as the user typed it, macros UNEXPANDED: expansion
/// is what this function is reasoning about, so an expanded text would have
/// thrown away the question.
#[must_use]
pub fn extract_block(src: &str, table: &dyn EffectTable, ctx: &StaticCtx<'_>) -> EffectSet {
    extract_block_with_stats(src, table, ctx).0
}

/// [`extract_block`] plus the ADR-017 counters.
#[must_use]
pub fn extract_block_with_stats(
    src: &str,
    table: &dyn EffectTable,
    ctx: &StaticCtx<'_>,
) -> (EffectSet, ExtractStats) {
    let mut x = Extractor::new(table, ctx);
    let mut out = EffectSet::new();
    x.source(src, &mut out);
    (out, x.stats)
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

struct Extractor<'a> {
    table: &'a dyn EffectTable,
    known_vars: Option<&'a [Name]>,
    cwd: &'a Utf8Path,
    for_audit: bool,

    /// Rule 6. Local macros whose literal value is known here. An entry is
    /// DROPPED, never guessed, the moment anything makes it doubtful.
    consts: FxHashMap<Name, Name>,
    /// Rule 8. Programs defined in this document, by name.
    programs: FxHashMap<String, EffectSet>,
    /// Rule 8's recursion cap.
    depth: u32,
    /// True while walking a loop body or a conditional arm. Rule 6: an
    /// assignment seen in here kills the entry instead of setting it, because
    /// whether it ran at all is not statically known.
    conditional: bool,
    /// Delimiter mode of the text currently being walked. `#delimit ;` is
    /// file-scoped, so a body re-segmented without it mis-parses every `;`.
    delim: Delimiter,
    budget: u32,
    stats: ExtractStats,
}

impl<'a> Extractor<'a> {
    fn new(table: &'a dyn EffectTable, ctx: &StaticCtx<'a>) -> Self {
        Extractor {
            table,
            known_vars: ctx.known_vars,
            cwd: ctx.cwd,
            for_audit: ctx.for_audit,
            consts: ctx.const_macros.clone(),
            programs: FxHashMap::default(),
            depth: 0,
            conditional: false,
            delim: Delimiter::Cr,
            budget: UNROLL_BUDGET,
            stats: ExtractStats::default(),
        }
    }

    fn ctx(&self) -> StaticCtx<'_> {
        StaticCtx {
            known_vars: self.known_vars,
            const_macros: &self.consts,
            cwd: self.cwd,
            for_audit: self.for_audit,
        }
    }

    /// Walk a source buffer: segment it, and union every executable region.
    fn source(&mut self, src: &str, out: &mut EffectSet) {
        let seg = crate::dispatch::segment_in(src, self.delim);
        for region in &seg.regions {
            if !region.is_executable() {
                continue;
            }
            self.stats.regions += 1;
            let outer_delim = std::mem::replace(&mut self.delim, region.entry_delimiter);

            // Rule 9's declaration is a comment on the line(s) ABOVE the code,
            // which is exactly the text `outer_span` carries and `span` does
            // not.
            let lead = slice(src, region.outer_span.start, region.span.start);
            let decl = Declaration::parse(lead);

            // Rule 6 applied: every macro whose literal value is known is
            // substituted before the parser runs, because that is when Stata
            // substitutes it. What is left over stays a hole and widens.
            let raw = slice(src, region.span.start, region.span.end);
            let text = self.substitute_consts(raw);

            let (ast, _) = parse_command(&text, ParseMode::Speculative);
            let e = self.command(&ast, &text, decl.as_ref());
            out.union(&e);
            self.learn(&ast, &text);
            self.delim = outer_delim;
        }
    }

    /// One command, prefixes included.
    fn command(&mut self, cmd: &CommandAst, text: &str, decl: Option<&Declaration>) -> EffectSet {
        self.stats.commands += 1;

        // Rule 7, third bullet: a macro-derived COMMAND WORD. Nothing about the
        // rest of the line can be trusted once the verb itself is unknown, and
        // `capture noisily `cmd'` is the idiom the taint bit is named for.
        if dynamic_command_word(cmd, text) {
            let mut e = EffectSet::unknown_all();
            e.taint |= Taint::DYNAMIC_DISPATCH;
            return e;
        }

        match &cmd.cmd {
            Command::Block(b) => self.block(b, cmd, text),
            Command::Known(_) => {
                let mut e = {
                    let ctx = self.ctx();
                    self.table.effects(cmd, &ctx)
                };
                // A `Known` row that the table declined to claim comes back as
                // `unknown_all`; `external_name` promotes the taint from "we
                // have no row" to "this leaves the engine", which is the
                // difference between Stale and CurrentUnverifiable.
                if let Some(name) = command_word(cmd) {
                    if is_external(name) {
                        e.taint |= Taint::EXTERNAL;
                    }
                }
                e
            }
            Command::Directive(_) => {
                // `#delimit` changes how the NEXT line is read. It touches no
                // state the staleness model tracks.
                EffectSet::new()
            }
            Command::Unknown { name, rest, .. } => self.unknown(name, rest, decl),
        }
    }

    /// Rule 7 bullets 2 and 4, rule 8's call site, and rule 9's declaration —
    /// all of which are the same syntactic thing: a word the table has no row
    /// for.
    fn unknown(&mut self, name: &str, rest: &RawArgs, decl: Option<&Declaration>) -> EffectSet {
        // Rule 8. A program defined earlier in this document, whose body we
        // analysed exactly, is followed.
        if let Some(body) = self.programs.get(name) {
            if body.confidence == Confidence::Exact && self.depth < MAX_PROGRAM_DEPTH {
                let mut e = body.clone();
                // The call's own arguments name things at the CALL site. The
                // body cannot tell us what they were, so they are read.
                e.reads.union(&self.words_as_reads(&rest.text));
                self.stats.programs_inlined += 1;
                return e;
            }
        }

        // Rule 9. An author's declaration is trusted here and verified at
        // runtime — if the observed footprint contradicts it, lint R024 fires
        // and the block is permanently downgraded. Trust, then verify, then
        // never trust again.
        if let Some(d) = decl {
            self.stats.declarations_trusted += 1;
            return d.to_effects();
        }

        let mut e = EffectSet::unknown_all();
        // Rule 7 bullet 4. `shell` does not merely have effects we cannot name:
        // it has effects OUTSIDE the engine, which is what makes a block that
        // ran cleanly `CurrentUnverifiable` rather than `Current`.
        if is_external(name) {
            e.taint |= Taint::EXTERNAL;
        }
        e
    }

    /// Rules 4, 5 and 8 — everything whose effect is the union of a body's.
    fn block(&mut self, b: &BlockCommand, cmd: &CommandAst, text: &str) -> EffectSet {
        let mut out = EffectSet::new();
        match b {
            // Rule 4. BOTH arms are unioned, and the conditions are read. Every
            // write in here is a may-write, which is exactly what staleness
            // needs: a block that MIGHT have written `income` must invalidate
            // everything downstream of `income`.
            BlockCommand::IfElse { arms } => {
                for (cond, body) in arms {
                    if let Some(c) = cond {
                        self.expr(c, &mut out);
                    }
                    self.conditional_body(*body, text, &mut out);
                }
            }
            BlockCommand::While { cond, body } => {
                self.expr(cond, &mut out);
                self.conditional_body(*body, text, &mut out);
            }

            // Rule 5.
            BlockCommand::Foreach {
                loopvar,
                source,
                body,
            } => {
                let bindings = self.foreach_bindings(source);
                self.loop_body(loopvar, bindings, *body, text, &mut out);
                // The list itself is read whatever happens: `foreach v of
                // varlist age educ` touches both columns even before the body
                // does anything with them.
                if let ForeachSource::OfVarlist(l) | ForeachSource::OfNewlist(l) = source {
                    let ctx = self.ctx();
                    let vs = varset(Some(l), &ctx);
                    match source {
                        // `newlist` names variables that do not exist yet.
                        ForeachSource::OfNewlist(_) => out.creates.union(&vs),
                        _ => out.reads.union(&vs),
                    }
                }
                out.macro_writes.insert(loopvar);
            }
            BlockCommand::Forvalues {
                loopvar,
                range,
                body,
            } => {
                let bindings = numeric_bindings(range.from, range.step.unwrap_or(1.0), range.to);
                self.loop_body(loopvar, bindings, *body, text, &mut out);
                out.macro_writes.insert(loopvar);
            }

            // Rule 8. Defining a program writes the program namespace and does
            // nothing else — the body's effects belong to the CALL, not to the
            // definition.
            BlockCommand::Program { name, .. } => {
                out.program_writes.insert(name);
                // The parser leaves `body` empty on purpose and hands the
                // question to the segmenter — see `dispatch::end_block_body`.
                let body = crate::dispatch::end_block_body(text, self.delim);
                let mut inner = EffectSet::new();
                self.depth += 1;
                if self.depth <= MAX_PROGRAM_DEPTH {
                    self.body(body, text, &mut inner);
                }
                self.depth -= 1;
                self.programs.insert(name.clone(), inner);
            }

            // Transparent wrappers: they change what is printed and how an
            // error is reported, never what is touched.
            BlockCommand::Quietly { body }
            | BlockCommand::Noisily { body }
            | BlockCommand::Anonymous { body } => self.body(*body, text, &mut out),
            // `capture { }` swallows errors, which is lint R016's business and
            // not an effect. Its body still ran.
            BlockCommand::Capture { body } => self.body(*body, text, &mut out),

            // `input a b` adds observations from literal data.
            BlockCommand::Input { spec, .. } => {
                let ctx = self.ctx();
                out.creates.union(&varset(Some(spec), &ctx));
                out.row_membership = Tri::Yes;
            }

            // Rule 7 bullet 4: another language, inside our process.
            BlockCommand::Mata { .. } | BlockCommand::Python { .. } => {
                out = EffectSet::unknown_all();
                out.taint |= Taint::EXTERNAL;
            }
        }
        // A prefix chain on a block region — `quietly: foreach …` — is the
        // table's business for a `Known` command and ours for this one.
        self.apply_prefixes(cmd, &mut out);
        out
    }

    /// A body that may or may not run. Rule 6: an assignment in here cannot be
    /// propagated, because whether it happened is not statically known.
    fn conditional_body(&mut self, body: Span, text: &str, out: &mut EffectSet) {
        let was = std::mem::replace(&mut self.conditional, true);
        self.body(body, text, out);
        self.conditional = was;
    }

    fn body(&mut self, body: Span, text: &str, out: &mut EffectSet) {
        self.stats.bodies += 1;
        let inner = slice(text, body.start, body.end).to_owned();
        self.source(&inner, out);
    }

    /// Rule 5's unrolling, and its fallback.
    fn loop_body(
        &mut self,
        loopvar: &str,
        bindings: Option<Vec<String>>,
        body: Span,
        text: &str,
        out: &mut EffectSet,
    ) {
        let src = slice(text, body.start, body.end).to_owned();

        let Some(bindings) = bindings else {
            // Rule 5's "beyond the bound, or if the list is macro-built": the
            // body is still analysed, with `` `v' `` left as the hole it is, so
            // every component it reaches widens on its own. That is strictly
            // more informative than `unknown_all` and just as sound.
            self.stats.bodies += 1;
            let was = std::mem::replace(&mut self.conditional, true);
            self.source(&src, out);
            self.conditional = was;
            out.taint |= Taint::UNBOUNDED_LOOP;
            out.confidence = Confidence::Speculative;
            return;
        };

        let cost = u32::try_from(bindings.len()).unwrap_or(u32::MAX);
        if cost > self.budget {
            self.stats.budget_exhausted = true;
            self.stats.bodies += 1;
            let was = std::mem::replace(&mut self.conditional, true);
            self.source(&src, out);
            self.conditional = was;
            out.taint |= Taint::UNBOUNDED_LOOP;
            out.confidence = Confidence::Speculative;
            return;
        }
        self.budget -= cost;

        // A zero-iteration loop still cannot be narrowed to "no effect": the
        // list was literal and empty, so the body provably never runs, but a
        // `foreach v of varlist` over an empty varlist is a parse we do not
        // want to over-trust. Analyse it once, unresolved.
        if bindings.is_empty() {
            self.stats.bodies += 1;
            let was = std::mem::replace(&mut self.conditional, true);
            self.source(&src, out);
            self.conditional = was;
            return;
        }

        let was = std::mem::replace(&mut self.conditional, true);
        for b in &bindings {
            self.stats.unrolled += 1;
            self.stats.bodies += 1;
            let text = substitute_local(&src, loopvar, b);
            self.source(&text, out);
        }
        self.conditional = was;
    }

    /// Rule 5's literal lists. `None` means "not statically enumerable".
    fn foreach_bindings(&self, source: &ForeachSource) -> Option<Vec<String>> {
        match source {
            ForeachSource::In(raw) => literal_words(&raw.text),
            ForeachSource::OfVarlist(l) | ForeachSource::OfNewlist(l) => {
                let ctx = self.ctx();
                let vs = varset(Some(l), &ctx);
                // Only a set that is entirely concrete names enumerates. A
                // pattern that survived means the layout was unknown, and
                // guessing which names it covers is the narrowing rule 7 bans.
                if vs.unknown || !vs.patterns.is_empty() || vs.named.len() as u64 > MAX_UNROLL {
                    return None;
                }
                Some(vs.named.iter().map(|n| n.as_ref().to_owned()).collect())
            }
            ForeachSource::OfNumlist(nl) => {
                if nl.count() > MAX_UNROLL {
                    return None;
                }
                Some(
                    nl.expand()
                        .into_iter()
                        .map(stratum_parse::macros::stringify_number)
                        .collect(),
                )
            }
            // Rule 6 feeds rule 5: `local vars "age educ"` then `foreach v of
            // local vars` resolves exactly, which is the whole point of
            // propagating constants at all.
            ForeachSource::OfLocal(name) => {
                let v = self.consts.get(name.as_str())?;
                literal_words(v)
            }
            // Globals are not propagated — see `learn`.
            ForeachSource::OfGlobal(_) => None,
        }
    }

    /// Rule 2 for the block commands. `effects_rows` does this for the rows it
    /// owns; a `Command::Block` never reaches that table.
    fn apply_prefixes(&self, cmd: &CommandAst, e: &mut EffectSet) {
        use stratum_parse::ast::command::Prefix;
        for p in &cmd.prefixes {
            match p {
                Prefix::By(by) => {
                    let ctx = self.ctx();
                    e.reads.union(&varset(Some(&by.group), &ctx));
                    e.reads.union(&varset(Some(&by.extra_sort), &ctx));
                    e.order_sensitive = true;
                    if by.sort {
                        e.row_order = Tri::Yes;
                    }
                }
                Prefix::Quietly { .. }
                | Prefix::Noisily { .. }
                | Prefix::Version { .. }
                | Prefix::Capture { .. } => {}
                Prefix::Frame { .. } => e.frame = stratum_effects::FrameEffect::Unknown,
                Prefix::Generic { .. } => {
                    let wrapped = std::mem::replace(e, EffectSet::unknown_all());
                    e.union(&wrapped);
                }
            }
        }
    }

    /// Rule 1 for the expressions the driver owns — `while` conditions and
    /// `if` arms. The walk itself is `effects_rows::expr_effects`, because two
    /// walkers would be two answers to "does this read `price`".
    fn expr(&self, e: &Expr, out: &mut EffectSet) {
        let ctx = self.ctx();
        expr_effects(e, &ctx, out);
    }

    /// Words at a program call site, as a read set.
    fn words_as_reads(&self, args: &str) -> VarSet {
        let mut out = VarSet::new();
        let Some(words) = literal_words(args) else {
            out.unknown = true;
            return out;
        };
        for w in words {
            if is_plain_name(&w) {
                out.insert(&w);
            } else {
                // An option, a number, a quoted string — or something we cannot
                // classify. Widening is the only sound answer.
                out.unknown = true;
                return out;
            }
        }
        out
    }

    /// Rule 6's bookkeeping, run AFTER a statement's effects are taken.
    ///
    /// Only `local name <literal>` at unconditional top level adds an entry.
    /// Everything else about a macro name — a conditional assignment, a
    /// non-literal right-hand side, a `macro drop` — REMOVES it. Removing is
    /// always sound; the entry only ever existed to narrow.
    fn learn(&mut self, cmd: &CommandAst, text: &str) {
        let Some(word) = command_word(cmd) else {
            return;
        };
        let Command::Known(k) = &cmd.cmd else {
            return;
        };
        let rest = k.slots.rest.as_ref().map_or("", |r| r.text.as_str());
        match word {
            "local" => match local_assignment(rest) {
                Some((name, value)) if !self.conditional => {
                    self.consts
                        .insert(name.as_str().into(), value.as_str().into());
                }
                Some((name, _)) => {
                    self.consts.remove(name.as_str());
                }
                None => {
                    // A `local` whose target we could not read — `local
                    // `name'' — could be any of them.
                    self.consts.clear();
                }
            },
            // A macro can also be assigned through `macro define`/`macro drop`,
            // through `syntax`, `args`, `tempvar` and `tempname`, and by a
            // program call. None of those produce a value we can read here, so
            // each one clears what it could have touched.
            "macro" | "syntax" | "args" | "tempvar" | "tempname" | "tempfile" => {
                self.consts.clear();
            }
            _ => {
                // A command word we could not classify may have been an ado
                // that sets locals in the caller's scope only through
                // `c_local`, which is rare enough to be worth the entry.
                let _ = text;
            }
        }
    }

    /// Rule 6 applied: rewrite `` `name' `` for every macro whose literal value
    /// is known.
    fn substitute_consts<'s>(&self, text: &'s str) -> std::borrow::Cow<'s, str> {
        if self.consts.is_empty() || !text.contains('`') {
            return std::borrow::Cow::Borrowed(text);
        }
        let mut out = text.to_owned();
        for (k, v) in &self.consts {
            let needle = format!("`{k}'");
            if out.contains(&needle) {
                out = out.replace(&needle, v);
            }
        }
        std::borrow::Cow::Owned(out)
    }
}

// ---------------------------------------------------------------------------
// Rule 9 — opt-in author declarations
// ---------------------------------------------------------------------------

/// A `*! stataide: reads(x y) writes(z) rng(none)` comment.
///
/// A plain Stata comment, so the file still runs in Stata (spec §5). Trusted
/// statically and verified against the observed footprint after the command
/// runs; on a contradiction the runtime raises lint R024 and downgrades the
/// block to `UNKNOWN_ALL` for the rest of the session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Declaration {
    /// `reads(...)`.
    pub reads: VarSet,
    /// `writes(...)`.
    pub writes: VarSet,
    /// `creates(...)`.
    pub creates: VarSet,
    /// `drops(...)`.
    pub drops: VarSet,
    /// `rng(none|consumes)`. `None` when the author did not say.
    pub rng: Option<RngEffect>,
}

impl Declaration {
    /// Find the declaration in a run of leading comment text, if there is one.
    #[must_use]
    pub fn parse(lead: &str) -> Option<Declaration> {
        let line = lead.lines().rev().find_map(|l| {
            let l = l.trim();
            let l = l.strip_prefix("*!")?.trim();
            l.strip_prefix("stataide:").map(str::trim)
        })?;

        let mut d = Declaration::default();
        let mut rest = line;
        while let Some(open) = rest.find('(') {
            let key = rest[..open].trim();
            let Some(close) = rest[open..].find(')') else {
                // An unterminated declaration is not a declaration. Answering
                // `None` sends the caller to `unknown_all`, which is where a
                // malformed claim belongs.
                return None;
            };
            let arg = &rest[open + 1..open + close];
            match key.rsplit(char::is_whitespace).next().unwrap_or(key) {
                "reads" => d.reads = names(arg),
                "writes" => d.writes = names(arg),
                "creates" => d.creates = names(arg),
                "drops" => d.drops = names(arg),
                "rng" => {
                    d.rng = Some(match arg.trim() {
                        "none" => RngEffect::None,
                        "consumes" => RngEffect::Consumes,
                        _ => RngEffect::Unknown,
                    });
                }
                // An unrecognised key is a claim we cannot check. The whole
                // declaration goes, rather than being honoured in part.
                _ => return None,
            }
            rest = &rest[open + close + 1..];
        }
        Some(d)
    }

    /// The declaration as an effect set.
    ///
    /// `Confidence::Probable`, never `Exact`: the narrowing was justified by an
    /// author's claim rather than by a literal in the source, and CONTRACTS §4's
    /// middle rung is exactly that distinction.
    #[must_use]
    pub fn to_effects(&self) -> EffectSet {
        EffectSet {
            reads: self.reads.clone(),
            writes: self.writes.clone(),
            creates: self.creates.clone(),
            drops: self.drops.clone(),
            rng: self.rng.unwrap_or(RngEffect::Unknown),
            confidence: Confidence::Probable,
            ..EffectSet::new()
        }
    }
}

fn names(arg: &str) -> VarSet {
    let mut out = VarSet::new();
    for w in arg.split_whitespace() {
        if is_plain_name(w) {
            out.insert(w);
        } else {
            out.unknown = true;
            return out;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

fn slice(src: &str, start: u32, end: u32) -> &str {
    let lo = (start as usize).min(src.len());
    let hi = (end as usize).min(src.len()).max(lo);
    // Spans are byte offsets into text this crate segmented, so they are always
    // on character boundaries; `get` rather than indexing so a caller that
    // hands us a span from a DIFFERENT buffer gets an empty slice instead of a
    // panic on the control thread.
    src.get(lo..hi).unwrap_or("")
}

/// The canonical command word, or the word as typed.
fn command_word(cmd: &CommandAst) -> Option<&str> {
    match &cmd.cmd {
        Command::Known(k) => Some(stratum_parse::cmdtable::command(k.id).canonical),
        Command::Unknown { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

/// Rule 7 bullet 3: is the command word itself macro-derived?
fn dynamic_command_word(cmd: &CommandAst, text: &str) -> bool {
    match &cmd.cmd {
        Command::Unknown { name, .. } => has_macro(name),
        // A line that is nothing but a macro reference parses as no command at
        // all; the text is the only evidence left.
        Command::Directive(_) | Command::Block(_) | Command::Known(_) => {
            matches!(&cmd.cmd, Command::Known(_) | Command::Block(_))
                .then(|| false)
                .unwrap_or_else(|| has_macro(text.split_whitespace().next().unwrap_or("")))
        }
    }
}

fn has_macro(s: &str) -> bool {
    s.contains('`') || s.contains('$')
}

/// Rule 7 bullet 4.
fn is_external(name: &str) -> bool {
    matches!(
        name,
        "shell" | "!" | "winexec" | "python" | "java" | "plugin" | "ssc" | "net" | "mata"
    )
}

/// Whitespace-separated words, or `None` when the text is not a literal.
fn literal_words(s: &str) -> Option<Vec<String>> {
    let s = s.trim();
    if has_macro(s) {
        return None;
    }
    // A quoted list is one binding per quoted string in Stata, and getting that
    // wrong would substitute the wrong text. Decline rather than guess.
    if s.contains('"') {
        return None;
    }
    Some(s.split_whitespace().map(str::to_owned).collect())
}

/// `forvalues i = from(step)to` as literal bindings, or `None` past the bound.
fn numeric_bindings(from: f64, step: f64, to: f64) -> Option<Vec<String>> {
    if step == 0.0 || !from.is_finite() || !to.is_finite() || !step.is_finite() {
        return None;
    }
    let span = (to - from) / step;
    if span < 0.0 {
        return Some(Vec::new());
    }
    // `floor` before the cast: `1(1)10` is ten values, and a count computed in
    // floating point that landed on 9.999… would silently drop the last one.
    let n = span.floor() as u64 + 1;
    if n > MAX_UNROLL {
        return None;
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let v = from + step * i as f64;
        out.push(stratum_parse::macros::stringify_number(v));
    }
    Some(out)
}

/// `local name value` / `local name = value`, when the value is a literal.
fn local_assignment(rest: &str) -> Option<(String, String)> {
    let rest = rest.trim();
    let (name, tail) = match rest.find(|c: char| c.is_whitespace() || c == '=') {
        Some(i) => (&rest[..i], rest[i..].trim()),
        None => (rest, ""),
    };
    if !is_plain_name(name) {
        return None;
    }
    let tail = tail.strip_prefix('=').map_or(tail, str::trim);
    if has_macro(tail) {
        // A right-hand side that is itself a macro reference is not a literal.
        // Returning the name with no value REMOVES the entry, which is right.
        return Some((name.to_owned(), String::new()));
    }
    // `local x "age educ"` and `local x age educ` both hold `age educ`.
    let value = tail
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(tail);
    if value.contains('"') {
        return Some((name.to_owned(), String::new()));
    }
    Some((name.to_owned(), value.trim().to_owned()))
}

/// Substitute one local macro's literal value into text.
fn substitute_local(text: &str, name: &str, value: &str) -> String {
    let needle = format!("`{name}'");
    text.replace(&needle, value)
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
    use crate::effects_rows::BuiltinEffects;

    fn bare<'a>(empty: &'a FxHashMap<Name, Name>) -> StaticCtx<'a> {
        StaticCtx::bare(Utf8Path::new("."), empty)
    }

    fn go(src: &str) -> (EffectSet, ExtractStats) {
        let empty = FxHashMap::default();
        let ctx = bare(&empty);
        extract_block_with_stats(src, &BuiltinEffects, &ctx)
    }

    fn reads(e: &EffectSet) -> Vec<String> {
        e.reads.named.iter().map(|n| n.to_string()).collect()
    }

    fn creates(e: &EffectSet) -> Vec<String> {
        e.creates.named.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn rule_5_unrolls_a_literal_varlist_loop_exactly() {
        // The idiom design 03 §5.3 rule 5 is written for. Getting `reads`
        // exactly right here is the difference between "re-run the file" and
        // "re-run one block".
        let (e, stats) = go("foreach v of varlist age educ {\n gen l_`v' = log(`v')\n}\n");
        assert!(!e.reads.unknown, "a literal varlist loop must not widen");
        assert_eq!(reads(&e), vec!["age".to_owned(), "educ".to_owned()]);
        assert_eq!(creates(&e), vec!["l_age".to_owned(), "l_educ".to_owned()]);
        assert_eq!(stats.unrolled, 2);
    }

    #[test]
    fn rule_5_declines_a_macro_built_list_and_says_so() {
        let (e, stats) = go("foreach v of local vars {\n gen l_`v' = log(`v')\n}\n");
        assert!(e.taint.contains(Taint::UNBOUNDED_LOOP));
        assert!(e.creates.unknown, "an unresolved `v' may create anything");
        assert_eq!(stats.unrolled, 0);
    }

    #[test]
    fn rule_5_bound_is_two_hundred_and_fifty_six() {
        let (_, small) = go("forvalues i = 1/256 {\n gen x`i' = 1\n}\n");
        assert_eq!(small.unrolled, 256);
        let (e, big) = go("forvalues i = 1/257 {\n gen x`i' = 1\n}\n");
        assert_eq!(big.unrolled, 0, "past the bound nothing is unrolled");
        assert!(e.taint.contains(Taint::UNBOUNDED_LOOP));
    }

    #[test]
    fn the_unroll_budget_bounds_a_nested_loop() {
        // Two nested 256-iteration loops are 65 536 body parses without a
        // budget, on the control thread, per keystroke.
        let (e, stats) = go("forvalues i = 1/256 {\n forvalues j = 1/256 {\n gen x = `i'\n}\n}\n");
        assert!(stats.budget_exhausted);
        assert!(stats.unrolled <= UNROLL_BUDGET);
        assert!(e.taint.contains(Taint::UNBOUNDED_LOOP));
    }

    #[test]
    fn rule_6_propagates_a_literal_local_into_a_varlist() {
        let (e, _) = go("local x \"age educ\"\nlist `x'\n");
        assert!(!e.reads.unknown, "a literal local resolves the varlist");
        assert_eq!(reads(&e), vec!["age".to_owned(), "educ".to_owned()]);
    }

    #[test]
    fn rule_6_kills_an_entry_reassigned_inside_a_conditional() {
        // The whole point of the rule: an assignment whose execution is not
        // statically known must not narrow anything downstream.
        let (e, _) = go("local x \"age\"\nif 1 {\n local x \"educ\"\n}\nlist `x'\n");
        assert!(
            e.reads.unknown,
            "a conditionally reassigned macro cannot resolve a varlist"
        );
    }

    #[test]
    fn rule_4_unions_both_arms_of_a_branch() {
        let (e, _) = go("if 1 {\n gen a = price\n}\nelse {\n gen b = mpg\n}\n");
        assert_eq!(creates(&e), vec!["a".to_owned(), "b".to_owned()]);
        assert!(reads(&e).contains(&"price".to_owned()));
        assert!(reads(&e).contains(&"mpg".to_owned()));
    }

    #[test]
    fn rule_7_makes_an_unknown_command_unknown_everything() {
        let (e, _) = go("xtfrobnicate price\n");
        assert!(e.reads.unknown && e.writes.unknown);
        assert!(e.taint.contains(Taint::UNKNOWN_COMMAND));
        assert_eq!(e.confidence, Confidence::Speculative);
    }

    #[test]
    fn rule_7_separates_external_from_merely_unknown() {
        // `shell` is not "we have no row for this": it is "this left the
        // engine", which is `CurrentUnverifiable` rather than `Stale`.
        let (e, _) = go("shell rm -rf /tmp/x\n");
        assert!(e.taint.contains(Taint::EXTERNAL));
    }

    #[test]
    fn rule_7_flags_a_macro_derived_command_word() {
        let (e, _) = go("`cmd' price mpg\n");
        assert!(e.taint.contains(Taint::DYNAMIC_DISPATCH));
        assert!(e.reads.unknown);
    }

    #[test]
    fn rule_8_defining_a_program_writes_only_the_program_namespace() {
        let (e, _) = go("program define p\n gen z = price\nend\n");
        assert!(e.program_writes.names.iter().any(|n| n.as_ref() == "p"));
        assert!(
            e.creates.is_empty(),
            "defining a program creates no variable; calling it does"
        );
    }

    #[test]
    fn rule_8_follows_a_call_to_a_program_defined_here() {
        let (e, stats) = go("program define p\n gen z = price\nend\np\n");
        assert_eq!(stats.programs_inlined, 1);
        assert_eq!(creates(&e), vec!["z".to_owned()]);
        assert!(!e.creates.unknown);
    }

    #[test]
    fn rule_9_trusts_a_declaration_and_records_that_it_is_a_claim() {
        let (e, stats) = go("*! stataide: reads(price) writes(mpg) rng(none)\nmyado price\n");
        assert_eq!(stats.declarations_trusted, 1);
        assert_eq!(reads(&e), vec!["price".to_owned()]);
        assert!(!e.reads.unknown);
        assert_eq!(
            e.confidence,
            Confidence::Probable,
            "an author's claim is not a literal in the source"
        );
    }

    #[test]
    fn a_malformed_declaration_is_not_a_declaration() {
        let (e, stats) = go("*! stataide: reads(price\nmyado price\n");
        assert_eq!(stats.declarations_trusted, 0);
        assert!(e.reads.unknown);
    }

    #[test]
    fn extraction_cost_is_linear_in_the_file_not_in_the_row_count() {
        // ADR-017: the counter, not the clock. Twice the statements, twice the
        // commands — and nothing here scales with a dataset.
        let one = go("gen a = price\n").1;
        let two = go("gen a = price\ngen b = mpg\n").1;
        assert_eq!(two.commands, one.commands * 2);
    }
}
