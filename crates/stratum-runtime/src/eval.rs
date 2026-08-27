//! Expression evaluation — design 02 §8, CONTRACTS §13.1.
//!
//! # Compile once, evaluate per row
//!
//! [`Compiled`] is the whole design. `replace lnprice = log(price)` on 10 M
//! observations must resolve the name `price` **once**, not ten million times,
//! and must not allocate per row. So the `Expr` the parser produced is lowered
//! to a [`Node`] tree in which every name has already become a `VarIdx`, every
//! function has already become a [`Fun`] discriminant, and every literal has
//! already become an `f64` or a `Box<str>`. After that, evaluating one
//! observation is a tree walk over an allocation-free enum.
//!
//! ADR-017 makes that claim assertable rather than rhetorical:
//! [`Counters::name_resolutions`](crate::ctx::Counters::name_resolutions) counts
//! name→`VarIdx` lookups, and it is a function of the expression's SIZE and the
//! number of CHUNKS, never of the row count. `tests` at the bottom of this file
//! pin that.
//!
//! # The three rules that are easy to get wrong
//!
//! 1. **Arithmetic collapses tags, and an annihilator needs a guard first.**
//!    `.a + 1` is `.`, which `stratum_core::missing::canon` gives for free; but
//!    `.z * 0` is `.` and `canon(0.0)` is `0.0`, so every binary arithmetic
//!    operator asks `either_missing` BEFORE it computes. That is `canon`'s own
//!    documented caveat, and it is the difference between `.` and `0` in a
//!    published table.
//! 2. **Comparison does not propagate missing, because it does not have to.**
//!    Missing values are enormous doubles, so `5 > .` is plain IEEE `false` →
//!    `0` [golden: `core_surface.log`], and `. > 5` is `1`. There is no
//!    special-case code here and there must not be: CONTRACTS §13.1 is explicit
//!    that "every one of Stata's ordering rules falls out of plain IEEE
//!    comparison with **zero** special-case code".
//! 3. **`&` and `|` treat missing as TRUE**, because they are `!= 0` tests and
//!    `.` is `2^1023`. `count if x` counts `.` and skips `0`.
//!
//! # What is deliberately not here
//!
//! The random generators, the date/time family and the matrix functions parse
//! to a signature and answer `rc 10` — *our* "unsupported in this version"
//! (A16), not a Stata code — so that a compatibility report can separate "we
//! are wrong" from "we are incomplete". The RNG in particular cannot be added
//! without design 03 §4.6's `RngFingerprint`, which is `state/` (W06b): a
//! generator that advanced an unrecorded stream would make every downstream
//! block's `Current` a lie.

use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use stratum_core::missing::{canon, either_missing, is_missing, missing_f64, SYSMISS};
use stratum_core::{dist, math, Value};
use stratum_data::Frame;
use stratum_parse::ast::expr::{BinOp, CoefKind, Expr, StoredClass, SysVar, UnOp};
use stratum_parse::lex::{tokens, LexMode};
use stratum_parse::lints::StataError;
use stratum_parse::parse::{parse_expr, Cursor, ParseMode};
use stratum_proto::{Diagnostic, Severity, Span, VarIdx};

use crate::ctx::{ExecCtx, Ns, Settings, StoredResults};

/// Parse ONE standalone expression out of text.
///
/// `stratum_parse::parse_expr` takes a `Cursor` because the parser lexes a
/// whole command line once and hands each slot a sub-cursor over the same token
/// buffer — that is what keeps every span in the AST an offset into the text the
/// caller passed. Three places in this crate have only a bare expression in a
/// `String` and no surrounding command: `` `=exp' `` (`expand_host`), a
/// `syntax` default, and the tests below. Each of them lexing and wrapping for
/// itself is three chances to pick a different `LexMode` and get a different
/// answer for the same text, so the wrapping lives here, once.
///
/// Trailing tokens are an error rather than a silent truncation: `` `=2 +' ``
/// must be `r(198)`, not `2`.
#[must_use]
pub fn parse_expr_text(text: &str, mode: ParseMode) -> (Expr, Vec<Diagnostic>) {
    let lex_mode = match mode {
        ParseMode::Execute => LexMode::Expanded,
        ParseMode::Speculative => LexMode::Speculative,
    };
    let toks = tokens(text, lex_mode);
    let mut cur = Cursor::new(text, &toks, mode);
    let e = parse_expr(&mut cur);
    if !cur.done() {
        let at = cur.peek().span;
        cur.error(198, "invalid syntax", at);
    }
    (e, cur.into_diagnostics())
}

/// The first error in a diagnostic list, as a [`StataError`].
///
/// The parser's own diagnostic already carries the span and the offending
/// token, which `07` calls the single most important field in the product;
/// re-deriving a message at the call site would give the user a second, worse
/// one for the same mistake.
#[must_use]
pub fn first_error(diags: &[Diagnostic]) -> Option<StataError> {
    let d = diags.iter().find(|d| d.severity == Severity::Error)?;
    let mut e = StataError::new(d.stata_rc.unwrap_or(198), d.message.clone());
    if let Some(s) = d.span {
        e = e.at(s);
    }
    if let Some(t) = &d.offending_token {
        e = e.token(t.clone());
    }
    Some(e)
}

/// `_pi`, to the last bit Stata prints.
const PI: f64 = std::f64::consts::PI;

/// What a compiled node evaluates to, without evaluating it.
///
/// `generate` needs this before it creates a column: the storage type of
/// `gen x = "abc"` is `str3`, and of `gen x = 1` it is `float`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Ty {
    /// A double.
    Num,
    /// A string.
    Str,
}

/// The built-in functions this build evaluates.
///
/// A discriminant and not a `&str`, because the alternative is a string compare
/// per row. Resolution happens once, in [`resolve_fun`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[allow(missing_docs)] // one line per Stata function would say only its own name
pub enum Fun {
    Abs,
    Ceil,
    Floor,
    Int,
    Round,
    Sign,
    Mod,
    Min,
    Max,
    Exp,
    Ln,
    Log,
    Log10,
    Sqrt,
    Float,
    Double,
    Missing,
    Inrange,
    Inlist,
    Cond,
    Normal,
    Invnormal,
    Normalden,
    Ttail,
    Chi2tail,
    Ftail,
    Chi2,
    FDist,
    TDist,
    StringOf,
    Real,
    Substr,
    Strlen,
    Length,
    Ustrlen,
    Upper,
    Lower,
    Trim,
    Itrim,
    Strpos,
    Subinstr,
    Word,
    Wordcount,
}

impl Fun {
    /// The type this function returns. `cond()` is whichever branch was taken,
    /// so it answers `None` and the compiler takes the type of its arms.
    const fn ret(self) -> Option<Ty> {
        match self {
            Fun::StringOf
            | Fun::Substr
            | Fun::Upper
            | Fun::Lower
            | Fun::Trim
            | Fun::Itrim
            | Fun::Subinstr
            | Fun::Word => Some(Ty::Str),
            Fun::Cond => None,
            _ => Some(Ty::Num),
        }
    }
}

