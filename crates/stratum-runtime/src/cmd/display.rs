//! `display` — spec §1's "substitute for a hand calculator", and the command
//! that pins how every scalar in the product is rendered.
//!
//! # `%10.0g`, measured, not documented
//!
//! Stata's manual says `display` shows a numeric result in `%9.0g`. The machine
//! disagrees, and the machine is the authority here (project rule): the golden
//! `di "\`v' mean = " r(mean)` prints `6165.2568` for `6165.25675675676`, which
//! is nine significant digits — `%9.0g` gives `6165.257`, `%10.0g` gives
//! `6165.2568`. `di r(mean)` on `mpg` prints `21.297297`, again `%10.0g`. So
//! [`DEFAULT_WIDTH`] is 10, and both goldens are in `tests/cmd_surface.rs`.
//!
//! Leading blanks are stripped for the DEFAULT format and kept for an explicit
//! one: `di 2 + 3 * 4` prints `14`, not `        14`, while `di %9.2f x` is a
//! request for a nine-column field.
//!
//! # Items concatenate with no separator
//!
//! `display` takes a sequence of items and joins them with nothing at all —
//! `di "a" "b"` is `ab`. A non-quoted, non-directive run is taken as ONE
//! expression as far as the next item boundary, which is what makes
//! `di 2 + 3 * 4` a single evaluation rather than three.

use stratum_core::fmt::StataFormat;
use stratum_core::Value;
use stratum_parse::ast::CommandAst;
use stratum_parse::lex::LexMode;
use stratum_parse::parse::{parse_expr, Cursor, ParseMode};
use stratum_parse::StataError;
use stratum_proto::{Span, StyleId};

use super::{err, rest, rest_span, CmdHost, CmdOutcome, CmdResult, Out};

/// The width `display` formats a bare numeric result at. See the module note.
pub const DEFAULT_WIDTH: usize = 10;

/// `display [item [item ...]]`.
pub fn display(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let text = rest(ast);
    let span = rest_span(ast);
    let runs = render(host, text, span)?;
    host.emit(&runs);
    Ok(CmdOutcome::text_only())
}

/// Render one `display` argument list.
///
/// Public because `noisily display` and the `di` in a `program` body go through
/// exactly this, and because the tests drive it without building an AST.
///
/// # Errors
///
/// Whatever evaluating one of the expressions returns.
pub fn render(
    host: &mut dyn CmdHost,
    text: &str,
    span: Span,
) -> Result<Vec<stratum_proto::StyledRun>, StataError> {
    let mut out = Out::new();
    let mut style = StyleId::Result;
    let mut fmt: Option<StataFormat> = None;
    let mut advance = true;
    let mut items = Items::new(text);

    while let Some(item) = items.next_item(span)? {
        match item {
            Item::Str(s) => out.push(style, &s),
            Item::Style(st) => style = st,
            Item::Format(f) => fmt = Some(f),
            Item::Newline(n) => {
                for _ in 0..n {
                    out.push(StyleId::Text, "\n");
                }
            }
            Item::Spaces(n) => out.push(style, &" ".repeat(n)),
            Item::Column(c) => {
                // `_col(#)` moves to column # of the CURRENT line.
                let line = out.to_plain();
                let at = line.rsplit('\n').next().map_or(0, str::len);
                out.push(style, &" ".repeat(c.saturating_sub(at + 1)));
            }
            Item::NoAdvance => advance = false,
            Item::Expr(src, at) => {
                let v = eval_text(host, &src, at)?;
                out.push(style, &render_value(&v, fmt.take().as_ref()));
            }
        }
    }
    if advance {
        out.push(StyleId::Text, "\n");
    }
    Ok(out.into_runs())
}

/// One value, in the format `display` would use.
#[must_use]
pub fn render_value(v: &Value, fmt: Option<&StataFormat>) -> String {
    match (v, fmt) {
        (Value::Real(x), Some(f)) => f.format_f64(*x),
        (Value::Real(x), None) => stratum_core::fmt::fmt_g(*x, DEFAULT_WIDTH)
            .trim_start()
            .to_owned(),
        (Value::Str(s), Some(f)) => f.format_str(s),
        (Value::Str(s), None) => s.clone(),
    }
}

/// Parse and evaluate one expression written as text.
fn eval_text(host: &mut dyn CmdHost, src: &str, span: Span) -> Result<Value, StataError> {
    let toks = stratum_parse::tokens(src, LexMode::Expanded);
    let mut cur = Cursor::new(src, &toks, ParseMode::Execute);
    let e = parse_expr(&mut cur);
    host.eval_scalar(&e).map_err(|err| match err.span {
        Some(_) => err,
        None => err.at(span),
    })
}

/// One `display` item.
enum Item {
    Str(String),
    Style(StyleId),
    Format(StataFormat),
    Newline(usize),
    Spaces(usize),
    Column(usize),
    NoAdvance,
    Expr(String, Span),
}

