//! Extended macro functions — design 02 §4.3, the text-only half.
//!
//! `` `: word count a b c' `` is `3` [V]. `` `: word 2 of one two three' `` is
//! `two` [V] — both from `tests/golden/stata18/semantics.log`.
//!
//! # Why this file only answers some of them
//!
//! An extended macro function is either a string operation over macro text —
//! `word`, `list`, `length`, `subinstr`, `piece` — or a question about live
//! state: `type price` needs the dataset, `di %9.4f 1/3` needs the expression
//! evaluator, `pwd` needs the file system a wasm build does not have.
//!
//! [`eval`] answers the first group and returns `None` for the second, which
//! `expand` then routes to [`crate::macros::ExpandHost::eval_xmf`]. That split
//! is what lets the whole macro test suite run with a mock host and no engine,
//! and it keeps the file-system functions out of a crate that ARCHITECTURE §8.4
//! requires to build for `wasm32-unknown-unknown`.

use crate::macros::env::MacroEnv;
use crate::macros::StataError;

/// Evaluate the body of a `` `:…' `` reference.
///
/// `None` means "this one needs the runtime", not "this one failed".
pub fn eval(body: &str, env: &MacroEnv) -> Option<Result<String, StataError>> {
    let body = body.trim();
    let (head, rest) = split_word(body);
    match head {
        "word" => Some(word(rest)),
        "list" => Some(list(rest, env)),
        "length" => length(rest, env),
        "subinstr" => subinstr(rest, env),
        "piece" => Some(piece(rest)),
        _ => None,
    }
}

/// `word count <text>` and `word # of <text>`.
fn word(rest: &str) -> Result<String, StataError> {
    let (op, tail) = split_word(rest);
    if op == "count" {
        return Ok(tokenize(tail).len().to_string());
    }
    let n: i64 = op
        .parse()
        .map_err(|_| StataError::new(198, "invalid syntax in `word'"))?;
    let (of, text) = split_word(tail);
    if of != "of" {
        return Err(StataError::new(198, "invalid syntax in `word'"));
    }
    let words = tokenize(text);
    // Out of range is the empty string, not an error [U] 18.3.
    if n < 1 || n as usize > words.len() {
        return Ok(String::new());
    }
    Ok(words[n as usize - 1].to_owned())
}

/// The macro-list functions of [P] macro lists.
fn list(rest: &str, env: &MacroEnv) -> Result<String, StataError> {
    let (op, tail) = split_word(rest);
    match op {
        "sizeof" => Ok(elems(tail.trim(), env).len().to_string()),
        "uniq" | "clean" => {
            let mut seen: Vec<String> = Vec::new();
            for e in elems(tail.trim(), env) {
                if !seen.contains(&e) {
                    seen.push(e);
                }
            }
            Ok(seen.join(" "))
        }
        "dups" => {
            let mut seen: Vec<String> = Vec::new();
            let mut dup: Vec<String> = Vec::new();
            for e in elems(tail.trim(), env) {
                if seen.contains(&e) {
                    dup.push(e);
                } else {
                    seen.push(e);
                }
            }
            Ok(dup.join(" "))
        }
        "sort" => {
            let mut v = elems(tail.trim(), env);
            // Stata's macro-list sort is byte-wise ascending, the same order the
            // expression `<` uses on strings (02 §8.2) — `"cat" > "Zebra"`.
            v.sort_unstable();
            Ok(v.join(" "))
        }
        "retokenize" => Ok(elems(tail.trim(), env).join(" ")),
        _ => set_op(rest, env),
    }
}

