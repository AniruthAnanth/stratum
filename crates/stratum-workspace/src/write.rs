//! **The only place in Stratum that writes a `.do` file** — ADR-010,
//! ARCHITECTURE §6.3, spec §§5–6.
//!
//! Everything else in the product hands bytes to [`write_document`] or does not
//! write source at all. There are exactly four callers, they are the four
//! variants of [`Writer`], and each one can only reach [`write_document`] by
//! first constructing a [`GatedEdits`] — whose every constructor runs the gate
//! that writer is required to pass.
//!
//! That is the point of the type. "Four gated writers" stated as prose is what
//! the audit found (A15): `section.rename` and `section.move` were listed as
//! permitted writers with no command, no owner and no gate, while
//! `section.move` reorders *executable statements*. Stated as a type, a fifth
//! writer cannot be added by forgetting something — there is no way to obtain a
//! `GatedEdits` except through a gate, and no way to write except with one.
//!
//! # The seam to W20
//!
//! The two equivalence proofs belong to `stratum-intel` (W20):
//! `assert_comment_only` is the spec §23 proof and
//! `assert_statement_partition_preserved` is the §3 section-move proof (A15).
//! W20 has not landed. [`EditGate`] is the trait it plugs into, and
//! [`StandaloneGate`] is the conservative implementation this crate ships in the
//! meantime so that the guarantee is *enforced today* rather than deferred.
//!
//! `StandaloneGate` is conservative in the only direction that is safe: it may
//! reject an edit that was in fact harmless, but it accepts nothing whose
//! statement partition, code-token stream or code-byte histogram differs. What
//! it does **not** yet provide is design 07 §8.3's stronger claim that the three
//! checks are independent code paths sharing no lexer, and §13.2's requirement
//! that the lexer be *the same code the runtime executes with*. Both need
//! `stratum-parse`'s `ProgramIndex`. When W20 lands, `stratum_intel` implements
//! [`EditGate`] and the desktop passes it here; nothing else in this crate
//! changes.

use camino::{Utf8Path, Utf8PathBuf};
use stratum_proto::{Edit, TextHash};

use crate::bytes::{encode, DocBytes};
use crate::document::{apply_edits, EditError};

// ---------------------------------------------------------------------------
// The four writers
// ---------------------------------------------------------------------------

/// The four — and only four — code paths permitted to write a `.do` file.
///
/// Transcribed from ARCHITECTURE §6.3's table. The variant carried on a
/// [`GatedEdits`] is what the write is attributed to in the audit trail, and it
/// is fixed by the constructor, so it cannot be relabelled after the gate ran.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Writer {
    /// `doc_save`. Gate: byte fidelity — reproduce the recorded EOL and BOM
    /// exactly, transform nothing else.
    DocSave,
    /// `section_rename`. Gate: `assert_comment_only`.
    SectionRename,
    /// `section_move`. Gate: `assert_statement_partition_preserved`.
    SectionMove,
    /// `ai_apply_patch`. Gate: explicit user acceptance, plus
    /// `assert_comment_only` when the task's declared scope is comment-only.
    AiApplyPatch,
}

impl Writer {
    /// The command name, for diagnostics and the audit log.
    pub const fn command(self) -> &'static str {
        match self {
            Writer::DocSave => "doc_save",
            Writer::SectionRename => "section_rename",
            Writer::SectionMove => "section_move",
            Writer::AiApplyPatch => "ai_apply_patch",
        }
    }
}

// ---------------------------------------------------------------------------
// The gate seam
// ---------------------------------------------------------------------------

/// Which of the checks in design 07 §8.3 rejected a patch.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Check {
    /// Check A — the significant token stream of the code differs.
    TokenStream,
    /// Check B — the statement partition differs (count or content).
    StatementPartition,
    /// Check C — the non-whitespace code-byte histogram differs.
    ByteHistogram,
    /// The multiset of statement bytes changed, so this was not a reordering.
    StatementMultiset,
    /// A comment was added, removed or altered by an operation that may only
    /// move text.
    CommentMultiset,
    /// The buffer ends inside a block comment or an unterminated string, so no
    /// scan of it can be trusted. Refused rather than guessed.
    Unterminated,
}

/// A gate said no. **The entire patch is rejected**; nothing is applied and
/// nothing is written (design 07 §8.3: "Silence on a failed safety check is not
/// an option").
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
#[error("{writer}: refused — {detail} (check {check:?})")]
pub struct GateRejection {
    /// The writer whose edits were refused.
    pub writer: &'static str,
    /// Which check fired.
    pub check: Check,
    /// Human-readable detail, shown to the user with a `[Report]` affordance.
    pub detail: String,
}

