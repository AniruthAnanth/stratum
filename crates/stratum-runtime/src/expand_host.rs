//! The runtime half of macro expansion — CONTRACTS §13's `ExpandHost`.
//!
//! `stratum-parse` does the substitution algorithm and the text-only extended
//! macro functions; the two things it cannot do are the two methods here:
//! `` `=exp' `` needs the expression evaluator, and the state-dependent extended
//! macro functions (`` `:type price' ``, `` `:di %9.4f 1/3' ``) need the dataset
//! and the formatter. Both re-enter the interpreter, which is why
//! `ExpandStats::host_calls` counts them separately.
//!
//! # `%18.0g`, and it is not a display choice
//!
//! Design 02 §4.4 turns an `` `=exp' `` result into macro text with `%18.0g`
//! then trim. That **loses precision on large magnitudes exactly the way Stata
//! loses it**, and a loop that accumulates a counter into a macro diverges on
//! the first value past 18 significant digits if you reach for `f64::to_string`
//! instead. `stratum_parse::macros::stringify_number` is that rule, delegating
//! to the workspace's one `%g`; this module calls it and does not reimplement
//! it.
//!
//! # The borrow, and why the environment is moved out and back
//!
//! `expand(text, &mut MacroEnv, &mut dyn ExpandHost)` wants a mutable borrow of
//! the environment *and* of the host, and in this crate both live inside one
//! `ExecCtx`. [`ExecCtx::expand_line`] moves the environment out for the
//! duration of the call and puts it back afterwards. That is sound because
//! nothing an `ExpandHost` callback can reach reads the macro environment:
//! `expand` has already substituted every macro reference in the text it hands
//! back to us, so `` `=`x'+1' `` arrives here as `=2+1`. A callback that DID
//! need the environment would be a design error — it would mean expansion order
//! was observable twice.

use stratum_data::StorageType;
use stratum_parse::lints::StataError;
use stratum_parse::macros::{expand, ExpandHost, Expansion};
use stratum_parse::ParseMode;

use crate::ctx::ExecCtx;
use crate::eval::{first_error, parse_expr_text};

/// [`ExpandHost`] over a live interpreter.
///
/// Constructed for the duration of one [`ExecCtx::expand_line`] call and
/// dropped; it holds a mutable borrow of the context and must not outlive it.
pub struct RuntimeExpandHost<'a, 'h> {
    ctx: &'a mut ExecCtx<'h>,
}

impl<'a, 'h> RuntimeExpandHost<'a, 'h> {
    /// Wrap a context.
    pub fn new(ctx: &'a mut ExecCtx<'h>) -> Self {
        Self { ctx }
    }
}

impl ExpandHost for RuntimeExpandHost<'_, '_> {
    fn eval_expr_to_macro_text(&mut self, exp: &str) -> Result<String, StataError> {
        self.ctx.counters.host_callbacks += 1;
        let (ast, diags) = parse_expr_text(exp, ParseMode::Execute);
        if let Some(e) = first_error(&diags) {
            return Err(e);
        }
        let v = self.ctx.eval_scalar(&ast)?;
        Ok(match v {
            stratum_core::Value::Real(x) => stratum_parse::macros::stringify_number(x),
            stratum_core::Value::Str(s) => s,
        })
    }

    fn eval_xmf(&mut self, body: &str) -> Result<String, StataError> {
        self.ctx.counters.host_callbacks += 1;
        let body = body.trim();
        let (head, rest) = match body.find(char::is_whitespace) {
            Some(i) => (&body[..i], body[i..].trim()),
            None => (body, ""),
        };
        match head {
            // `` `:type varname' `` — the storage type as `describe` spells it.
            "type" => {
                let frame = self.ctx.frames.current();
                let Some(idx) = frame.index_of(rest) else {
                    return Err(
                        StataError::new(111, format!("variable {rest} not found")).token(rest)
                    );
                };
                self.ctx.access.read_var_layout = true;
                let ty = self
                    .ctx
                    .frames
                    .current()
                    .var(idx)
                    .expect("index_of answered with a live position")
                    .ty;
                Ok(type_name(ty))
            }
            // `` `:format varname' ``.
            "format" => {
                let frame = self.ctx.frames.current();
                let Some(idx) = frame.index_of(rest) else {
                    return Err(
                        StataError::new(111, format!("variable {rest} not found")).token(rest)
                    );
                };
                self.ctx.access.read_var_layout = true;
                let f = self
                    .ctx
                    .frames
                    .current()
                    .var(idx)
                    .expect("index_of answered with a live position")
                    .format;
                Ok(stratum_data::variable::format_string(&f))
            }
            // `` `:variable label varname' ``.
            "variable" => {
                let rest = rest.strip_prefix("label").map(str::trim).unwrap_or(rest);
                let frame = self.ctx.frames.current();
                let Some(idx) = frame.index_of(rest) else {
                    return Err(
                        StataError::new(111, format!("variable {rest} not found")).token(rest)
                    );
                };
                self.ctx.access.read_var_layout = true;
                Ok(self
                    .ctx
                    .frames
                    .current()
                    .var(idx)
                    .expect("index_of answered with a live position")
                    .label
                    .to_string())
            }
            // `` `:di %fmt exp' `` — format an expression the way `display`
            // would. The format goes through the workspace's one `%g`.
            "di" | "display" => {
                let (fmt, exp) = match rest.strip_prefix('%') {
                    Some(tail) => match tail.find(char::is_whitespace) {
                        Some(i) => (Some(format!("%{}", &tail[..i])), tail[i..].trim()),
                        None => (Some(format!("%{tail}")), ""),
                    },
                    None => (None, rest),
                };
                let text = self.eval_expr_to_macro_text(exp)?;
                let Some(fmt) = fmt else { return Ok(text) };
                let Ok(parsed) = stratum_core::fmt::StataFormat::parse(&fmt) else {
                    return Err(StataError::new(120, format!("invalid format {fmt}")).token(fmt));
                };
                let x: f64 = text
                    .trim()
                    .parse()
                    .unwrap_or(stratum_core::missing::SYSMISS);
                Ok(parsed.format_f64(x).trim().to_owned())
            }
            // `` `:pwd' `` — through the recorded door, never `std::env`.
            "pwd" => Ok(self.ctx.cwd.to_string()),
            // `` `:env NAME' `` — recorded as an ambient read, because a block
            // that consults the environment is not reproducible from its source.
            "env" | "environment" => Ok(self.ctx.env(rest).unwrap_or_default()),
            // `` `:rowsof' ``/`` `:colsof' `` need matrices, which are v1.5.
            other => Err(StataError::new(
                10,
                format!("unsupported in this version: extended macro function `:{other}'"),
            )
            .token(other.to_owned())),
        }
    }
}

