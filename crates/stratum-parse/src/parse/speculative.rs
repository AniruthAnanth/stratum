//! Speculative parsing for the IDE — design 02 §10.
//!
//! The editor has to fold, highlight, complete and gutter-mark code that has NOT
//! been macro-expanded, because expansion needs a running engine and a keystroke
//! does not have one. So the same recursive-descent code runs with
//! [`ParseMode::Speculative`], in which:
//!
//! * `` `…' `` and `$x` lex to one `MacroRef` token and are accepted wherever a
//!   name, number, string, varlist item or whole expression is expected,
//!   producing [`crate::ast::Expr::Hole`] / [`crate::ast::VarPattern::Hole`];
//! * an unexpected token is RECORDED and skipped rather than aborting;
//! * nothing is reported for a macro that was never expanded.
//!
//! # One grammar, not two
//!
//! 02 §10 chose a mode flag over a second tolerant grammar, and the reason is
//! worth restating: the mode flag costs a predictable branch, whereas two
//! grammars diverge silently and the divergence surfaces as "the editor said it
//! was fine and the engine refused it". That is the worst bug an IDE for an
//! existing language can have.

use stratum_proto::Diagnostic;

use crate::ast::CommandAst;
use crate::scan::{Derived, LogicalLine};

/// A speculative parse: the tree plus whatever could not be understood.
#[derive(Clone, PartialEq, Debug)]
pub struct SpecStmt {
    /// The tree. Contains [`crate::ast::Expr::Hole`] wherever a macro stood.
    pub stmt: CommandAst,
    /// Findings. In speculative mode these are warnings about structure, not
    /// r(198)s: the text is not what will run.
    pub diags: Vec<Diagnostic>,
    /// True when any macro reference reached the tree. Completion and the
    /// "Created by"/"Used by" sidebar (spec §20) downgrade their confidence on
    /// it rather than asserting something they cannot know.
    pub has_holes: bool,
}

/// Parse unexpanded text tolerantly.
pub fn parse_speculative(text: &str) -> SpecStmt {
    let (stmt, diags) = crate::parse::parse_command(text, crate::parse::ParseMode::Speculative);
    let has_holes = tree_has_holes(&stmt);
    SpecStmt {
        stmt,
        diags,
        has_holes,
    }
}

/// Design 02 §13.1's spelling: parse one scanned logical line.
///
/// `src` must be the buffer the line was scanned from and `derived` its entry in
/// the parallel `Segmentation::derived` table — the same pairing
/// [`LogicalLine::code`] requires. Spans in the result are offsets into the
/// line's CODE, which [`LogicalLine::map`] takes back to the source.
pub fn parse_speculative_line(
    line: &LogicalLine,
    src: &str,
    derived: Option<&Derived>,
) -> SpecStmt {
    parse_speculative(line.code(src, derived))
}

fn tree_has_holes(stmt: &CommandAst) -> bool {
    use crate::ast::{BlockCommand, Command, ForeachSource};
    let expr_hole = |e: &Option<crate::ast::Expr>| e.as_ref().is_some_and(|e| e.has_hole());
    match &stmt.cmd {
        Command::Known(k) => {
            k.slots.varlist.as_ref().is_some_and(|v| v.has_hole())
                || expr_hole(&k.slots.assign)
                || expr_hole(&k.slots.if_)
                || k.slots.weight.as_ref().is_some_and(|w| w.expr.has_hole())
        }
        Command::Block(b) => match b.as_ref() {
            BlockCommand::While { cond, .. } => cond.has_hole(),
            BlockCommand::IfElse { arms } => arms
                .iter()
                .any(|(c, _)| c.as_ref().is_some_and(crate::ast::Expr::has_hole)),
            BlockCommand::Foreach {
                source: ForeachSource::OfVarlist(v) | ForeachSource::OfNewlist(v),
                ..
            } => v.has_hole(),
            BlockCommand::Input { spec, .. } => spec.has_hole(),
            _ => false,
        },
        Command::Directive(_) | Command::Unknown { .. } => false,
    }
}
