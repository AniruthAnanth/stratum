//! The log normalizer for the LIVE half: volatile content that must never
//! count as a difference, driven by `tests/difftest/normalize.rules`.
//!
//! The corpus channel never comes here — it is byte-exact over blocks that
//! contain nothing volatile, and a normalizer in that path would hide real
//! bugs. The live half compares whole fresh logs whose banner, absolute paths
//! and timestamps genuinely differ per machine, and the committed corpus's
//! banner additionally carries `[redacted]` licence lines; the rules file
//! makes both shapes converge on the same normalized text.
//!
//! # The rules file
//!
//! Plain lines, one rule each, applied per log line in file order:
//!
//! ```text
//! # comment / blank — ignored
//! drop /REGEX/              delete every line matching REGEX
//! sub  /REGEX/ REPLACEMENT  replace every match with REPLACEMENT
//! ```
//!
//! A data file rather than code so that a future capture machine can teach
//! the harness a new banner shape without recompiling — the exact move the
//! `[redacted]` redaction already forced once.

use anyhow::{bail, Context, Result};
use regex::Regex;

/// One parsed rule.
#[derive(Debug)]
enum Rule {
    Drop(Regex),
    Sub(Regex, String),
}

/// The compiled rule set.
#[derive(Debug)]
pub struct Rules {
    rules: Vec<Rule>,
}

impl Rules {
    /// Parse a rules file.
    ///
    /// # Errors
    /// Malformed lines and invalid regexes, with their line number.
    pub fn parse(text: &str) -> Result<Rules> {
        let mut rules = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let n = i + 1;
            if let Some(rest) = line.strip_prefix("drop ") {
                let re = take_pattern(rest.trim())
                    .with_context(|| format!("normalize.rules:{n}: bad drop pattern"))?;
                if !re.1.trim().is_empty() {
                    bail!("normalize.rules:{n}: drop takes no replacement");
                }
                rules.push(Rule::Drop(re.0));
            } else if let Some(rest) = line.strip_prefix("sub ") {
                let (re, tail) = take_pattern(rest.trim())
                    .with_context(|| format!("normalize.rules:{n}: bad sub pattern"))?;
                rules.push(Rule::Sub(re, tail.trim().to_owned()));
            } else {
                bail!("normalize.rules:{n}: expected `drop /re/` or `sub /re/ text`: {line:?}");
            }
        }
        Ok(Rules { rules })
    }

    /// Normalize a whole log: apply every rule to every line, drop the lines
    /// the `drop` rules match, and collapse the result to LF.
    #[must_use]
    pub fn apply(&self, log: &str) -> String {
        let mut out = String::with_capacity(log.len());
        'line: for raw in log.lines() {
            let mut line = raw.trim_end_matches('\r').to_owned();
            for rule in &self.rules {
                match rule {
                    Rule::Drop(re) => {
                        if re.is_match(&line) {
                            continue 'line;
                        }
                    }
                    Rule::Sub(re, rep) => {
                        if re.is_match(&line) {
                            line = re.replace_all(&line, rep.as_str()).into_owned();
                        }
                    }
                }
            }
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

/// `/pattern/ tail` → (compiled pattern, tail). A `\/` inside the pattern is
/// an escaped slash, not the terminator.
fn take_pattern(s: &str) -> Result<(Regex, &str)> {
    let Some(rest) = s.strip_prefix('/') else {
        bail!("pattern must be /…/-delimited: {s:?}");
    };
    let bytes = rest.as_bytes();
    let mut i = 0usize;
    let mut end = None;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'/' => {
                end = Some(i);
                break;
            }
            _ => i += 1,
        }
    }
    let Some(end) = end else {
        bail!("unterminated pattern: {s:?}");
    };
    let re = Regex::new(&rest[..end]).with_context(|| format!("regex {:?}", &rest[..end]))?;
    Ok((re, &rest[end + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULES: &str = "\
# licence lines — including the corpus's redacted form\n\
drop /^\\s*(Stata license|Serial number|  Licensed to):/\n\
drop /^\\s*\\[redacted\\]\\s*$/\n\
sub  /\\d{1,2} [A-Z][a-z]{2} \\d{4}[, ]+\\d{2}:\\d{2}(:\\d{2})?/ <TIMESTAMP>\n\
sub  /(\\/(Applications|Users|private|tmp|home|var)\\/)\\S+/ <PATH>\n";

    #[test]
    fn redacted_and_live_banners_normalize_identically() {
        let r = Rules::parse(RULES).expect("rules parse");
        let redacted = "Stata license: [redacted]\nSerial number: [redacted]\n\
                        [redacted]\nnote kept\n";
        let live = "Stata license: Single-user 4-core\nSerial number: 501809\n\
                    note kept\n";
        assert_eq!(r.apply(redacted), r.apply(live));
        assert_eq!(r.apply(redacted), "note kept\n");
    }

    #[test]
    fn paths_and_timestamps_never_count_as_differences() {
        let r = Rules::parse(RULES).expect("rules parse");
        let a = r.apply("log opened 22 Aug 2026, 13:45:12 in /Users/alice/work/x\n");
        let b = r.apply("log opened 3 Jan 2027, 09:01:02 in /home/bob/proj/x\n");
        assert_eq!(a, b);
    }

    #[test]
    fn malformed_rules_are_an_error_with_a_line_number() {
        let e = Rules::parse("frob /x/\n").expect_err("bad verb");
        assert!(e.to_string().contains(":1:"), "{e}");
    }
}
