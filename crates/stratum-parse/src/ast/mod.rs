//! The command AST — design 02 §6.2.
//!
//! **AMENDED (A29).** [`command`] moved from W04b (wave 2) into W04 (wave 1) so
//! that `stratum-effects`' `EffectTable::effects(&self, cmd: &CommandAst, …)`
//! and `stratum-intel` compile before the parser exists. The parser itself —
//! `parse/`, `lex/`, `macros/` — stays in W04b.
//!
//! # NOTE FOR W04b — the one sanctioned edit to this file
//!
//! Design 02 §§7 and 8.4 put `VarList` in `ast/varlist.rs` and `Expr`/`NumList`/
//! `Format` in `ast/expr.rs`, both of which W04b owns. Wave 1 cannot declare
//! them there without claiming a file it does not own, and cannot declare
//! [`command::CommandAst`] without naming them at all. So they live HERE, as
//! opaque spanned-text stand-ins, and W04b replaces this block with
//!
//! ```text
//! pub mod expr;
//! pub mod varlist;
//! pub use expr::{Expr, Format, NumList};
//! pub use varlist::VarList;
//! ```
//!
//! **W04b TOOK THAT EDIT.** `Expr`, `Format`, `NumList` and `VarList` are now the
//! real types from [`expr`] and [`varlist`]; the stand-ins below are gone.
//! `ast/command.rs` is unchanged, exactly as W04 predicted — it names these
//! types through `crate::ast` and never saw the swap.
//!
//! The swap is confined to this module: `ast/command.rs` — the shape
//! `stratum-effects`, `stratum-intel` and W05/W06's effect rows code against —
//! names these types through `crate::ast`, so freezing the `CommandAst` shape in
//! wave 1 cost wave 2 nothing.

pub mod command;
pub mod expr;
pub mod varlist;

pub use command::{
    BlockCommand, ByPrefix, Command, CommandAst, FileSpec, ForeachSource, InRange, KnownCommand,
    NumRange, ObsRef, OptionArg, OptionItem, Options, Prefix, PrefixKind, RawArgs, Slots, Stmt,
    Weight, WeightKind,
};
pub use expr::{
    BinOp, CoefKind, Expr, Format, NumList, NumListItem, StoredClass, SysVar, UnOp, NOT_PREC,
    SIGN_PREC,
};
pub use varlist::{
    BaseLevel, FvOp, TsLag, TsOp, VarAtom, VarItem, VarItemKind, VarList, VarPattern,
};