/// `list A | B`, `A & B`, `A - B`, `A in B`.
fn set_op(rest: &str, env: &MacroEnv) -> Result<String, StataError> {
    for (sym, kind) in [("|", 0u8), ("&", 1), ("-", 2), (" in ", 3)] {
        if let Some((l, r)) = rest.split_once(sym) {
            let a = elems(l.trim(), env);
            let b = elems(r.trim(), env);
            return Ok(match kind {
                0 => {
                    // A union is a SET: `list A | B` with `A` holding a
                    // duplicate must not carry it through.
                    let mut out: Vec<String> = Vec::new();
                    for e in a.into_iter().chain(b) {
                        if !out.contains(&e) {
                            out.push(e);
                        }
                    }
                    out.join(" ")
                }
                // Intersection and difference are sets too: `A & B` where `A`
                // repeats an element must not repeat it in the answer.
                1 => dedup(a.into_iter().filter(|e| b.contains(e))).join(" "),
                2 => dedup(a.into_iter().filter(|e| !b.contains(e))).join(" "),
                _ => u8::from(a.iter().all(|e| b.contains(e))).to_string(),
            });
        }
    }
    Err(StataError::new(198, "invalid macro list operation"))
}

/// `length local NAME` / `length global NAME`. Returns `None` for
/// `length varname`, which needs the dataset.
fn length(rest: &str, env: &MacroEnv) -> Option<Result<String, StataError>> {
    let (kind, name) = split_word(rest);
    let name = name.trim();
    let v = match kind {
        "local" => env.local(name).unwrap_or(""),
        "global" => env.global(name).unwrap_or(""),
        _ => return None,
    };
    // Stata counts BYTES here, not characters: `length local` is the storage
    // length, which is what a `str#` width has to be computed from.
    Some(Ok(v.len().to_string()))
}

/// `subinstr local NAME "from" "to" [, all count word]`.
fn subinstr(rest: &str, env: &MacroEnv) -> Option<Result<String, StataError>> {
    let (kind, tail) = split_word(rest);
    let (name, tail) = split_word(tail);
    let subject = match kind {
        "local" => env.local(name.trim()).unwrap_or("").to_owned(),
        "global" => env.global(name.trim()).unwrap_or("").to_owned(),
        _ => return None,
    };
    let (args, opts) = match tail.split_once(',') {
        Some((a, o)) => (a, o.trim()),
        None => (tail, ""),
    };
    let Some((from, to)) = two_quoted(args) else {
        return Some(Err(StataError::new(198, "invalid syntax in `subinstr'")));
    };
    let all = opts
        .split_whitespace()
        .any(|o| "all".starts_with(o) && !o.is_empty());
    let count = opts
        .split_whitespace()
        .any(|o| "count".starts_with(o) && !o.is_empty());
    let word_mode = opts
        .split_whitespace()
        .any(|o| "word".starts_with(o) && !o.is_empty());

    if word_mode {
        let mut hits = 0usize;
        let out: Vec<String> = subject
            .split_whitespace()
            .map(|w| {
                if w == from && (all || hits == 0) {
                    hits += 1;
                    to.to_owned()
                } else {
                    w.to_owned()
                }
            })
            .collect();
        return Some(Ok(if count {
            hits.to_string()
        } else {
            out.join(" ")
        }));
    }
    if from.is_empty() {
        return Some(Ok(if count { "0".to_owned() } else { subject }));
    }
    let hits = subject.matches(from).count();
    if count {
        return Some(Ok(hits.to_string()));
    }
    Some(Ok(if all {
        subject.replace(from, to)
    } else {
        subject.replacen(from, to, 1)
    }))
}

/// `piece # # of "text"` — the `#`th chunk of at most `#` characters, split on
/// word boundaries ([P] macro).
fn piece(rest: &str) -> Result<String, StataError> {
    let (n, tail) = split_word(rest);
    let (w, tail) = split_word(tail);
    let (of, text) = split_word(tail);
    if of != "of" {
        return Err(StataError::new(198, "invalid syntax in `piece'"));
    }
    let (n, w): (usize, usize) = match (n.parse(), w.parse()) {
        (Ok(a), Ok(b)) if b > 0 => (a, b),
        _ => return Err(StataError::new(198, "invalid syntax in `piece'")),
    };
    let text = strip_quotes(text.trim());
    let mut pieces: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur.chars().count() + 1 + word.chars().count() <= w {
            cur.push(' ');
            cur.push_str(word);
        } else {
            pieces.push(std::mem::take(&mut cur));
            cur.push_str(word);
        }
    }
    if !cur.is_empty() {
        pieces.push(cur);
    }
    Ok(pieces.get(n.wrapping_sub(1)).cloned().unwrap_or_default())
}