/// The two equivalence proofs, as consumed by this crate.
///
/// Implemented by `stratum_intel` (W20) in production and by [`StandaloneGate`]
/// until it lands. Both methods take whole buffers rather than an edit list on
/// purpose: the proof is about the *program*, and an edit list is only a claim
/// about how the program got there.
pub trait EditGate {
    /// Prove that `after` differs from `before` only in comments and
    /// whitespace — that the runtime sees the same program.
    fn assert_comment_only(&self, before: &str, after: &str) -> Result<(), GateRejection>;

    /// Prove that `after` is a *reordering* of `before`: the multiset of
    /// per-statement canonical token streams is unchanged, every statement's
    /// bytes are unchanged, and only the order differs.
    fn assert_statement_partition_preserved(
        &self,
        before: &str,
        after: &str,
    ) -> Result<(), GateRejection>;
}

// ---------------------------------------------------------------------------
// The unforgeable permission slip
// ---------------------------------------------------------------------------

/// An edit list that has already passed its writer's gate, together with the
/// resulting buffer.
///
/// There is no public constructor and no public field. The only ways to build
/// one are the four associated functions below, one per [`Writer`], and each
/// runs that writer's gate before returning. So `write_document` can take
/// "an already-gated edit list" as a *fact about the type* rather than a comment
/// somebody has to keep true.
#[derive(Clone, Debug)]
pub struct GatedEdits {
    writer: Writer,
    edits: Vec<Edit>,
    text: String,
}

impl GatedEdits {
    /// `doc_save` — the byte-fidelity writer. No edits: the buffer is written as
    /// it stands, and the only thing that may differ from the in-memory text is
    /// the EOL/BOM reconstruction in [`crate::bytes::encode`].
    pub fn byte_fidelity(text: impl Into<String>) -> Self {
        GatedEdits {
            writer: Writer::DocSave,
            edits: Vec::new(),
            text: text.into(),
        }
    }

    /// `section_rename` — gated by `assert_comment_only`.
    ///
    /// A section title lives in a `// %%` comment, so renaming one is provably a
    /// comment-only edit; if it is not, something other than the title changed
    /// and the rename is refused.
    pub fn section_rename(
        before: &str,
        edits: Vec<Edit>,
        gate: &dyn EditGate,
    ) -> Result<Self, WriteError> {
        Self::comment_only(Writer::SectionRename, before, edits, gate)
    }

    /// `ai_apply_patch` for a comment-scoped task — gated by
    /// `assert_comment_only` (spec §23).
    ///
    /// The caller is responsible for the *other* half of this writer's gate,
    /// explicit user acceptance in the diff view; that is a UI fact this crate
    /// cannot observe. A patch whose declared scope is not comment-only takes
    /// [`GatedEdits::ai_accepted_patch`].
    pub fn ai_comment_patch(
        before: &str,
        edits: Vec<Edit>,
        gate: &dyn EditGate,
    ) -> Result<Self, WriteError> {
        Self::comment_only(Writer::AiApplyPatch, before, edits, gate)
    }

    /// `ai_apply_patch` for a patch the user explicitly accepted in the diff
    /// view, whose declared scope is *not* comment-only (a refactor, a repro
    /// fix).
    ///
    /// `accepted` is the caller's assertion that a human read the diff and
    /// pressed Accept. It is a required argument rather than an implicit `true`
    /// so that the acceptance appears in the call site of every such write.
    pub fn ai_accepted_patch(
        before: &str,
        edits: Vec<Edit>,
        accepted: bool,
    ) -> Result<Self, WriteError> {
        if !accepted {
            return Err(WriteError::Gate(GateRejection {
                writer: Writer::AiApplyPatch.command(),
                check: Check::StatementPartition,
                detail: "an AI patch outside comment scope requires explicit user \
                         acceptance in the diff view"
                    .to_owned(),
            }));
        }
        let text = apply_edits(before, &edits)?;
        Ok(GatedEdits {
            writer: Writer::AiApplyPatch,
            edits,
            text,
        })
    }

    /// `section_move` — gated by `assert_statement_partition_preserved`.
    pub fn section_move(
        before: &str,
        edits: Vec<Edit>,
        gate: &dyn EditGate,
    ) -> Result<Self, WriteError> {
        let text = apply_edits(before, &edits)?;
        gate.assert_statement_partition_preserved(before, &text)?;
        Ok(GatedEdits {
            writer: Writer::SectionMove,
            edits,
            text,
        })
    }