/// A storage type as `describe` and `` `:type' `` spell it.
///
/// Deliberately local and deliberately small. `StorageType` is declared in
/// `stratum-proto` (A10) and carries no `Display`, because "what a human calls
/// this type" is a rendering decision and proto renders nothing. The `str#`
/// arm is why this cannot be a `&'static str` table.
fn type_name(ty: StorageType) -> String {
    match ty {
        StorageType::Byte => "byte".to_owned(),
        StorageType::Int => "int".to_owned(),
        StorageType::Long => "long".to_owned(),
        StorageType::Float => "float".to_owned(),
        StorageType::Double => "double".to_owned(),
        StorageType::Str { width } => format!("str{width}"),
        StorageType::StrL => "strL".to_owned(),
    }
}

impl ExecCtx<'_> {
    /// Expand one logical line's code, re-entering the interpreter for
    /// `` `=exp' `` and the state-dependent extended macro functions.
    ///
    /// # Errors
    ///
    /// Whatever expansion or a callback raised — `r(198)` for a malformed
    /// reference, `r(111)` for a name a callback could not resolve.
    pub fn expand_line(&mut self, code: &str) -> Result<Expansion, StataError> {
        self.counters.expansions += 1;
        // See the module header: `expand` needs `&mut MacroEnv` and
        // `&mut dyn ExpandHost` at once, and in this crate both are reachable
        // only through `self`.
        let mut env = std::mem::take(&mut self.macros);
        let out = {
            let mut host = RuntimeExpandHost::new(self);
            expand(code, &mut env, &mut host)
        };
        self.macros = env;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::{NoHost, Transcript};
    use stratum_data::StorageType;

    struct Fixture {
        out: Transcript,
        host: NoHost,
    }

    fn fixture() -> Fixture {
        Fixture {
            out: Transcript::new(),
            host: NoHost,
        }
    }

    #[test]
    fn a_line_with_no_macro_in_it_comes_back_unchanged() {
        let mut f = fixture();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        let e = ctx.expand_line("summarize price").unwrap();
        assert_eq!(e.text, "summarize price");
        assert_eq!(e.stats.substitutions, 0);
        assert_eq!(e.stats.host_calls, 0);
    }

    #[test]
    fn an_eq_exp_reference_re_enters_the_evaluator() {
        let mut f = fixture();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        ctx.macros.set_local("i", "3");
        let e = ctx.expand_line("display `=`i' * 2'").unwrap();
        assert_eq!(e.text, "display 6");
        assert_eq!(ctx.counters.host_callbacks, 1);
    }

    #[test]
    fn eq_exp_uses_the_workspace_percent_g_and_not_rusts_float_printer() {
        // 02 §4.4: `%18.0g` then trim. `f64::to_string` would print
        // `0.30000000000000004` here, which is a different macro and therefore
        // a different command line.
        let mut f = fixture();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        let e = ctx.expand_line("display `=.1 + .2'").unwrap();
        assert_eq!(e.text, "display .3");
    }

    #[test]
    fn the_type_extended_function_reads_the_live_dataset() {
        let mut f = fixture();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        ctx.frames.current_mut().set_n_obs(2);
        ctx.frames
            .current_mut()
            .add_var("price", StorageType::Int)
            .unwrap();
        let e = ctx.expand_line("display \"`:type price'\"").unwrap();
        assert_eq!(e.text, "display \"int\"");
        assert!(
            ctx.access.read_var_layout,
            "reading a type is a layout read"
        );
    }

    #[test]
    fn an_unresolvable_extended_function_names_what_it_could_not_do() {
        let mut f = fixture();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        let err = ctx.expand_line("display \"`:rowsof M'\"").unwrap_err();
        assert_eq!(err.rc, 10, "rc 10 is ours: incomplete, not wrong");
        assert_eq!(err.offending_token.as_deref(), Some("rowsof"));
    }

    #[test]
    fn the_macro_environment_survives_a_callback() {
        // `expand_line` moves the environment out for the duration of the call;
        // if it forgot to put it back, this would be the test that noticed.
        let mut f = fixture();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        ctx.macros.set_local("i", "3");
        ctx.expand_line("display `=`i' * 2'").unwrap();
        assert_eq!(ctx.macros.local("i"), Some("3"));
    }
}