fn dedup(it: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in it {
        if !out.contains(&e) {
            out.push(e);
        }
    }
    out
}

/// Split off the first whitespace-delimited word.
fn split_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

/// The elements of a macro-list argument: either a local's contents (a bare
/// name) or a literal list.
fn elems(spec: &str, env: &MacroEnv) -> Vec<String> {
    let spec = spec.trim();
    if !spec.is_empty() && crate::lex::is_name(spec) {
        if let Some(v) = env.local(spec) {
            return tokenize(v).into_iter().map(str::to_owned).collect();
        }
    }
    tokenize(spec).into_iter().map(str::to_owned).collect()
}

/// Whitespace-split, honouring `"…"` as one token.
fn tokenize(s: &str) -> Vec<&str> {
    crate::macros::env::split_args(s)
}

fn strip_quotes(s: &str) -> &str {
    crate::lex::unquote(s)
}

/// Read two `"…"` arguments from `s`.
fn two_quoted(s: &str) -> Option<(&str, &str)> {
    let mut it = crate::macros::env::split_args(s).into_iter();
    let a = it.next()?;
    let b = it.next().unwrap_or("");
    Some((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(body: &str, env: &MacroEnv) -> String {
        eval(body, env).expect("handled here").expect("no error")
    }

    #[test]
    fn word_matches_the_golden() {
        let env = MacroEnv::new();
        // tests/golden/stata18/semantics.log, verbatim.
        assert_eq!(ev(" word count one two three", &env), "3");
        assert_eq!(ev(" word 2 of one two three", &env), "two");
        assert_eq!(ev(" word count a b c", &env), "3");
        // Out of range is empty, not an error.
        assert_eq!(ev(" word 9 of one two", &env), "");
    }

    #[test]
    fn list_ops() {
        let mut env = MacroEnv::new();
        env.set_local("A", "a b c b");
        env.set_local("B", "b d");
        assert_eq!(ev(" list sizeof A", &env), "4");
        assert_eq!(ev(" list uniq A", &env), "a b c");
        assert_eq!(ev(" list dups A", &env), "b");
        assert_eq!(ev(" list sort A", &env), "a b b c");
        assert_eq!(ev(" list A | B", &env), "a b c d");
        assert_eq!(ev(" list A & B", &env), "b");
        assert_eq!(ev(" list A - B", &env), "a c");
    }

    #[test]
    fn length_and_subinstr() {
        let mut env = MacroEnv::new();
        env.set_local("s", "aaa");
        assert_eq!(ev(" length local s", &env), "3");
        assert_eq!(ev(r#" subinstr local s "a" "b", all"#, &env), "bbb");
        assert_eq!(ev(r#" subinstr local s "a" "b""#, &env), "baa");
        assert_eq!(ev(r#" subinstr local s "a" "b", count"#, &env), "3");
    }

    #[test]
    fn state_dependent_functions_go_to_the_host() {
        let env = MacroEnv::new();
        assert!(eval(" type price", &env).is_none());
        assert!(eval(" di %9.4f 1/3", &env).is_none());
        assert!(eval(" pwd", &env).is_none());
        assert!(eval(" length varname price", &env).is_none());
    }

    #[test]
    fn piece_splits_on_word_boundaries() {
        let env = MacroEnv::new();
        assert_eq!(ev(r#" piece 1 5 of "this is it""#, &env), "this");
        assert_eq!(ev(r#" piece 2 5 of "this is it""#, &env), "is it");
    }
}