    fn comment_only(
        writer: Writer,
        before: &str,
        edits: Vec<Edit>,
        gate: &dyn EditGate,
    ) -> Result<Self, WriteError> {
        let text = apply_edits(before, &edits)?;
        gate.assert_comment_only(before, &text)?;
        Ok(GatedEdits {
            writer,
            edits,
            text,
        })
    }

    /// Which writer produced this.
    pub fn writer(&self) -> Writer {
        self.writer
    }

    /// The buffer after the edits — what will be written.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The edits themselves, for the `{ edits, version }` IPC reply.
    pub fn edits(&self) -> &[Edit] {
        &self.edits
    }

    /// Consume into the edit list, for callers that only need to echo it back.
    pub fn into_edits(self) -> Vec<Edit> {
        self.edits
    }
}

// ---------------------------------------------------------------------------
// The write itself
// ---------------------------------------------------------------------------

/// What `doc_save` reports back (CONTRACTS §11).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SavedAck {
    /// Where it was written.
    pub path: Utf8PathBuf,
    /// blake3-128 over **the bytes on disk**, not over the in-memory buffer.
    ///
    /// Its documented job is detecting "the file changed on disk", so it has to
    /// be a hash of what is on disk: a CRLF file and its LF twin hold the same
    /// text and are not the same file.
    pub text_hash: TextHash,
    /// The EOL that was written.
    pub eol: crate::bytes::Eol,
    /// Whether a BOM was written.
    pub bom: bool,
    /// Which of the four writers wrote it.
    pub writer: Writer,
}