/// Map a function name to its discriminant. Function names do **not**
/// abbreviate and they are case sensitive ([U] 13.3).
fn resolve_fun(name: &str) -> Option<Fun> {
    Some(match name {
        "abs" => Fun::Abs,
        "ceil" => Fun::Ceil,
        "floor" => Fun::Floor,
        "int" => Fun::Int,
        "round" => Fun::Round,
        "sign" => Fun::Sign,
        "mod" => Fun::Mod,
        "min" => Fun::Min,
        "max" => Fun::Max,
        "exp" => Fun::Exp,
        "ln" => Fun::Ln,
        "log" => Fun::Log,
        "log10" => Fun::Log10,
        "sqrt" => Fun::Sqrt,
        "float" => Fun::Float,
        "double" => Fun::Double,
        "missing" | "mi" => Fun::Missing,
        "inrange" => Fun::Inrange,
        "inlist" => Fun::Inlist,
        "cond" => Fun::Cond,
        "normal" => Fun::Normal,
        "invnormal" => Fun::Invnormal,
        "normalden" => Fun::Normalden,
        "ttail" => Fun::Ttail,
        "chi2tail" => Fun::Chi2tail,
        "Ftail" => Fun::Ftail,
        "chi2" => Fun::Chi2,
        "F" => Fun::FDist,
        "t" => Fun::TDist,
        "string" | "strofreal" => Fun::StringOf,
        "real" => Fun::Real,
        "substr" => Fun::Substr,
        "strlen" => Fun::Strlen,
        "length" => Fun::Length,
        "ustrlen" => Fun::Ustrlen,
        "strupper" | "upper" => Fun::Upper,
        "strlower" | "lower" => Fun::Lower,
        "strtrim" | "trim" => Fun::Trim,
        "itrim" => Fun::Itrim,
        "strpos" => Fun::Strpos,
        "subinstr" => Fun::Subinstr,
        "word" => Fun::Word,
        "wordcount" => Fun::Wordcount,
        _ => return None,
    })
}

/// One node of a compiled expression.
///
/// Every variant that could have needed a lookup at evaluation time has already
/// had it: `Var` is a storage position, `Fun` is a discriminant, `Stored` is an
/// owned key. That is what makes per-row evaluation allocation-free for numeric
/// expressions.
#[derive(Clone, PartialEq, Debug)]
pub enum Node {
    /// A numeric literal, or a missing sentinel.
    Num(f64),
    /// A string literal.
    Str(Box<str>),
    /// A column, by storage position.
    Var(VarIdx),
    /// A scalar, by name.
    Scalar(Box<str>),
    /// `r(x)` / `e(x)` / `c(x)` / `s(x)` with a literal key.
    Stored(StoredClass, Box<str>),
    /// `_n`.
    ObsNo,
    /// `_N`.
    ObsCount,
    /// `_pi`.
    Pi,
    /// `_rc`.
    Rc,
    /// `x[exp]`.
    Index(VarIdx, Box<Node>),
    /// `-x`, `!x`.
    Unary(UnOp, Box<Node>),
    /// `a + b`.
    Binary(BinOp, Box<Node>, Box<Node>),
    /// `f(a, b)`.
    Call(Fun, Vec<Node>),
}

/// A compiled expression, ready to evaluate over observations.
#[derive(Clone, PartialEq, Debug)]
pub struct Compiled {
    root: Node,
    ty: Ty,
    reads: SmallVec<[VarIdx; 4]>,
    order_sensitive: bool,
    span: Span,
}

impl Compiled {
    /// The type this expression evaluates to.
    #[must_use]
    pub fn ty(&self) -> Ty {
        self.ty
    }

    /// The columns it reads, sorted and deduplicated.
    #[must_use]
    pub fn reads(&self) -> &[VarIdx] {
        &self.reads
    }

    /// True when the answer depends on the observation order — `_n`, `_N` or a
    /// subscript. Design 03 §4.8 makes this a structural property of the AST,
    /// and it is what decides whether `row_order` enters the block's footprint.
    #[must_use]
    pub fn order_sensitive(&self) -> bool {
        self.order_sensitive
    }
}

/// Everything evaluation may read. One immutable borrow of the context.
struct Env<'a> {
    frame: &'a Frame,
    scalars: &'a FxHashMap<String, Value>,
    results: &'a StoredResults,
    settings: &'a Settings,
    rc: u32,
}

// ---------------------------------------------------------------------------
// Compilation
// ---------------------------------------------------------------------------

struct Compiler<'a> {
    frame: &'a Frame,
    varabbrev: bool,
    reads: SmallVec<[VarIdx; 4]>,
    order_sensitive: bool,
    resolutions: u64,
}

