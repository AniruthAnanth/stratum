//! Strict parsers for the structured surfaces.
//!
//! # Containment is enforced here, not requested in the prompt
//!
//! `07` §10 says a drafted repro fix is "rejected wholesale if it touches any
//! line outside the ones the deterministic checks cited. That containment rule
//! is enforced in `ai-tasks::parse`, not requested in the prompt." This module
//! is that sentence. A prompt instruction is a preference; [`patch`] is a
//! predicate, and it fails the whole patch rather than dropping the offending
//! edit — a partially applied fix is a file in a state nobody designed.
//!
//! # Everything here assumes the model is hostile
//!
//! Not because the model is, but because a `.do` file, a variable label or a
//! dataset note can be, and prompt injection is not solved by delimiters (`07`
//! §5.5). The structural answers are that the model has no tools and that
//! nothing here can construct an edit to executable code: [`comments`] yields
//! text that has passed [`ProposedComment::validate`], and [`patch`] yields
//! lines the deterministic layer already named.

use super::task::{CommentAnchor, CommentRejection, ProposedComment, ProposedEdit, ProposedPatch};

/// A reply that could not be understood.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    /// No JSON object was found in the reply at all.
    #[error("the reply contained no JSON object")]
    NoJson,
    /// It was JSON, but not the shape asked for.
    #[error("the reply did not match the expected shape: {0}")]
    Shape(String),
    /// A drafted patch reached a line the deterministic checks never cited.
    #[error("the drafted fix would edit line {line}, which no check cited; the whole patch was rejected")]
    OutOfScope {
        /// The offending line.
        line: u32,
    },
}

/// Find the JSON object in a reply that may be wrapped in prose or a fence.
///
/// Models emit ```` ```json ```` fences, leading apologies and trailing
/// explanations. Scanning for the first balanced `{ … }` outside a string is a
/// dozen lines and removes an entire class of "it worked in testing" failure,
/// which is worth more than insisting the model behave.
#[must_use]
pub fn extract_json(reply: &str) -> Option<&str> {
    let bytes = reply.as_bytes();
    let start = reply.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for i in start..bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&reply[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The `{"comments": [...]}` payload.
///
/// Returns the comments that survived and the rejections that did not, because
/// the UI's disclosure is "3 of 18 comments could not be safely placed" — a
/// count the user can see, not a silent drop.
///
/// # Errors
/// [`ParseError`] when the reply is not the expected shape at all. A single bad
/// entry inside a well-formed array is a rejection, not an error: one malformed
/// comment must not throw away seventeen good ones.
pub fn comments(
    reply: &str,
    anchors: &[CommentAnchor],
) -> Result<(Vec<ProposedComment>, Vec<CommentRejection>), ParseError> {
    let json = extract_json(reply).ok_or(ParseError::NoJson)?;
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| ParseError::Shape(e.to_string()))?;
    let array = value
        .get("comments")
        .and_then(|c| c.as_array())
        .ok_or_else(|| ParseError::Shape("no `comments` array".to_owned()))?;

    let mut kept = Vec::new();
    let mut rejected = Vec::new();
    for entry in array {
        let Ok(c) = serde_json::from_value::<ProposedComment>(entry.clone()) else {
            // A malformed entry is indistinguishable from a comment we refused;
            // reporting it as "empty" keeps the disclosure honest without
            // inventing a rejection reason for a shape we never saw.
            rejected.push(CommentRejection::Empty);
            continue;
        };
        match c.validate(anchors) {
            Ok(()) => kept.push(c),
            Err(r) => rejected.push(r),
        }
    }
    Ok((kept, rejected))
}

/// The `{"edits": [...]}` payload, confined to the cited lines.
///
/// # Errors
/// [`ParseError::OutOfScope`] when any edit names a line the deterministic
/// checks did not cite. The whole patch fails: a fix that half-applied is worse
/// than one that did not.
pub fn patch(reply: &str, cited_lines: &[u32]) -> Result<ProposedPatch, ParseError> {
    let json = extract_json(reply).ok_or(ParseError::NoJson)?;
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| ParseError::Shape(e.to_string()))?;
    let array = value
        .get("edits")
        .and_then(|c| c.as_array())
        .ok_or_else(|| ParseError::Shape("no `edits` array".to_owned()))?;

    let mut edits: Vec<ProposedEdit> = Vec::with_capacity(array.len());
    for entry in array {
        let e: ProposedEdit = serde_json::from_value(entry.clone())
            .map_err(|err| ParseError::Shape(err.to_string()))?;
        if !cited_lines.contains(&e.line) {
            return Err(ParseError::OutOfScope { line: e.line });
        }
        // A "replacement line" that is several lines is a rewrite wearing a
        // replacement's clothes, and it would silently change the file's
        // statement count.
        if e.replacement.contains(['\n', '\r']) {
            return Err(ParseError::Shape(format!(
                "line {} replacement spans lines",
                e.line
            )));
        }
        edits.push(e);
    }
    edits.sort_by_key(|e| e.line);
    edits.dedup_by_key(|e| e.line);
    Ok(ProposedPatch { edits })
}