/// Everything that can stop a write.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// A gate refused the edits. Nothing was applied and nothing was written.
    #[error(transparent)]
    Gate(#[from] GateRejection),
    /// The edit list could not be applied to the buffer.
    #[error(transparent)]
    Edit(#[from] EditError),
    /// The document was opened read-only — the usual reason being that its
    /// bytes are not UTF-8 and we declined to transcode them (`STRATUM0601`).
    #[error("{path} is open read-only and will not be written")]
    ReadOnly {
        /// The document's path.
        path: Utf8PathBuf,
    },
    /// The filesystem said no.
    #[error("writing {path}: {source}")]
    Io {
        /// The document's path.
        path: Utf8PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// **The single sanctioned `.do` writer.** ADR-010, ARCHITECTURE §6.3.
///
/// Takes the recorded [`DocBytes`] and an already-gated edit list, reconstitutes
/// the file's own line endings and BOM, and lands the bytes atomically: write a
/// sibling temporary file, flush and `sync_all` it, then `rename` over the
/// target. A crash mid-save therefore leaves either the old file or the new one,
/// never a half-written do-file — the failure mode that costs somebody an
/// afternoon of analysis.
///
/// The temporary file is a sibling rather than a `$TMPDIR` entry because
/// `rename` is only atomic within a filesystem, and a user's project may well be
/// on an external volume or a network share.
pub fn write_document(
    path: &Utf8Path,
    doc_bytes: DocBytes,
    gated: &GatedEdits,
) -> Result<SavedAck, WriteError> {
    let raw = encode(gated.text(), doc_bytes);
    write_bytes_atomic(path, &raw).map_err(|source| WriteError::Io {
        path: path.to_owned(),
        source,
    })?;
    Ok(SavedAck {
        path: path.to_owned(),
        text_hash: TextHash(text_hash_of(&raw)),
        eol: doc_bytes.eol,
        bom: doc_bytes.bom,
        writer: gated.writer(),
    })
}

/// Land `raw` at `path` atomically: sibling temp file, `sync_all`, `rename`.
///
/// **This is the crate's only file-creating primitive**, and
/// `tests/do_writer_lint.rs` keeps it that way — that file also carries the
/// repo-wide half of ARCHITECTURE §6.3's lint, which no other crate may open a
/// `.do` path for writing. Sidecars, layout overlays and the keymap overlay go
/// through this function too — not because ADR-010 covers them, but
/// because a torn write of the durable sidecar loses a researcher's section
/// names, and there is no reason to have a second, worse write path in the crate
/// for the lint to have to reason about.
pub(crate) fn write_bytes_atomic(path: &Utf8Path, raw: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let dir = path.parent().unwrap_or_else(|| Utf8Path::new("."));
    let file_name = path.file_name().unwrap_or("document");
    // `.` prefix so a half-landed temp file is hidden rather than showing up in
    // the project explorer, and a pid suffix so two windows writing two files in
    // the same directory cannot collide.
    let tmp = dir.join(format!(".{file_name}.{}.tmp", std::process::id()));

    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(raw)?;
        // Durability before visibility: without this the rename can be ordered
        // ahead of the data on a crash and the file lands empty.
        f.sync_all()?;
    }
    // Carry the original file's permission bits across, so saving does not
    // quietly make a read-only-for-the-group project file world-writable.
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// blake3-128 (first 16 bytes of the blake3 digest), the product's one hash.
pub(crate) fn text_hash_of(raw: &[u8]) -> [u8; 16] {
    let full = blake3::hash(raw);
    let mut out = [0u8; 16];
    out.copy_from_slice(&full.as_bytes()[..16]);
    out
}

// ---------------------------------------------------------------------------
// The conservative standalone gate
// ---------------------------------------------------------------------------

pub use standalone::StandaloneGate;

mod standalone {
    //! A self-contained implementation of the two proofs, good enough to enforce
    //! them today and deliberately stricter than `stratum-intel` will be.
    //!
    //! Read the "seam to W20" note in the module header before extending this.
    //! The rule for every judgement call here is: **when unsure, classify as
    //! code**. Mistaking a comment for code makes the gate reject a harmless
    //! edit, which costs a user one refusal. Mistaking code for a comment makes
    //! the gate erase a real change before comparing, which is the failure this
    //! whole subsystem exists to prevent.

    use super::{Check, EditGate, GateRejection};

    /// The conservative stand-in for `stratum_intel`'s two proofs.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct StandaloneGate;

    /// One logical statement: its code with comments removed, and its raw source
    /// bytes from the first code byte to the last.
    #[derive(Clone, PartialEq, Eq, Debug)]
    struct Statement {
        code: Vec<u8>,
        raw: Vec<u8>,
    }

    #[derive(Debug)]
    struct Scan {
        statements: Vec<Statement>,
        /// Every comment body in document order, so a "move" cannot quietly drop
        /// one.
        comments: Vec<Vec<u8>>,
        /// The buffer ended inside a block comment or an unterminated string.
        unterminated: bool,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Delim {
        Cr,
        Semi,
    }

    /// Lex `src` into logical statements, stripping comments.
    ///
    /// Handles, because each of these is a way to get the answer wrong:
    /// `//` and `*` line comments, `/* */` blocks (**non-nesting**: Stata nests
    /// them, and assuming it does not can only end a comment *early*, i.e. treat
    /// comment text as code, which is the safe direction), `///` continuations,
    /// `"…"` and compound `` `"…"' `` string literals, `#delimit ;` regions, and
    /// `input`/`mata`/`python`/`java` … `end` verbatim regions in which nothing
    /// is a comment.
    fn scan(src: &str) -> Scan {
        let b = src.as_bytes();
        let n = b.len();
        let mut statements = Vec::new();
        let mut comments = Vec::new();

        let mut delim = Delim::Cr;
        let mut verbatim = false;
        let mut i = 0usize;

        // Accumulators for the statement in progress.
        let mut code: Vec<u8> = Vec::new();
        let mut code_start: Option<usize> = None;
        let mut code_end = 0usize;
        let mut unterminated = false;

        // `.take()` rather than a trailing `code_start = None`: the final call
        // after the loop would otherwise be an assignment nothing reads.
        macro_rules! finish {
            () => {
                if let Some(start) = code_start.take() {
                    statements.push(Statement {
                        code: std::mem::take(&mut code),
                        raw: b[start..code_end].to_vec(),
                    });
                } else {
                    code.clear();
                }
            };
        }

        while i < n {
            let c = b[i];

            // A newline: statement break in `cr` mode, plain whitespace under
            // `#delimit ;`.
            if c == b'\n' {
                if verbatim || delim == Delim::Cr {
                    finish!();
                } else {
                    code.push(b' ');
                }
                i += 1;
                continue;
            }

            if verbatim {
                // Inside `input`/`mata`/… nothing is a comment. Copy verbatim and
                // watch for the terminating `end` line.
                if !c.is_ascii_whitespace() {
                    if code_start.is_none() {
                        code_start = Some(i);
                    }
                    code_end = i + 1;
                }
                code.push(c);
                i += 1;
                if i == n || b[i] == b'\n' {
                    let line: &[u8] = &code;
                    if trim(line) == b"end" {
                        verbatim = false;
                    }
                }
                continue;
            }

            let at_stmt_start = code.iter().all(|x| x.is_ascii_whitespace());
            let prev_is_boundary = i == 0 || b[i - 1].is_ascii_whitespace();

            // `*` line comment — only legal where a statement may begin.
            if c == b'*' && at_stmt_start {
                let end = line_end(b, i);
                comments.push(b[i..end].to_vec());
                i = end;
                continue;
            }

            // `///` continuation, then `//` line comment. Order matters.
            if c == b'/' && prev_is_boundary && b[i..].starts_with(b"///") {
                let end = line_end(b, i);
                comments.push(b[i..end].to_vec());
                // The newline is swallowed: the statement continues.
                i = if end < n { end + 1 } else { end };
                code.push(b' ');
                continue;
            }
            if c == b'/' && prev_is_boundary && b[i..].starts_with(b"//") {
                let end = line_end(b, i);
                comments.push(b[i..end].to_vec());
                i = end;
                continue;
            }

            // `/* … */` block comment. Legal anywhere in code.
            if c == b'/' && b[i..].starts_with(b"/*") {
                match find(b, i + 2, b"*/") {
                    Some(end) => {
                        comments.push(b[i..end + 2].to_vec());
                        i = end + 2;
                        // A block comment is a token separator, not a joiner.
                        code.push(b' ');
                    }
                    None => {
                        // Unterminated: refuse to reason about this buffer.
                        comments.push(b[i..].to_vec());
                        unterminated = true;
                        i = n;
                    }
                }
                continue;
            }

            // String literals. Their bytes are code, comment markers inside them
            // are not comments.
            if c == b'`' && b[i..].starts_with(b"`\"") {
                let end = compound_quote_end(b, i);
                if end.is_none() {
                    unterminated = true;
                }
                let end = end.unwrap_or(n);
                push_code(&mut code, &mut code_start, &mut code_end, b, i, end);
                i = end;
                continue;
            }
            if c == b'"' {
                let end = match find_byte(b, i + 1, b'"') {
                    // A `"` string does not span lines in Stata; if the closing
                    // quote is past the newline the literal is unterminated.
                    Some(e) if !b[i + 1..e].contains(&b'\n') => e + 1,
                    _ => {
                        unterminated = true;
                        line_end(b, i)
                    }
                };
                push_code(&mut code, &mut code_start, &mut code_end, b, i, end);
                i = end;
                continue;
            }

            // Ordinary code byte.
            if !c.is_ascii_whitespace() {
                if code_start.is_none() {
                    code_start = Some(i);
                }
                code_end = i + 1;
            }
            code.push(c);
            i += 1;

            // Statement terminator under `#delimit ;`.
            if c == b';' && delim == Delim::Semi {
                finish!();
                continue;
            }

            // Two statement-scoped mode switches, decided the moment the
            // statement's first word is complete.
            if i == n || b[i].is_ascii_whitespace() || b[i] == b';' {
                match first_word(&code) {
                    w if w == b"#delimit" => {
                        // Read to the end of the physical line to see which mode.
                        let rest = &b[i..line_end(b, i)];
                        delim = if rest.contains(&b';') {
                            Delim::Semi
                        } else {
                            Delim::Cr
                        };
                    }
                    w if is_verbatim_opener(w) => {
                        // `mata: x = 1` and `input a b` on one line still open a
                        // verbatim region in Stata unless the region is closed on
                        // the same line, which it cannot be — `end` is its own
                        // command.
                        verbatim = true;
                    }
                    _ => {}
                }
            }
        }

        finish!();
        Scan {
            statements,
            comments,
            unterminated,
        }
    }

    fn push_code(
        code: &mut Vec<u8>,
        code_start: &mut Option<usize>,
        code_end: &mut usize,
        b: &[u8],
        from: usize,
        to: usize,
    ) {
        if code_start.is_none() {
            *code_start = Some(from);
        }
        *code_end = to;
        code.extend_from_slice(&b[from..to]);
    }

    fn is_verbatim_opener(w: &[u8]) -> bool {
        // design 07 §8.2's list. `program … end` is deliberately absent: its body
        // is ordinary code and comments in it are ordinary comments.
        matches!(
            w,
            b"input" | b"mata" | b"mata:" | b"python" | b"python:" | b"java" | b"java:"
        )
    }

    fn first_word(code: &[u8]) -> &[u8] {
        let s = trim(code);
        match s.iter().position(|c| c.is_ascii_whitespace()) {
            Some(p) => &s[..p],
            None => s,
        }
    }

    fn trim(mut s: &[u8]) -> &[u8] {
        while let [f, rest @ ..] = s {
            if f.is_ascii_whitespace() {
                s = rest;
            } else {
                break;
            }
        }
        while let [rest @ .., l] = s {
            if l.is_ascii_whitespace() {
                s = rest;
            } else {
                break;
            }
        }
        s
    }

    fn line_end(b: &[u8], from: usize) -> usize {
        b[from..]
            .iter()
            .position(|&c| c == b'\n')
            .map_or(b.len(), |p| from + p)
    }

    fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
        if from >= b.len() {
            return None;
        }
        b[from..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| from + p)
    }

    fn find_byte(b: &[u8], from: usize, needle: u8) -> Option<usize> {
        if from >= b.len() {
            return None;
        }
        b[from..]
            .iter()
            .position(|&c| c == needle)
            .map(|p| from + p)
    }

    /// End of a compound `` `"…"' `` literal, which nests.
    fn compound_quote_end(b: &[u8], from: usize) -> Option<usize> {
        let mut depth = 0usize;
        let mut i = from;
        while i < b.len() {
            if b[i..].starts_with(b"`\"") {
                depth += 1;
                i += 2;
            } else if b[i..].starts_with(b"\"'") {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    return Some(i);
                }
            } else {
                i += 1;
            }
        }
        None
    }

    /// Check A's projection: `(kind, text)` pairs over the code of one statement.
    ///
    /// Whitespace is dropped; identifiers, numbers and string literals are kept
    /// whole; every other byte is its own token. Keeping literals whole is what
    /// stops `"a b"` and `"a  b"` from comparing equal after whitespace
    /// collapsing.
    fn tokens(code: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < code.len() {
            let c = code[i];
            if c.is_ascii_whitespace() {
                i += 1;
            } else if c == b'"' {
                let end = find_byte(code, i + 1, b'"').map_or(code.len(), |e| e + 1);
                out.push(code[i..end].to_vec());
                i = end;
            } else if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c >= 0x80 {
                let start = i;
                while i < code.len()
                    && (code[i].is_ascii_alphanumeric()
                        || code[i] == b'_'
                        || code[i] == b'.'
                        || code[i] >= 0x80)
                {
                    i += 1;
                }
                out.push(code[start..i].to_vec());
            } else {
                out.push(vec![c]);
                i += 1;
            }
        }
        out
    }

    /// Check C: counts of every non-whitespace byte of code.
    fn histogram(statements: &[Statement]) -> [u32; 256] {
        let mut h = [0u32; 256];
        for s in statements {
            for &c in &s.code {
                if !c.is_ascii_whitespace() {
                    h[c as usize] += 1;
                }
            }
        }
        h
    }

    fn reject(writer: &'static str, check: Check, detail: impl Into<String>) -> GateRejection {
        GateRejection {
            writer,
            check,
            detail: detail.into(),
        }
    }

    fn sorted(mut v: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
        v.sort();
        v
    }

    impl EditGate for StandaloneGate {
        fn assert_comment_only(&self, before: &str, after: &str) -> Result<(), GateRejection> {
            let w = "assert_comment_only";
            let (a, b) = (scan(before), scan(after));

            if a.unterminated || b.unterminated {
                return Err(reject(
                    w,
                    Check::Unterminated,
                    "the buffer ends inside a block comment or an unterminated string \
                     literal; refusing to reason about it",
                ));
            }

            // Check B — statement partition. First, because when it fires it has
            // the most useful message: it is what catches a comment inserted into
            // the middle of a `///` continuation chain.
            if a.statements.len() != b.statements.len() {
                return Err(reject(
                    w,
                    Check::StatementPartition,
                    format!(
                        "statement count changed: {} → {} (a comment inside a `///` \
                         chain terminates the statement early)",
                        a.statements.len(),
                        b.statements.len()
                    ),
                ));
            }
            for (i, (x, y)) in a.statements.iter().zip(&b.statements).enumerate() {
                if collapse(&x.code) != collapse(&y.code) {
                    return Err(reject(
                        w,
                        Check::StatementPartition,
                        format!("statement {i} changed: {}", show2(&x.code, &y.code)),
                    ));
                }
            }

            // Check A — significant token stream, with statement breaks kept.
            let sig = |s: &Scan| -> Vec<Vec<u8>> {
                let mut v = Vec::new();
                for st in &s.statements {
                    v.extend(tokens(&st.code));
                    v.push(b"\x00break".to_vec());
                }
                v
            };
            if sig(&a) != sig(&b) {
                return Err(reject(
                    w,
                    Check::TokenStream,
                    "the significant token stream of the code differs",
                ));
            }

            // Check C — byte histogram.
            if histogram(&a.statements) != histogram(&b.statements) {
                return Err(reject(
                    w,
                    Check::ByteHistogram,
                    "the non-whitespace code-byte histogram differs",
                ));
            }

            Ok(())
        }

        fn assert_statement_partition_preserved(
            &self,
            before: &str,
            after: &str,
        ) -> Result<(), GateRejection> {
            let w = "assert_statement_partition_preserved";
            let (a, b) = (scan(before), scan(after));

            if a.unterminated || b.unterminated {
                return Err(reject(
                    w,
                    Check::Unterminated,
                    "the buffer ends inside a block comment or an unterminated string \
                     literal; refusing to reason about it",
                ));
            }
            if a.statements.len() != b.statements.len() {
                return Err(reject(
                    w,
                    Check::StatementPartition,
                    format!(
                        "statement count changed: {} → {}; a move may not add or \
                         remove a statement",
                        a.statements.len(),
                        b.statements.len()
                    ),
                ));
            }

            // "every statement's bytes are unchanged; only the order differs"
            let raw = |s: &Scan| sorted(s.statements.iter().map(|x| x.raw.clone()).collect());
            if raw(&a) != raw(&b) {
                return Err(reject(
                    w,
                    Check::StatementMultiset,
                    "the multiset of statement bytes changed, so this is not a \
                     reordering",
                ));
            }

            // "the multiset of per-statement canonical token streams is unchanged"
            let toks = |s: &Scan| -> Vec<Vec<u8>> {
                sorted(
                    s.statements
                        .iter()
                        .map(|x| flatten(&tokens(&x.code)))
                        .collect(),
                )
            };
            if toks(&a) != toks(&b) {
                return Err(reject(
                    w,
                    Check::TokenStream,
                    "the multiset of per-statement canonical token streams changed",
                ));
            }

            // A move carries a section's comments with it. Losing or rewriting one
            // is not a reordering either, and nothing else in this gate would see
            // it — comments are stripped before every other comparison.
            if sorted(a.comments.clone()) != sorted(b.comments.clone()) {
                return Err(reject(
                    w,
                    Check::CommentMultiset,
                    "the multiset of comments changed; a move may only reorder text",
                ));
            }

            Ok(())
        }
    }

    fn flatten(t: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for x in t {
            out.extend_from_slice(x);
            out.push(0);
        }
        out
    }

    /// Collapse whitespace runs to one space and trim, for Check B's `normalize`.
    fn collapse(code: &[u8]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::with_capacity(code.len());
        let mut in_ws = true;
        for &c in code {
            if c.is_ascii_whitespace() {
                if !in_ws {
                    out.push(b' ');
                }
                in_ws = true;
            } else {
                out.push(c);
                in_ws = false;
            }
        }
        while out.last() == Some(&b' ') {
            out.pop();
        }
        out
    }

    fn show2(a: &[u8], b: &[u8]) -> String {
        format!(
            "{:?} → {:?}",
            String::from_utf8_lossy(&collapse(a)),
            String::from_utf8_lossy(&collapse(b))
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn stmts(src: &str) -> Vec<String> {
            scan(src)
                .statements
                .iter()
                .map(|s| String::from_utf8_lossy(&collapse(&s.code)).into_owned())
                .collect()
        }

        #[test]
        fn line_comments_and_blank_lines_are_not_statements() {
            assert_eq!(
                stmts("// %% Setup\n\nsysuse auto\n* a star comment\nlist\n"),
                vec!["sysuse auto", "list"]
            );
        }

        #[test]
        fn continuation_chain_is_one_statement() {
            assert_eq!(
                stmts("gen x = a + ///\n    b + ///\n    c\nlist\n"),
                vec!["gen x = a + b + c", "list"]
            );
        }

        #[test]
        fn delimit_semi_switches_the_terminator() {
            assert_eq!(
                stmts("#delimit ;\nsummarize\n  price;\nlist;\n#delimit cr\nlist\n"),
                vec![
                    "#delimit ;",
                    "summarize price;",
                    "list;",
                    "#delimit cr",
                    "list"
                ]
            );
        }

        #[test]
        fn comment_markers_inside_strings_are_not_comments() {
            assert_eq!(stmts("di \"a // b\"\n"), vec!["di \"a // b\""]);
            assert_eq!(stmts("di `\"a // b\"'\n"), vec!["di `\"a // b\"'"]);
        }

        #[test]
        fn star_is_multiplication_after_code() {
            assert_eq!(stmts("gen z = a*b\n"), vec!["gen z = a*b"]);
        }

        #[test]
        fn input_block_is_verbatim() {
            // `// 3` inside an `input` block is DATA, not a comment.
            assert_eq!(
                stmts("input a\n1\n// 3\nend\nlist\n"),
                vec!["input a", "1", "// 3", "end", "list"]
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_proto::Span;

    fn edit(start: u32, end: u32, text: &str) -> Edit {
        Edit {
            span: Span { start, end },
            text: text.to_owned(),
        }
    }

    const SRC: &str = "// %% Setup\nsysuse auto\n\n// %% Model\nregress price mpg\n";

    #[test]
    fn comment_gate_accepts_a_title_rename() {
        let e = vec![edit(6, 11, "Load the data")];
        let g = GatedEdits::section_rename(SRC, e, &StandaloneGate).unwrap();
        assert_eq!(g.writer(), Writer::SectionRename);
        assert!(g.text().starts_with("// %% Load the data\n"));
    }

    #[test]
    fn comment_gate_rejects_a_doctored_edit_that_touches_code() {
        // The edit claims to be a rename but reaches past the comment line.
        let e = vec![edit(6, 24, "Setup\nsysuse nlsw88")];
        let err = GatedEdits::section_rename(SRC, e, &StandaloneGate).unwrap_err();
        assert!(matches!(err, WriteError::Gate(_)), "{err:?}");
    }

    #[test]
    fn comment_gate_rejects_a_comment_that_breaks_a_continuation_chain() {
        let before = "gen x = a + ///\n    b\nlist\n";
        // Insert a comment line between the two halves of the chain.
        let e = vec![edit(16, 16, "// note\n")];
        let err = GatedEdits::ai_comment_patch(before, e, &StandaloneGate).unwrap_err();
        match err {
            WriteError::Gate(r) => assert_eq!(r.check, Check::StatementPartition),
            other => panic!("expected a gate rejection, got {other:?}"),
        }
    }

    #[test]
    fn comment_gate_accepts_a_comment_above_a_chain() {
        let before = "gen x = a + ///\n    b\nlist\n";
        let e = vec![edit(0, 0, "// note\n")];
        assert!(GatedEdits::ai_comment_patch(before, e, &StandaloneGate).is_ok());
    }

    #[test]
    fn partition_gate_accepts_a_pure_reorder() {
        let before = "sysuse auto\nregress price mpg\n";
        let after_edits = vec![edit(0, 30, "regress price mpg\nsysuse auto\n")];
        assert!(GatedEdits::section_move(before, after_edits, &StandaloneGate).is_ok());
    }

    #[test]
    fn partition_gate_rejects_an_edit_that_alters_a_moved_statement() {
        let before = "sysuse auto\nregress price mpg\n";
        let after_edits = vec![edit(0, 30, "regress price weight\nsysuse auto\n")];
        let err = GatedEdits::section_move(before, after_edits, &StandaloneGate).unwrap_err();
        match err {
            WriteError::Gate(r) => assert_eq!(r.check, Check::StatementMultiset),
            other => panic!("expected a gate rejection, got {other:?}"),
        }
    }

    #[test]
    fn partition_gate_rejects_a_dropped_comment() {
        let before = "// %% A\nsysuse auto\n// %% B\nlist\n";
        // Same statements in a new order, but the `// %% A` heading is gone.
        let after_edits = vec![edit(0, 33, "// %% B\nlist\nsysuse auto\n")];
        let err = GatedEdits::section_move(before, after_edits, &StandaloneGate).unwrap_err();
        match err {
            WriteError::Gate(r) => assert_eq!(r.check, Check::CommentMultiset),
            other => panic!("expected a gate rejection, got {other:?}"),
        }
    }

    #[test]
    fn an_unaccepted_ai_patch_cannot_be_written() {
        let e = vec![edit(0, 0, "drop _all\n")];
        assert!(GatedEdits::ai_accepted_patch(SRC, e, false).is_err());
    }
}