/// The item reader.
struct Items<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Items<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn skip_ws(&mut self) {
        while self.rest().starts_with([' ', '\t']) {
            self.pos += 1;
        }
    }

    fn next_item(&mut self, span: Span) -> Result<Option<Item>, StataError> {
        self.skip_ws();
        if self.rest().is_empty() {
            return Ok(None);
        }
        // Compound double quotes come first: `"…"' may contain plain quotes.
        if self.rest().starts_with("`\"") {
            return Ok(Some(Item::Str(self.compound()?)));
        }
        if self.rest().starts_with('"') {
            return Ok(Some(Item::Str(self.simple_quote())));
        }
        if self.rest().starts_with('%') {
            let word = self.word();
            let f = StataFormat::parse(word).map_err(|_| err::invalid(word).at(span))?;
            return Ok(Some(Item::Format(f)));
        }
        if let Some(item) = self.directive(span)? {
            return Ok(Some(item));
        }
        // Everything else, up to the next quoted string or directive, is ONE
        // expression: `di 2 + 3 * 4` must evaluate as a whole.
        let start = self.pos;
        while !self.rest().is_empty() {
            let save = self.pos;
            self.skip_ws();
            if self.rest().starts_with('"')
                || self.rest().starts_with("`\"")
                || self.peek_directive()
                || self.rest().starts_with('%')
            {
                self.pos = save;
                break;
            }
            if self.rest().is_empty() {
                break;
            }
            self.pos = save;
            self.advance_expr_token();
        }
        let text = self.src[start..self.pos].trim().to_owned();
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(Item::Expr(text, span)))
    }

    /// Consume one whitespace-delimited chunk of an expression, keeping
    /// parentheses and quoted substrings together.
    fn advance_expr_token(&mut self) {
        self.skip_ws();
        let mut depth = 0usize;
        while let Some(c) = self.rest().chars().next() {
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' => depth = depth.saturating_sub(1),
                '"' => {
                    self.pos += 1;
                    while !self.rest().is_empty() && !self.rest().starts_with('"') {
                        self.pos += 1;
                    }
                }
                ' ' | '\t' if depth == 0 => return,
                _ => {}
            }
            self.pos += c.len_utf8();
        }
    }

    fn word(&mut self) -> &'a str {
        self.skip_ws();
        let start = self.pos;
        while !self.rest().is_empty() && !self.rest().starts_with([' ', '\t']) {
            self.pos += 1;
        }
        &self.src[start..self.pos]
    }

    fn simple_quote(&mut self) -> String {
        self.pos += 1;
        let start = self.pos;
        while !self.rest().is_empty() && !self.rest().starts_with('"') {
            self.pos += 1;
        }
        let text = self.src[start..self.pos].to_owned();
        if !self.rest().is_empty() {
            self.pos += 1;
        }
        text
    }

    /// `` `"…"' `` — may nest, so the closer is matched by depth.
    fn compound(&mut self) -> Result<String, StataError> {
        self.pos += 2;
        let start = self.pos;
        let mut depth = 1usize;
        while !self.rest().is_empty() {
            if self.rest().starts_with("`\"") {
                depth += 1;
                self.pos += 2;
                continue;
            }
            if self.rest().starts_with("\"'") {
                depth -= 1;
                if depth == 0 {
                    let text = self.src[start..self.pos].to_owned();
                    self.pos += 2;
                    return Ok(text);
                }
                self.pos += 2;
                continue;
            }
            self.pos += self.rest().chars().next().map_or(1, char::len_utf8);
        }
        Ok(self.src[start..].to_owned())
    }

    fn peek_directive(&self) -> bool {
        let r = self.rest();
        let word = r.split([' ', '\t']).next().unwrap_or("");
        matches!(
            word,
            "_n" | "_newline" | "_c" | "_continue" | "_asis" | "_quote"
        ) || word.starts_with("_skip(")
            || word.starts_with("_col(")
            || word.starts_with("_dup(")
            || word.starts_with("_newline(")
            || word.starts_with("_char(")
            || word == "as"
    }

    /// The next item, when it is a directive rather than a string or an
    /// expression.
    ///
    /// # Errors
    ///
    /// **rc 10** for a directive [`Items::peek_directive`] recognises and this
    /// build does not implement. That pairing is deliberate: the two functions
    /// have to agree, and the only way to make a disagreement loud is for the
    /// unhandled arm to raise rather than to emit nothing. `di "x" _dup(3)`
    /// silently printing `x` is a wrong answer in a differentially-tested
    /// product; `unsupported in this version` is a true one.
    fn directive(&mut self, span: Span) -> Result<Option<Item>, StataError> {
        if !self.peek_directive() {
            return Ok(None);
        }
        let word = self.word();
        if word == "as" {
            let which = self.word();
            return Ok(Some(Item::Style(match which {
                "text" | "txt" => StyleId::Text,
                "error" | "err" => StyleId::Error,
                "input" | "inp" => StyleId::Input,
                // `as result` and anything unrecognised fall back to result,
                // which is what an unstyled `display` already uses.
                _ => StyleId::Result,
            })));
        }
        Ok(Some(match word {
            "_n" | "_newline" => Item::Newline(1),
            "_c" | "_continue" => Item::NoAdvance,
            "_asis" | "_quote" => Item::Str(String::new()),
            w => {
                let arg = paren_arg(w).unwrap_or(1);
                if w.starts_with("_skip(") {
                    Item::Spaces(arg)
                } else if w.starts_with("_col(") {
                    Item::Column(arg)
                } else if w.starts_with("_newline(") {
                    Item::Newline(arg)
                } else if w.starts_with("_char(") {
                    Item::Str(
                        char::from_u32(arg as u32)
                            .map(String::from)
                            .unwrap_or_default(),
                    )
                } else {
                    // `_dup(#)` is the one directive `peek_directive` claims
                    // and this arm does not implement. It repeats the PREVIOUS
                    // item, which the item reader cannot do without keeping
                    // one; saying so is better than printing the item once.
                    return Err(err::unsupported(&format!("display directive {w}"))
                        .token(w)
                        .at(span));
                }
            }
        }))
    }
}

fn paren_arg(word: &str) -> Option<usize> {
    let open = word.find('(')?;
    let close = word.rfind(')')?;
    word.get(open + 1..close)?.trim().parse().ok()
}