/// A cleaned-up history block.
#[derive(Clone, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CleanedHistory {
    /// The block, in execution order, one statement per entry.
    pub lines: Vec<String>,
    /// Everything from the history that did not make it, and why.
    #[serde(default)]
    pub dropped: Vec<DroppedCommand>,
}

/// One command the cleanup left out.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct DroppedCommand {
    /// The command, verbatim.
    pub command: String,
    /// Why it was left out.
    pub why: String,
}

/// The `{"lines": [...], "dropped": [...]}` payload.
///
/// # Errors
/// [`ParseError`] when the reply is not that shape, or when a "line" spans more
/// than one line — a do-file block whose statement count does not match its
/// entry count is not the thing the user asked to review.
pub fn history(reply: &str) -> Result<CleanedHistory, ParseError> {
    let json = extract_json(reply).ok_or(ParseError::NoJson)?;
    let cleaned: CleanedHistory =
        serde_json::from_str(json).map_err(|e| ParseError::Shape(e.to_string()))?;
    if let Some(bad) = cleaned.lines.iter().find(|l| l.contains(['\n', '\r'])) {
        return Err(ParseError::Shape(format!(
            "a block entry spans lines: {bad:?}"
        )));
    }
    Ok(cleaned)
}

/// A ghost completion, stripped of everything a model wraps it in.
///
/// Returns the empty string when there is nothing usable, which the editor
/// renders as no ghost text at all — the correct outcome for "the obvious
/// continuation was not obvious".
#[must_use]
pub fn ghost(reply: &str) -> String {
    let mut text = reply.trim_start_matches('\u{feff}').trim();
    // A fenced block: take the first line of its body.
    if let Some(rest) = text.strip_prefix("```") {
        let body = rest.split_once('\n').map_or("", |(_, b)| b);
        text = body.split("```").next().unwrap_or("").trim_end();
    }
    // A single line only, always: ghost text is one line by construction, and a
    // second line would be an edit the user never saw before accepting it.
    let first = text.lines().next().unwrap_or("").trim_end();
    first.trim_start_matches("stata").trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task::{CommentKind, CommentPosition};

    fn anchors() -> Vec<CommentAnchor> {
        vec![
            CommentAnchor::new(42, "gen ln_price = log(price)"),
            CommentAnchor::new(7, "use auto"),
        ]
    }

    fn reply_with(text: &str) -> String {
        format!(
            r#"Here you go!
```json
{{"comments": [
  {{"anchor_line": 42, "anchor_hash": "{}", "position": "above",
    "text": "{text}", "kind": "explain"}}
]}}
```
Hope that helps."#,
            CommentAnchor::hash_line("gen ln_price = log(price)")
        )
    }

    #[test]
    fn json_is_found_inside_a_fence_and_inside_prose() {
        // Bound, not inlined: `extract_json` borrows from its argument, so the
        // reply has to outlive the slice it returns.
        let reply = reply_with("fine");
        let extracted = extract_json(&reply).unwrap();
        assert!(extracted.starts_with('{') && extracted.ends_with('}'));
        assert!(serde_json::from_str::<serde_json::Value>(extracted).is_ok());
    }

    #[test]
    fn a_brace_inside_a_string_does_not_end_the_object() {
        let src = r#"prose {"text": "a } brace", "n": 1} trailing"#;
        assert_eq!(
            extract_json(src).unwrap(),
            r#"{"text": "a } brace", "n": 1}"#
        );
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        let src = r#"{"text": "he said \"hi\" }", "n": 1}"#;
        assert_eq!(extract_json(src).unwrap(), src);
    }

    #[test]
    fn a_good_comment_is_kept_and_a_dangerous_one_is_rejected_without_losing_the_batch() {
        let good = reply_with("Log-transform price before estimating.");
        let (kept, rejected) = comments(&good, &anchors()).unwrap();
        assert_eq!(kept.len(), 1);
        assert!(rejected.is_empty());
        assert_eq!(kept[0].kind, CommentKind::Explain);
        assert_eq!(kept[0].position, CommentPosition::Above);

        let bad = reply_with("close the block */ drop _all");
        let (kept, rejected) = comments(&bad, &anchors()).unwrap();
        assert!(kept.is_empty());
        assert_eq!(rejected, vec![CommentRejection::ContainsCommentDelimiter]);
    }

    #[test]
    fn one_malformed_entry_does_not_throw_away_the_good_ones() {
        let hash = CommentAnchor::hash_line("use auto");
        let reply = format!(
            r#"{{"comments": [
              {{"nonsense": true}},
              {{"anchor_line": 7, "anchor_hash": "{hash}", "position": "above",
                "text": "Load the example dataset.", "kind": "explain"}}
            ]}}"#
        );
        let (kept, rejected) = comments(&reply, &anchors()).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(rejected.len(), 1);
    }

    #[test]
    fn a_reply_with_no_json_at_all_is_an_error_not_an_empty_batch() {
        assert_eq!(
            comments("I could not do that.", &anchors()),
            Err(ParseError::NoJson)
        );
    }

    #[test]
    fn a_patch_that_reaches_an_uncited_line_is_rejected_whole() {
        // The containment rule. Not a request in the prompt — a predicate.
        let reply = r#"{"edits": [
            {"line": 37, "replacement": "use \"data/w.dta\", clear", "why": "relative"},
            {"line": 99, "replacement": "drop _all", "why": "tidy"}
        ]}"#;
        assert_eq!(
            patch(reply, &[37]),
            Err(ParseError::OutOfScope { line: 99 })
        );
    }

    #[test]
    fn a_contained_patch_is_sorted_and_deduplicated() {
        let reply = r#"{"edits": [
            {"line": 40, "replacement": "b", "why": "y"},
            {"line": 37, "replacement": "a", "why": "x"},
            {"line": 37, "replacement": "a2", "why": "x2"}
        ]}"#;
        let p = patch(reply, &[37, 40]).unwrap();
        assert_eq!(p.edits.len(), 2);
        assert_eq!(p.edits[0].line, 37);
        assert_eq!(p.edits[1].line, 40);
    }

    #[test]
    fn a_multi_line_replacement_is_a_rewrite_and_is_refused() {
        let reply = r#"{"edits": [{"line": 37, "replacement": "a\nb", "why": "x"}]}"#;
        assert!(matches!(patch(reply, &[37]), Err(ParseError::Shape(_))));
    }

    #[test]
    fn a_cleaned_history_keeps_its_order_and_reports_what_it_dropped() {
        let reply = r#"{"lines": ["version 18", "use \"a.dta\", clear"],
                        "dropped": [{"command": "browse", "why": "interactive"}]}"#;
        let h = history(reply).unwrap();
        assert_eq!(h.lines, vec!["version 18", "use \"a.dta\", clear"]);
        assert_eq!(h.dropped.len(), 1);
    }

    #[test]
    fn ghost_text_is_one_line_however_it_arrives() {
        assert_eq!(ghost("  , robust"), ", robust");
        assert_eq!(
            ghost("```stata\nregress price mpg\nsummarize\n```"),
            "regress price mpg"
        );
        assert_eq!(ghost("line one\nline two"), "line one");
        assert_eq!(ghost("   "), "");
    }
}