impl Compiler<'_> {
    fn note_read(&mut self, v: VarIdx) {
        if let Err(at) = self.reads.binary_search(&v) {
            self.reads.insert(at, v);
        }
    }

    /// Resolve a bare name to a column. Exact match first; a unique prefix only
    /// when `set varabbrev` is on ([U] 11.2.3 — `di pri` is r(111) with it off).
    fn lookup_var(&mut self, name: &str) -> Option<VarIdx> {
        self.resolutions += 1;
        if let Some(i) = self.frame.index_of(name) {
            return Some(i);
        }
        if !self.varabbrev || name.is_empty() {
            return None;
        }
        let mut hit = None;
        for (pos, v) in self.frame.vars().iter().enumerate() {
            if v.name.starts_with(name) {
                if hit.is_some() {
                    // Ambiguous: two variables share the prefix, so this is not
                    // an abbreviation of either.
                    return None;
                }
                hit = Some(VarIdx(pos as u32));
            }
        }
        hit
    }

    fn compile(
        &mut self,
        e: &Expr,
        scalars: &FxHashMap<String, Value>,
    ) -> Result<Node, StataError> {
        Ok(match e {
            Expr::Num(v, _) => Node::Num(*v),
            Expr::Missing(tag, _) => Node::Num(missing_f64(*tag)),
            Expr::Str(s, _) => Node::Str(s.as_str().into()),
            Expr::Paren(inner, _) => self.compile(inner, scalars)?,
            Expr::Sys(s, _) => match s {
                SysVar::NLower => {
                    self.order_sensitive = true;
                    Node::ObsNo
                }
                SysVar::NUpper => {
                    self.order_sensitive = true;
                    Node::ObsCount
                }
                SysVar::Pi => Node::Pi,
                SysVar::Rc => Node::Rc,
            },
            Expr::Name(n, sp) => {
                if let Some(idx) = self.lookup_var(n) {
                    self.note_read(idx);
                    Node::Var(idx)
                } else if scalars.contains_key(n.as_str()) {
                    Node::Scalar(n.as_str().into())
                } else {
                    // `errors.log`: in an EXPRESSION position Stata prints the
                    // bare name, without the leading `variable `. The token is
                    // what spec §21 turns into "Did you mean 'income'?".
                    return Err(StataError::new(111, format!("{n} not found"))
                        .at(*sp)
                        .token(n.clone()));
                }
            }
            Expr::Index { base, idx, span } => {
                let Expr::Name(n, nsp) = strip_parens(base) else {
                    return Err(StataError::new(198, "invalid subscript")
                        .at(*span)
                        .token("[".to_owned()));
                };
                let Some(v) = self.lookup_var(n) else {
                    return Err(StataError::new(111, format!("{n} not found"))
                        .at(*nsp)
                        .token(n.clone()));
                };
                self.note_read(v);
                self.order_sensitive = true;
                Node::Index(v, Box::new(self.compile(idx, scalars)?))
            }
            Expr::Unary { op, rhs, .. } => Node::Unary(*op, Box::new(self.compile(rhs, scalars)?)),
            Expr::Binary { op, lhs, rhs, .. } => Node::Binary(
                *op,
                Box::new(self.compile(lhs, scalars)?),
                Box::new(self.compile(rhs, scalars)?),
            ),
            Expr::Call { name, args, span } => {
                let Some(f) = resolve_fun(name) else {
                    return Err(unknown_function(name, *span));
                };
                let sig = stratum_parse::function(name);
                if let Some(sig) = sig {
                    let n = args.len() as u32;
                    if n < u32::from(sig.min_args)
                        || (sig.max_args != 255 && n > u32::from(sig.max_args))
                    {
                        return Err(StataError::new(
                            198,
                            format!("{name}() takes {} argument(s)", sig.min_args),
                        )
                        .at(*span)
                        .token(name.clone()));
                    }
                }
                let mut out = Vec::with_capacity(args.len());
                for a in args {
                    out.push(self.compile(a, scalars)?);
                }
                Node::Call(f, out)
            }
            Expr::Stored { class, key, span } => {
                let Expr::Name(k, _) = strip_parens(key) else {
                    let Expr::Str(k, _) = strip_parens(key) else {
                        return Err(StataError::new(198, "invalid stored-result key")
                            .at(*span)
                            .token("(".to_owned()));
                    };
                    return Ok(Node::Stored(*class, k.as_str().into()));
                };
                Node::Stored(*class, k.as_str().into())
            }
            // `_b[]`/`_se[]` need an estimation result, which is `stratum-stats`
            // and W06b's `e()` table; `i.rep78` inside an expression is a factor
            // term, which only an estimation command can interpret. Both are
            // rc 10, not r(133): they are real Stata, we simply do not do them
            // yet, and A16 requires that distinction to survive to the user.
            Expr::Coef { kind, span, .. } => {
                let what = match kind {
                    CoefKind::B => "_b[]",
                    CoefKind::Se => "_se[]",
                    CoefKind::Coef => "_coef[]",
                };
                return Err(unsupported(what, *span));
            }
            Expr::MatElem { name, span, .. } => {
                return Err(unsupported(&format!("matrix element {name}[i,j]"), *span))
            }
            Expr::Term(_, span) => {
                return Err(unsupported("factor-variable term in an expression", *span))
            }
            // `ParseMode::Execute` never produces one: expansion runs before
            // parsing. Reaching here means a speculative AST was handed to the
            // executor, which is a caller bug, so it says so rather than
            // guessing a value.
            Expr::Hole { src } => {
                return Err(StataError::new(198, "unexpanded macro reached evaluation").at(*src))
            }
        })
    }
}

fn strip_parens(e: &Expr) -> &Expr {
    match e {
        Expr::Paren(inner, _) => strip_parens(inner),
        other => other,
    }
}

fn unknown_function(name: &str, span: Span) -> StataError {
    // r(133) is Stata's own "unknown function", and 02 §8.5 is explicit that the
    // parser must not reject an unknown call — users have ado-files, so the
    // error has to come from evaluation.
    StataError::new(133, format!("unknown function {name}()"))
        .at(span)
        .token(name.to_owned())
}

fn unsupported(what: &str, span: Span) -> StataError {
    StataError::new(10, format!("unsupported in this version: {what}"))
        .at(span)
        .token(what.to_owned())
}

/// Static type of a compiled node.
fn node_ty(n: &Node) -> Ty {
    match n {
        Node::Str(_) => Ty::Str,
        Node::Num(_) | Node::ObsNo | Node::ObsCount | Node::Pi | Node::Rc | Node::Unary(..) => {
            Ty::Num
        }
        Node::Var(_) | Node::Index(_, _) | Node::Scalar(_) | Node::Stored(..) => Ty::Num,
        Node::Binary(op, lhs, _) => {
            // `+` is the only overloaded operator: string + string is
            // concatenation, everything else is numeric. A comparison of two
            // strings is still numeric — it answers 0/1.
            if matches!(op, BinOp::Add) && node_ty(lhs) == Ty::Str {
                Ty::Str
            } else {
                Ty::Num
            }
        }
        Node::Call(f, args) => f.ret().unwrap_or_else(|| {
            // `cond()` — whichever branch was taken. Both arms have the same
            // type in any expression Stata accepts, so the second is enough.
            args.get(1).map_or(Ty::Num, node_ty)
        }),
    }
}

/// The static type of a `Var`/`Index`/`Scalar` node needs the frame, so it is a
/// second pass rather than a field of `node_ty`.
fn resolve_ty(n: &Node, env_frame: &Frame, scalars: &FxHashMap<String, Value>) -> Ty {
    match n {
        Node::Var(v) | Node::Index(v, _) => match env_frame.var(*v) {
            Some(var) if stratum_core::types::is_string(var.ty) => Ty::Str,
            _ => Ty::Num,
        },
        Node::Scalar(name) => match scalars.get(name.as_ref()) {
            Some(Value::Str(_)) => Ty::Str,
            _ => Ty::Num,
        },
        Node::Binary(BinOp::Add, lhs, _) => resolve_ty(lhs, env_frame, scalars),
        Node::Call(f, args) => match f.ret() {
            Some(t) => t,
            None => args
                .get(1)
                .map_or(Ty::Num, |a| resolve_ty(a, env_frame, scalars)),
        },
        other => node_ty(other),
    }
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// Read one observation of a column as a double. Out of range is `.`, never an
/// error (02 §8.4).
fn col_num(frame: &Frame, v: VarIdx, row: u64) -> f64 {
    match frame.col(v) {
        Some(c) if row < c.len() => c.get_f64(row).unwrap_or(SYSMISS),
        _ => SYSMISS,
    }
}

fn col_str(frame: &Frame, v: VarIdx, row: u64) -> String {
    match frame.col(v) {
        Some(c) if row < c.len() => c
            .get_bytes(row)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn eval_node(n: &Node, env: &Env<'_>, row: u64, nodes: &mut u64) -> Result<Value, StataError> {
    *nodes += 1;
    Ok(match n {
        Node::Num(v) => Value::Real(*v),
        Node::Str(s) => Value::Str(s.as_ref().to_owned()),
        Node::Var(v) => match env.frame.var(*v) {
            Some(var) if stratum_core::types::is_string(var.ty) => {
                Value::Str(col_str(env.frame, *v, row))
            }
            _ => Value::Real(col_num(env.frame, *v, row)),
        },
        Node::Index(v, idx) => {
            let sub = eval_node(idx, env, row, nodes)?;
            let string = env
                .frame
                .var(*v)
                .is_some_and(|var| stratum_core::types::is_string(var.ty));
            // A missing or out-of-range subscript yields the type's missing
            // value and never an error — 02 §8.4, and it is what makes
            // `x[_n-1]` usable on observation 1 without a guard.
            let Some(r) = sub.as_real().filter(|r| !is_missing(*r)) else {
                return Ok(if string {
                    Value::Str(String::new())
                } else {
                    Value::missing()
                });
            };
            let one_based = r as i64;
            if one_based < 1 || one_based as u64 > env.frame.n_obs() {
                return Ok(if string {
                    Value::Str(String::new())
                } else {
                    Value::missing()
                });
            }
            let target = one_based as u64 - 1;
            if string {
                Value::Str(col_str(env.frame, *v, target))
            } else {
                Value::Real(col_num(env.frame, *v, target))
            }
        }
        Node::Scalar(name) => env
            .scalars
            .get(name.as_ref())
            .cloned()
            .unwrap_or_else(Value::missing),
        Node::Stored(class, key) => stored_value(*class, key, env),
        Node::ObsNo => Value::Real(row as f64 + 1.0),
        Node::ObsCount => Value::Real(env.frame.n_obs() as f64),
        Node::Pi => Value::Real(PI),
        Node::Rc => Value::Real(f64::from(env.rc)),
        Node::Unary(op, rhs) => {
            let v = eval_node(rhs, env, row, nodes)?;
            let Some(x) = v.as_real() else {
                return Err(StataError::new(109, "type mismatch"));
            };
            match op {
                UnOp::Pos => Value::Real(x),
                // Negation of a missing value is still that missing value's
                // magnitude, which `canon` collapses to `.` — the tag does not
                // survive arithmetic.
                UnOp::Neg if is_missing(x) => Value::Real(SYSMISS),
                UnOp::Neg => Value::Real(canon(-x)),
                // `!.` is 0: missing is nonzero, so its negation is false.
                UnOp::Not => Value::bool(x == 0.0),
            }
        }
        Node::Binary(op, lhs, rhs) => {
            let a = eval_node(lhs, env, row, nodes)?;
            let b = eval_node(rhs, env, row, nodes)?;
            binary(*op, &a, &b)?
        }
        Node::Call(f, args) => call(*f, args, env, row, nodes)?,
    })
}

fn stored_value(class: StoredClass, key: &str, env: &Env<'_>) -> Value {
    if class == StoredClass::C {
        return creturn(key, env);
    }
    crate::ctx::stored_scalar(env.results, class, key)
        .as_ref()
        .map(crate::ctx::scalar_to_value)
        .unwrap_or_else(|| {
            // An unset stored result is `.` in a numeric context and `""` in a
            // string one; Stata resolves the ambiguity toward numeric, and so
            // does every do-file that tests `if r(N) == 0`.
            Value::missing()
        })
}

/// The `c()` keys the interpreter itself answers.
///
/// **W06c's `cmd/settings.rs::creturn` is the full surface** and the `creturn
/// list` command's source; this answers the handful the evaluator needs before
/// that file lands, and forwards to it once it does. `c(linesize)` is `80` in
/// every code path — ADR-016 / A16, and the reason `Settings::LINESIZE` is a
/// constant rather than a field.
fn creturn(key: &str, env: &Env<'_>) -> Value {
    match key {
        "linesize" => Value::Real(f64::from(crate::cmd::settings::LINESIZE)),
        "pi" => Value::Real(PI),
        "N" => Value::Real(env.frame.n_obs() as f64),
        "k" => Value::Real(f64::from(env.frame.n_vars())),
        "rc" => Value::Real(f64::from(env.rc)),
        "changed" => Value::Real(f64::from(u8::from(env.frame.changed()))),
        "more" => Value::Str(if env.settings.more { "on" } else { "off" }.to_owned()),
        "varabbrev" => Value::Str(if env.settings.varabbrev { "on" } else { "off" }.to_owned()),
        _ => Value::missing(),
    }
}

fn binary(op: BinOp, a: &Value, b: &Value) -> Result<Value, StataError> {
    use BinOp::{Add, And, Div, Eq, Ge, Gt, Le, Lt, Mul, Ne, Or, Pow, Sub};
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => Ok(match op {
            Add => Value::Str(format!("{x}{y}")),
            Eq => Value::bool(x == y),
            Ne => Value::bool(x != y),
            Gt => Value::bool(x > y),
            Lt => Value::bool(x < y),
            Ge => Value::bool(x >= y),
            Le => Value::bool(x <= y),
            // `"a" & "b"` and `"a" * 2` are both r(109). The comparison
            // operators above are the whole of what strings support.
            _ => return Err(StataError::new(109, "type mismatch")),
        }),
        (Value::Real(x), Value::Real(y)) => {
            let (x, y) = (*x, *y);
            Ok(match op {
                // Rule 1: an annihilating operator must ask about missing
                // BEFORE it computes. `.z * 0` is `.`, and `canon(0.0)` is
                // `0.0`, so the result-side check cannot do this.
                Add | Sub | Mul | Div | Pow if either_missing(x, y) => Value::Real(SYSMISS),
                Add => Value::Real(canon(x + y)),
                Sub => Value::Real(canon(x - y)),
                Mul => Value::Real(canon(x * y)),
                // `1/0` is `+inf`, which `canon` turns into `.` — golden:
                // `di 1/0` prints `.`.
                Div => Value::Real(canon(x / y)),
                Pow => Value::Real(canon(math::powf(x, y))),
                // Rule 2: plain IEEE comparison. No missing special case.
                Eq => Value::bool(x == y),
                Ne => Value::bool(x != y),
                Gt => Value::bool(x > y),
                Lt => Value::bool(x < y),
                Ge => Value::bool(x >= y),
                Le => Value::bool(x <= y),
                // Rule 3: `&`/`|` are `!= 0` tests, so missing is TRUE.
                And => Value::bool(x != 0.0 && y != 0.0),
                Or => Value::bool(x != 0.0 || y != 0.0),
            })
        }
        // A string on one side and a number on the other is r(109) for every
        // operator, `+` included: `gen x = "text" + 1`.
        _ => Err(StataError::new(109, "type mismatch")),
    }
}

fn num_arg(
    args: &[Node],
    i: usize,
    env: &Env<'_>,
    row: u64,
    n: &mut u64,
) -> Result<f64, StataError> {
    match eval_node(&args[i], env, row, n)? {
        Value::Real(v) => Ok(v),
        Value::Str(_) => Err(StataError::new(109, "type mismatch")),
    }
}

fn str_arg(
    args: &[Node],
    i: usize,
    env: &Env<'_>,
    row: u64,
    n: &mut u64,
) -> Result<String, StataError> {
    match eval_node(&args[i], env, row, n)? {
        Value::Str(s) => Ok(s),
        Value::Real(_) => Err(StataError::new(109, "type mismatch")),
    }
}

/// A unary numeric kernel that propagates missing and canonicalises.
fn unary_num(x: f64, f: impl FnOnce(f64) -> f64) -> Value {
    if is_missing(x) {
        Value::Real(SYSMISS)
    } else {
        Value::Real(canon(f(x)))
    }
}

fn call(f: Fun, args: &[Node], env: &Env<'_>, row: u64, n: &mut u64) -> Result<Value, StataError> {
    Ok(match f {
        Fun::Abs => unary_num(num_arg(args, 0, env, row, n)?, f64::abs),
        Fun::Ceil => unary_num(num_arg(args, 0, env, row, n)?, f64::ceil),
        Fun::Floor => unary_num(num_arg(args, 0, env, row, n)?, f64::floor),
        // `int()` truncates toward zero; `floor()` does not. `int(-1.5)` is
        // `-1` and `floor(-1.5)` is `-2`.
        Fun::Int => unary_num(num_arg(args, 0, env, row, n)?, f64::trunc),
        Fun::Sign => unary_num(num_arg(args, 0, env, row, n)?, |x| {
            if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            }
        }),
        Fun::Round => {
            let x = num_arg(args, 0, env, row, n)?;
            let unit = if args.len() > 1 {
                num_arg(args, 1, env, row, n)?
            } else {
                1.0
            };
            if either_missing(x, unit) || unit == 0.0 {
                Value::Real(if unit == 0.0 && !is_missing(x) {
                    // `round(x, 0)` is `x` — [U] 13.3, and it is what keeps a
                    // rounding unit built from a macro from destroying data.
                    x
                } else {
                    SYSMISS
                })
            } else {
                Value::Real(canon((x / unit).round() * unit))
            }
        }
        Fun::Mod => {
            let (x, y) = (
                num_arg(args, 0, env, row, n)?,
                num_arg(args, 1, env, row, n)?,
            );
            if either_missing(x, y) || y == 0.0 {
                Value::Real(SYSMISS)
            } else {
                // Stata's `mod()` is the non-negative remainder, not `%`.
                Value::Real(canon(x - y * (x / y).floor()))
            }
        }
        Fun::Min | Fun::Max => {
            // Stata's `min()`/`max()` IGNORE missing arguments and answer `.`
            // only when every argument is missing — unlike arithmetic, where one
            // missing operand poisons the result.
            let mut best: Option<f64> = None;
            for i in 0..args.len() {
                let v = num_arg(args, i, env, row, n)?;
                if is_missing(v) {
                    continue;
                }
                best = Some(match best {
                    None => v,
                    Some(b) if f == Fun::Min => b.min(v),
                    Some(b) => b.max(v),
                });
            }
            Value::Real(best.unwrap_or(SYSMISS))
        }
        Fun::Exp => unary_num(num_arg(args, 0, env, row, n)?, math::exp),
        // `log()` and `ln()` are the SAME function in Stata: `log` is not
        // base 10. `log10` is base 10.
        Fun::Ln | Fun::Log => unary_num(num_arg(args, 0, env, row, n)?, |x| {
            if x <= 0.0 {
                SYSMISS
            } else {
                math::ln(x)
            }
        }),
        Fun::Log10 => unary_num(num_arg(args, 0, env, row, n)?, |x| {
            if x <= 0.0 {
                SYSMISS
            } else {
                math::log10(x)
            }
        }),
        Fun::Sqrt => unary_num(num_arg(args, 0, env, row, n)?, |x| {
            if x < 0.0 {
                SYSMISS
            } else {
                x.sqrt()
            }
        }),
        // `float(x)` rounds to float precision and back; `double(x)` is a no-op
        // on a value that is already a double, which every expression is.
        Fun::Float => unary_num(num_arg(args, 0, env, row, n)?, |x| f64::from(x as f32)),
        Fun::Double => unary_num(num_arg(args, 0, env, row, n)?, |x| x),
        Fun::Missing => {
            let mut any = false;
            // Every argument is evaluated, never short-circuited: `missing()` is
            // variadic and its arguments can be arbitrary expressions, so the
            // node count `n` must come out the same whichever argument is the
            // missing one.
            for arg in args {
                any |= match eval_node(arg, env, row, n)? {
                    Value::Real(v) => is_missing(v),
                    Value::Str(s) => s.is_empty(),
                };
            }
            Value::bool(any)
        }
        Fun::Inrange => {
            let x = num_arg(args, 0, env, row, n)?;
            let lo = num_arg(args, 1, env, row, n)?;
            let hi = num_arg(args, 2, env, row, n)?;
            // `inrange` is the one place missing is FALSE rather than truthy:
            // it exists precisely so that a range test excludes `.`.
            Value::bool(!is_missing(x) && x >= lo && x <= hi)
        }
        Fun::Inlist => {
            let head = eval_node(&args[0], env, row, n)?;
            let mut hit = false;
            // Same reason as `missing()` above — no short-circuit, so the node
            // count does not depend on where the match is.
            for arg in &args[1..] {
                hit |= eval_node(arg, env, row, n)? == head;
            }
            Value::bool(hit)
        }
        Fun::Cond => {
            let c = num_arg(args, 0, env, row, n)?;
            // A missing condition selects the fourth argument when there is one
            // and the FALSE branch otherwise — [U] 13.3, and it is why `cond`
            // takes three or four arguments rather than three.
            let which = if is_missing(c) {
                if args.len() == 4 {
                    3
                } else {
                    2
                }
            } else if c != 0.0 {
                1
            } else {
                2
            };
            eval_node(&args[which], env, row, n)?
        }
        Fun::Normal => unary_num(num_arg(args, 0, env, row, n)?, dist::normal_cdf),
        Fun::Invnormal => unary_num(num_arg(args, 0, env, row, n)?, dist::normal_inv),
        Fun::Normalden => unary_num(num_arg(args, 0, env, row, n)?, dist::normal_pdf),
        Fun::Ttail => {
            let (t, df) = (
                num_arg(args, 0, env, row, n)?,
                num_arg(args, 1, env, row, n)?,
            );
            binary_dist(t, df, dist::t_sf)
        }
        Fun::TDist => {
            let (df, t) = (
                num_arg(args, 0, env, row, n)?,
                num_arg(args, 1, env, row, n)?,
            );
            binary_dist(df, t, |df, t| dist::t_cdf(t, df))
        }
        Fun::Chi2tail => {
            let (df, x) = (
                num_arg(args, 0, env, row, n)?,
                num_arg(args, 1, env, row, n)?,
            );
            binary_dist(df, x, |df, x| dist::chi2_sf(x, df))
        }
        Fun::Chi2 => {
            let (df, x) = (
                num_arg(args, 0, env, row, n)?,
                num_arg(args, 1, env, row, n)?,
            );
            binary_dist(df, x, |df, x| dist::chi2_cdf(x, df))
        }
        Fun::Ftail | Fun::FDist => {
            let d1 = num_arg(args, 0, env, row, n)?;
            let d2 = num_arg(args, 1, env, row, n)?;
            let x = num_arg(args, 2, env, row, n)?;
            if is_missing(d1) || is_missing(d2) || is_missing(x) {
                Value::Real(SYSMISS)
            } else if f == Fun::Ftail {
                Value::Real(canon(dist::f_sf(x, d1, d2)))
            } else {
                Value::Real(canon(dist::f_cdf(x, d1, d2)))
            }
        }
        Fun::StringOf => {
            let x = num_arg(args, 0, env, row, n)?;
            if args.len() > 1 {
                let fmt = str_arg(args, 1, env, row, n)?;
                match stratum_core::fmt::StataFormat::parse(&fmt) {
                    Ok(f) => Value::Str(f.format_f64(x).trim().to_owned()),
                    Err(_) => Value::Str(String::new()),
                }
            } else {
                // `string()`'s default is `%10.0g`, which is also what a bare
                // `display` of a number uses — golden: `di 1/3` is `.33333333`,
                // eight significant digits.
                Value::Str(stratum_core::fmt::fmt_g(x, 10).trim().to_owned())
            }
        }
        Fun::Real => {
            let s = str_arg(args, 0, env, row, n)?;
            Value::Real(s.trim().parse::<f64>().map_or(SYSMISS, canon))
        }
        Fun::Substr => {
            let s = str_arg(args, 0, env, row, n)?;
            let start = num_arg(args, 1, env, row, n)?;
            let len = num_arg(args, 2, env, row, n)?;
            Value::Str(substr(&s, start, len))
        }
        Fun::Strlen | Fun::Length => {
            // `length()` is polymorphic: on a number it is the length of its
            // %g rendering, which is why it is not simply `strlen`.
            match eval_node(&args[0], env, row, n)? {
                Value::Str(s) => Value::Real(s.len() as f64),
                Value::Real(v) if f == Fun::Length => {
                    Value::Real(stratum_core::fmt::fmt_g(v, 10).trim().len() as f64)
                }
                Value::Real(_) => return Err(StataError::new(109, "type mismatch")),
            }
        }
        Fun::Ustrlen => Value::Real(str_arg(args, 0, env, row, n)?.chars().count() as f64),
        Fun::Upper => Value::Str(str_arg(args, 0, env, row, n)?.to_uppercase()),
        Fun::Lower => Value::Str(str_arg(args, 0, env, row, n)?.to_lowercase()),
        Fun::Trim => Value::Str(str_arg(args, 0, env, row, n)?.trim().to_owned()),
        Fun::Itrim => {
            let s = str_arg(args, 0, env, row, n)?;
            let mut out = String::with_capacity(s.len());
            let mut prev_space = false;
            for c in s.chars() {
                let sp = c == ' ';
                if !(sp && prev_space) {
                    out.push(c);
                }
                prev_space = sp;
            }
            Value::Str(out)
        }
        Fun::Strpos => {
            let hay = str_arg(args, 0, env, row, n)?;
            let needle = str_arg(args, 1, env, row, n)?;
            // 1-based, 0 for "not found" — and an empty needle is 0, not 1.
            Value::Real(if needle.is_empty() {
                0.0
            } else {
                hay.find(&needle).map_or(0.0, |i| i as f64 + 1.0)
            })
        }
        Fun::Subinstr => {
            let s = str_arg(args, 0, env, row, n)?;
            let from = str_arg(args, 1, env, row, n)?;
            let to = str_arg(args, 2, env, row, n)?;
            let count = num_arg(args, 3, env, row, n)?;
            Value::Str(if from.is_empty() {
                s
            } else if is_missing(count) {
                s.replace(&from, &to)
            } else {
                s.replacen(&from, &to, count.max(0.0) as usize)
            })
        }
        Fun::Word => {
            let s = str_arg(args, 0, env, row, n)?;
            let k = num_arg(args, 1, env, row, n)?;
            let words: Vec<&str> = s.split_whitespace().collect();
            let i = if k < 0.0 {
                words.len() as i64 + k as i64
            } else {
                k as i64 - 1
            };
            Value::Str(
                usize::try_from(i)
                    .ok()
                    .and_then(|i| words.get(i))
                    .map_or_else(String::new, |w| (*w).to_owned()),
            )
        }
        Fun::Wordcount => {
            Value::Real(str_arg(args, 0, env, row, n)?.split_whitespace().count() as f64)
        }
    })
}

fn binary_dist(a: f64, b: f64, f: impl FnOnce(f64, f64) -> f64) -> Value {
    if either_missing(a, b) {
        Value::Real(SYSMISS)
    } else {
        Value::Real(canon(f(a, b)))
    }
}

/// `substr(s, start, len)` — 1-based, negative `start` counts from the end, and
/// a `len` of `.` means "to the end" ([U] 13.3).
///
/// Byte-oriented, like Stata's own `substr`; `usubstr` is the character-oriented
/// one and is not implemented here. A slice that lands mid-codepoint is repaired
/// lossily rather than panicking, because a `str8` column holding a truncated
/// UTF-8 sequence is a thing a real `.dta` contains.
fn substr(s: &str, start: f64, len: f64) -> String {
    if is_missing(start) {
        return String::new();
    }
    let b = s.as_bytes();
    let n = b.len() as i64;
    let start = start as i64;
    let from = if start < 0 { n + start } else { start - 1 };
    if from < 0 || from >= n {
        return String::new();
    }
    let take = if is_missing(len) {
        n - from
    } else {
        (len as i64).max(0)
    };
    let to = (from + take).min(n);
    String::from_utf8_lossy(&b[from as usize..to as usize]).into_owned()
}

// ---------------------------------------------------------------------------
// The entry points the command surface calls
// ---------------------------------------------------------------------------

impl ExecCtx<'_> {
    /// Compile an expression against the current frame, recording the columns
    /// it reads.
    ///
    /// # Errors
    ///
    /// `r(111)` for a name that is neither a variable nor a scalar, `r(133)` for
    /// an unknown function, `rc 10` for one this build does not implement.
    pub fn compile_expr(&mut self, e: &Expr) -> Result<Compiled, StataError> {
        let (node, reads, order_sensitive, resolutions, ty) = {
            let frame = self.frames.current();
            let mut c = Compiler {
                frame,
                varabbrev: self.settings.varabbrev,
                reads: SmallVec::new(),
                order_sensitive: false,
                resolutions: 0,
            };
            let node = c.compile(e, &self.scalars)?;
            let ty = resolve_ty(&node, frame, &self.scalars);
            (node, c.reads, c.order_sensitive, c.resolutions, ty)
        };
        self.counters.name_resolutions += resolutions;
        for v in &reads {
            self.access.note_read(*v);
        }
        if order_sensitive {
            self.access.read_row_order = true;
        }
        Ok(Compiled {
            root: node,
            ty,
            reads,
            order_sensitive,
            span: e.span(),
        })
    }

    /// The type an expression evaluates to, without evaluating it.
    ///
    /// # Errors
    ///
    /// As [`ExecCtx::compile_expr`].
    pub fn expr_ty(&mut self, e: &Expr) -> Result<Ty, StataError> {
        Ok(self.compile_expr(e)?.ty)
    }

    /// Evaluate once, outside any observation — `display`, `scalar =`, a
    /// control-flow condition. `_n` is observation 1.
    ///
    /// # Errors
    ///
    /// As [`ExecCtx::compile_expr`], plus `r(109)` on a type mismatch.
    pub fn eval_scalar(&mut self, e: &Expr) -> Result<Value, StataError> {
        let prog = self.compile_expr(e)?;
        self.eval_compiled_at(&prog, 0)
    }

    /// Evaluate a compiled expression at one observation.
    ///
    /// # Errors
    ///
    /// `r(109)` on a type mismatch.
    pub fn eval_compiled_at(&mut self, prog: &Compiled, row: u64) -> Result<Value, StataError> {
        let mut nodes = 0u64;
        let v = {
            let env = self.eval_env();
            eval_node(&prog.root, &env, row, &mut nodes)
        };
        self.counters.eval_nodes += nodes;
        self.counters.rows_touched += 1;
        v.map_err(|e| if e.span.is_none() { e.at(prog.span) } else { e })
    }

    /// Evaluate a **numeric** compiled expression for `len` observations
    /// starting at `row0`, appending exactly `len` values to `out`.
    ///
    /// Chunk-wise and never whole-column: the caller passes
    /// [`stratum_data::CHUNK_ROWS`] at a time, so a `replace` over 10 M rows
    /// needs one 512 KiB scratch buffer and not an 80 MB temporary. That is also
    /// the granule the storage and the undo journal use (A18, C35), so a fold
    /// boundary is a memory boundary.
    ///
    /// # Errors
    ///
    /// `r(109)` when the expression is a string.
    pub fn eval_compiled_num_rows(
        &mut self,
        prog: &Compiled,
        row0: u64,
        len: usize,
        out: &mut Vec<f64>,
    ) -> Result<(), StataError> {
        if prog.ty == Ty::Str {
            return Err(StataError::new(109, "type mismatch").at(prog.span));
        }
        let mut nodes = 0u64;
        let r = {
            let env = self.eval_env();
            (0..len).try_for_each(|i| {
                let v = eval_node(&prog.root, &env, row0 + i as u64, &mut nodes)?;
                out.push(v.as_real().unwrap_or(SYSMISS));
                Ok::<(), StataError>(())
            })
        };
        self.counters.eval_nodes += nodes;
        self.counters.rows_touched += len as u64;
        r.map_err(|e| if e.span.is_none() { e.at(prog.span) } else { e })
    }

    /// The string counterpart of [`ExecCtx::eval_compiled_num_rows`].
    ///
    /// # Errors
    ///
    /// `r(109)` when the expression is numeric.
    pub fn eval_compiled_str_rows(
        &mut self,
        prog: &Compiled,
        row0: u64,
        len: usize,
        out: &mut Vec<String>,
    ) -> Result<(), StataError> {
        if prog.ty == Ty::Num {
            return Err(StataError::new(109, "type mismatch").at(prog.span));
        }
        let mut nodes = 0u64;
        let r = {
            let env = self.eval_env();
            (0..len).try_for_each(|i| {
                let v = eval_node(&prog.root, &env, row0 + i as u64, &mut nodes)?;
                out.push(match v {
                    Value::Str(s) => s,
                    Value::Real(_) => return Err(StataError::new(109, "type mismatch")),
                });
                Ok::<(), StataError>(())
            })
        };
        self.counters.eval_nodes += nodes;
        self.counters.rows_touched += len as u64;
        r.map_err(|e| if e.span.is_none() { e.at(prog.span) } else { e })
    }

    fn eval_env(&self) -> Env<'_> {
        Env {
            frame: self.frames.current(),
            scalars: &self.scalars,
            results: &self.results,
            settings: &self.settings,
            rc: self.rc,
        }
    }

    /// Record that this command read a named namespace — the barrier a command
    /// implementation calls when it touches macros, scalars or settings.
    pub fn note_named_read(&mut self, ns: Ns, name: &str) {
        self.access.note_named_read(ns, name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::{NoHost, Transcript};
    use stratum_data::StorageType;

    /// Parse an expression the way the executor does.
    fn expr(src: &str) -> Expr {
        let (e, diags) = parse_expr_text(src, ParseMode::Execute);
        assert!(
            diags
                .iter()
                .all(|d| d.severity != stratum_proto::Severity::Error),
            "parse of {src:?} produced errors: {diags:?}"
        );
        e
    }

    struct Fixture {
        out: Transcript,
        host: NoHost,
    }

    impl Fixture {
        fn new() -> Self {
            Fixture {
                out: Transcript::new(),
                host: NoHost,
            }
        }
    }

    /// A context over a 5-observation frame with `x` (double) and `s` (str8).
    fn ctx_with_data(f: &mut Fixture) -> ExecCtx<'_> {
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        let frame = ctx.frames.current_mut();
        frame.set_n_obs(5);
        let x = frame.add_var("x", StorageType::Double).unwrap();
        let s = frame.add_var("s", StorageType::Str { width: 8 }).unwrap();
        for (row, v) in [1.0f64, 2.0, SYSMISS, 4.0, 5.0].into_iter().enumerate() {
            frame.col_mut(x).unwrap().set_f64(row as u64, v).unwrap();
        }
        for (row, v) in ["a", "bb", "", "dddd", "e"].into_iter().enumerate() {
            frame
                .col_mut(s)
                .unwrap()
                .set_bytes(row as u64, v.as_bytes())
                .unwrap();
        }
        ctx
    }

    fn scalar(ctx: &mut ExecCtx<'_>, src: &str) -> Value {
        ctx.eval_scalar(&expr(src)).expect("evaluates")
    }

    fn num(ctx: &mut ExecCtx<'_>, src: &str) -> f64 {
        scalar(ctx, src).as_real().expect("numeric")
    }

    // ── the golden display corpus, core_surface.log lines 585..616 ───────────

    // `round(3.14159, 0.01)` below asserts `3.14` because that is the literal
    // `core_surface.log` prints. clippy reads those digits and sees an
    // approximation of `f64::consts::PI`; it is a rounding *result* transcribed
    // from the golden and compared for exact equality, and swapping in the
    // constant would assert a number StataMP never printed. Scoped to this fn so
    // `approx_constant` keeps its teeth in the kernels.
    #[allow(clippy::approx_constant)]
    #[test]
    fn golden_display_expressions() {
        let mut f = Fixture::new();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        assert_eq!(num(&mut ctx, "2 + 3 * 4"), 14.0);
        assert_eq!(num(&mut ctx, "log(exp(1))"), 1.0);
        assert_eq!(num(&mut ctx, "sqrt(16)"), 4.0);
        assert_eq!(num(&mut ctx, "round(3.14159, 0.01)"), 3.14);
        // `di 1/0` prints `.`: +inf canonicalises to the system missing value.
        assert!(is_missing(num(&mut ctx, "1/0")));
        assert!(is_missing(num(&mut ctx, ".")));
        assert_eq!(num(&mut ctx, "missing(.)"), 1.0);
        // The one everybody ports wrong: missing is the LARGEST double.
        assert_eq!(num(&mut ctx, "5 > ."), 0.0);
        assert_eq!(
            scalar(&mut ctx, r#"("abc" + "def")"#),
            Value::Str("abcdef".to_owned())
        );
        assert_eq!(num(&mut ctx, r#"length("hello")"#), 5.0);
        assert_eq!(
            scalar(&mut ctx, r#"substr("hello", 2, 3)"#),
            Value::Str("ell".to_owned())
        );
    }

    #[test]
    fn precedence_follows_the_machine_not_the_manual() {
        // `stratum_parse` verified these against StataMP 18.5; evaluating them
        // here is what proves the evaluator agrees with the parser it is fed by.
        let mut f = Fixture::new();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        assert_eq!(num(&mut ctx, "2^3^2"), 64.0, "^ is LEFT associative");
        assert_eq!(num(&mut ctx, "!2^0"), 0.0, "^ binds tighter than !");
        assert_eq!(num(&mut ctx, "-2^2"), -4.0, "unary - is looser than ^");
    }

    #[test]
    fn an_annihilator_asks_about_missing_before_it_computes() {
        // `canon`'s documented caveat, and the difference between `.` and `0`
        // in a published table.
        let mut f = Fixture::new();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        assert!(is_missing(num(&mut ctx, ".z * 0")));
        assert!(is_missing(num(&mut ctx, "0 * .")));
        // …and arithmetic collapses the tag rather than carrying it.
        assert_eq!(
            stratum_core::missing::tag_of(num(&mut ctx, ".a + 1")),
            Some(0)
        );
    }

    #[test]
    fn missing_is_truthy_in_and_or_and_not() {
        let mut f = Fixture::new();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        assert_eq!(num(&mut ctx, ". & 1"), 1.0);
        assert_eq!(num(&mut ctx, ". | 0"), 1.0);
        assert_eq!(num(&mut ctx, "!."), 0.0);
        assert_eq!(num(&mut ctx, "!0"), 1.0);
    }

    #[test]
    fn inrange_is_the_one_place_missing_is_false() {
        let mut f = Fixture::new();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        assert_eq!(num(&mut ctx, "inrange(., 1, 10)"), 0.0);
        assert_eq!(num(&mut ctx, "inrange(5, 1, 10)"), 1.0);
    }

    #[test]
    fn min_and_max_ignore_missing_arguments() {
        let mut f = Fixture::new();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        assert_eq!(num(&mut ctx, "min(3, ., 1)"), 1.0);
        assert_eq!(num(&mut ctx, "max(3, ., 1)"), 3.0);
        assert!(is_missing(num(&mut ctx, "max(., .)")));
    }

    #[test]
    fn a_variable_resolves_before_a_scalar_and_reports_r111_otherwise() {
        let mut f = Fixture::new();
        let mut ctx = ctx_with_data(&mut f);
        assert_eq!(num(&mut ctx, "x"), 1.0, "observation 1 of x");
        let e = ctx.eval_scalar(&expr("nosuchthing")).unwrap_err();
        assert_eq!(e.rc, 111);
        // A missing offending_token here is a merge blocker (plan W06).
        assert_eq!(e.offending_token.as_deref(), Some("nosuchthing"));
    }

    #[test]
    fn a_subscript_out_of_range_is_missing_and_not_an_error() {
        let mut f = Fixture::new();
        let mut ctx = ctx_with_data(&mut f);
        assert!(is_missing(num(&mut ctx, "x[0]")));
        assert!(is_missing(num(&mut ctx, "x[99]")));
        assert!(is_missing(num(&mut ctx, "x[.]")));
        assert_eq!(num(&mut ctx, "x[2]"), 2.0);
    }

    #[test]
    fn string_and_numeric_mixing_is_r109() {
        let mut f = Fixture::new();
        let mut ctx = ctx_with_data(&mut f);
        assert_eq!(ctx.eval_scalar(&expr(r#""a" + 1"#)).unwrap_err().rc, 109);
        assert_eq!(ctx.eval_scalar(&expr("s * 2")).unwrap_err().rc, 109);
    }

    #[test]
    fn expression_type_is_answered_without_evaluating() {
        let mut f = Fixture::new();
        let mut ctx = ctx_with_data(&mut f);
        assert_eq!(ctx.expr_ty(&expr("x + 1")).unwrap(), Ty::Num);
        assert_eq!(ctx.expr_ty(&expr("s")).unwrap(), Ty::Str);
        assert_eq!(ctx.expr_ty(&expr(r#"s + "!""#)).unwrap(), Ty::Str);
        // A comparison of two strings is numeric — it answers 0/1.
        assert_eq!(ctx.expr_ty(&expr(r#"s == "a""#)).unwrap(), Ty::Num);
        assert_eq!(ctx.expr_ty(&expr(r#"substr(s, 1, 1)"#)).unwrap(), Ty::Str);
    }

    // ── ADR-017: the counter, not the clock ─────────────────────────────────

    #[test]
    fn name_resolution_is_a_function_of_the_expression_not_of_the_row_count() {
        // THE property `Compiled` exists to have. `x + x + x` resolves `x` three
        // times when it is compiled and never again, however many observations
        // it is then evaluated over. The assertion is a counter, per ADR-017.
        let mut f = Fixture::new();
        let mut ctx = ctx_with_data(&mut f);
        let e = expr("x + x + x");

        let prog = ctx.compile_expr(&e).unwrap();
        let after_compile = ctx.counters.name_resolutions;
        assert_eq!(after_compile, 3, "one lookup per Name node, once");

        let mut out = Vec::new();
        ctx.eval_compiled_num_rows(&prog, 0, 5, &mut out).unwrap();
        assert_eq!(
            ctx.counters.name_resolutions, after_compile,
            "evaluating 5 observations resolved no names at all"
        );
        assert_eq!(ctx.counters.rows_touched, 5);
        assert_eq!(out.len(), 5);
        assert_eq!(out[0], 3.0);
        assert!(is_missing(out[2]), "missing propagates through +");
    }

    #[test]
    fn a_compiled_expression_reports_what_it_reads_and_whether_order_matters() {
        let mut f = Fixture::new();
        let mut ctx = ctx_with_data(&mut f);
        let plain = ctx.compile_expr(&expr("x + 1")).unwrap();
        assert_eq!(plain.reads(), &[VarIdx(0)]);
        assert!(!plain.order_sensitive());

        let subscripted = ctx.compile_expr(&expr("x[_n-1]")).unwrap();
        assert!(
            subscripted.order_sensitive(),
            "design 03 §4.8: a subscript makes the block order-sensitive"
        );
        assert!(ctx.access.read_row_order);
    }

    #[test]
    fn evaluating_over_rows_records_the_read_once_not_once_per_row() {
        let mut f = Fixture::new();
        let mut ctx = ctx_with_data(&mut f);
        let prog = ctx.compile_expr(&expr("x * 2")).unwrap();
        let mut out = Vec::new();
        ctx.eval_compiled_num_rows(&prog, 0, 5, &mut out).unwrap();
        assert_eq!(ctx.access.vars_read, vec![VarIdx(0)]);
    }

    // ── the unimplemented surface answers rc 10, not a Stata code ────────────

    #[test]
    fn an_unimplemented_function_is_distinguishable_from_a_wrong_one() {
        // A16: rc 10 means "we are incomplete", r(133) means "that is not a
        // function". A compatibility report has to be able to tell them apart.
        let mut f = Fixture::new();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        let unknown = ctx.eval_scalar(&expr("nosuchfn(1)")).unwrap_err();
        assert_eq!(unknown.rc, 133);
        assert_eq!(unknown.offending_token.as_deref(), Some("nosuchfn"));
    }

    #[test]
    fn c_linesize_is_eighty_in_every_code_path() {
        let mut f = Fixture::new();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        assert_eq!(num(&mut ctx, "c(linesize)"), 80.0);
    }

    #[test]
    fn substr_handles_the_negative_and_to_the_end_forms() {
        let mut f = Fixture::new();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        assert_eq!(
            scalar(&mut ctx, r#"substr("hello", -3, 3)"#),
            Value::Str("llo".to_owned())
        );
        assert_eq!(
            scalar(&mut ctx, r#"substr("hello", 2, .)"#),
            Value::Str("ello".to_owned())
        );
        assert_eq!(
            scalar(&mut ctx, r#"substr("hello", 9, 2)"#),
            Value::Str(String::new())
        );
    }
}
