//! One variable's metadata.
//!
//! # `Arc<str>`, not `String` and not `CompactString`
//!
//! `04` §1.1 reached for `compact_str` to inline short names. That optimises the
//! wrong operation. The operation this engine performs constantly is *cloning
//! the metadata vector* — every `FrameSnapshot`, every `preserve`, every
//! `frame copy`, every Data-Editor page — and `Arc<str>` makes that a refcount
//! bump instead of a heap copy per name, per label, per format string. A
//! `Variable` clone allocates **nothing**, which is what turns the `frame copy`
//! acceptance bullet ("allocates O(nvars)") into "allocates one `Vec`".
//!
//! Renaming pays one allocation. Renaming is not on any hot path.
//!
//! # `Provenance` is proto's
//!
//! Declared once, in `stratum_proto::data` (A10). A structurally identical twin
//! here with no conversion between them is exactly the bug class that rule
//! exists to stop, so this module re-exports it and stores it behind an `Arc`
//! (it carries a statement's source text, and a snapshot must not copy that).

use std::sync::Arc;

use stratum_core::fmt::{DateTimeFmt, FormatKind, StataFormat};
use stratum_proto::{StorageType, VarId, VarIdx, VariableInfo};

pub use stratum_proto::Provenance;

use crate::version::DataVersion;

/// The maximum length of a Stata variable name, in bytes.
pub const MAX_NAME_LEN: usize = 32;

/// A variable's metadata. The storage itself is a
/// [`Column`](crate::column::Column) held in the frame at the same index.
#[derive(Clone, Debug, PartialEq)]
pub struct Variable {
    /// Column identity: survives `rename`, dies with `drop` (CONTRACTS §1).
    pub id: VarId,
    /// The Stata name. Validated by [`is_valid_name`] on the way in.
    pub name: Arc<str>,
    /// Storage type. Changing this is a whole-column rewrite, never a relabel.
    pub ty: StorageType,
    /// The variable label. Empty means none.
    pub label: Arc<str>,
    /// Parsed once at load and stored parsed — never re-parsed per cell (`04`
    /// §8), because cell formatting is on the Data Editor's hot path.
    pub format: StataFormat,
    /// Name of a table in the frame's [`ValueLabelSet`](crate::labels::ValueLabelSet).
    pub value_label: Option<Arc<str>>,
    /// Frame version at the last write to this column.
    pub version: DataVersion,
    /// Spec §20 "Created by analysis.do:42". Populated by the interpreter; we
    /// store it and hand it back. Never written to `.dta` — it would pollute a
    /// portable file — it lives in the `.workspace` sidecar.
    pub provenance: Option<Arc<Provenance>>,
}

impl Variable {
    /// A variable with a default format for its type and no label.
    ///
    /// # Panics
    ///
    /// Never: an invalid format literal here would be a bug in
    /// `stratum_core::types::default_format`, and `expect` says so rather than
    /// silently substituting `%9.0g`.
    #[must_use]
    pub fn new(id: VarId, name: &str, ty: StorageType, version: DataVersion) -> Self {
        let format = StataFormat::parse(stratum_core::types::default_format(ty))
            .expect("stratum_core::types::default_format returns a parseable format");
        Self {
            id,
            name: Arc::from(name),
            ty,
            label: Arc::from(""),
            format,
            value_label: None,
            version,
            provenance: None,
        }
    }

    /// The wire projection the Variables sidebar renders (spec §20).
    ///
    /// `n_missing` is passed in rather than computed: it is an O(rows) scan and
    /// the caller knows whether it already has the number cached.
    #[must_use]
    pub fn to_info(&self, idx: VarIdx, n_missing: u64) -> VariableInfo {
        VariableInfo {
            idx,
            id: self.id,
            name: self.name.to_string(),
            ty: self.ty,
            label: self.label.to_string(),
            format: format_string(&self.format),
            value_label: self.value_label.as_ref().map(|s| s.to_string()),
            n_missing,
            provenance: self.provenance.as_ref().map(|p| (**p).clone()),
        }
    }
}

/// Render a parsed [`StataFormat`] back to its Stata spelling.
///
/// `stratum_core::fmt` parses the grammar and does not currently invert it, and
/// two things need the inverse: `VariableInfo.format` on the wire (spec §20's
/// sidebar shows `%8.0gc`) and the `.dta` writer's `formats` block. Written here
/// because W02 needs it now; it belongs in `stratum_core::fmt` next to the
/// parser, and leaving a second copy in `stratum-dta` would be exactly the twin
/// A10 bans. Flagged for W01/W03.
#[must_use]
pub fn format_string(f: &StataFormat) -> String {
    let dash = if f.left { "-" } else { "" };
    match f.kind {
        FormatKind::DateTime(dt) => {
            let code = match dt {
                DateTimeFmt::Ms => 'c',
                DateTimeFmt::MsLeap => 'C',
                DateTimeFmt::Day => 'd',
                DateTimeFmt::Week => 'w',
                DateTimeFmt::Month => 'm',
                DateTimeFmt::Quarter => 'q',
                DateTimeFmt::HalfYear => 'h',
                DateTimeFmt::Year => 'y',
                DateTimeFmt::Generic => 'g',
            };
            format!("%{dash}t{code}")
        }
        FormatKind::Hex => format!("%{dash}{}x", f.width),
        FormatKind::Str => format!("%{dash}{}s", f.width),
        FormatKind::General | FormatKind::Fixed | FormatKind::Exponential => {
            let zero = if f.zero_pad { "0" } else { "" };
            let ty = match f.kind {
                FormatKind::General => 'g',
                FormatKind::Fixed => 'f',
                _ => 'e',
            };
            let commas = if f.commas { "c" } else { "" };
            format!("%{dash}{zero}{}.{}{ty}{commas}", f.width, f.prec)
        }
    }
}

/// Is `name` a legal Stata variable name?
///
/// `[A-Za-z_][A-Za-z0-9_]*`, at most [`MAX_NAME_LEN`] bytes. ASCII by
/// construction: Stata 14+ accepts Unicode names, and accepting them here would
/// mean `.dta` 117 (which is not UTF-8) could hold a name we cannot write back.
/// That restriction is deliberate and is the one v1 ships; widening it is a
/// reader/writer decision, not a storage one.
#[must_use]
pub fn is_valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return false;
    }
    let mut bytes = name.bytes();
    let first = bytes.next().expect("non-empty");
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_statas_rule() {
        assert!(is_valid_name("price"));
        assert!(is_valid_name("_n2"));
        assert!(is_valid_name("X9_"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("9lives"));
        assert!(!is_valid_name("has space"));
        assert!(!is_valid_name("héllo"));
        assert!(is_valid_name(&"a".repeat(32)));
        assert!(!is_valid_name(&"a".repeat(33)));
    }

    #[test]
    fn a_new_variable_carries_the_measured_default_format() {
        let v = Variable::new(VarId(1), "price", StorageType::Double, DataVersion::INITIAL);
        assert_eq!(format_string(&v.format), "%10.0g");
        let b = Variable::new(VarId(2), "b", StorageType::Byte, DataVersion::INITIAL);
        assert_eq!(format_string(&b.format), "%8.0g");
    }

    #[test]
    fn format_rendering_round_trips_the_auto_dta_formats() {
        // Every format `auto.dta` actually contains (`04` §0.1), plus the two
        // that exercise the flags.
        for s in [
            "%-18s", "%8.0gc", "%8.0g", "%6.1f", "%6.2f", "%10.0g", "%12.0g", "%9.2e", "%td", "%tC",
        ] {
            let f = StataFormat::parse(s).unwrap_or_else(|e| panic!("{s} did not parse: {e}"));
            assert_eq!(format_string(&f), s);
        }
    }

    #[test]
    fn cloning_a_variable_shares_its_strings() {
        let v = Variable::new(VarId(1), "price", StorageType::Double, DataVersion(3));
        let w = v.clone();
        assert!(Arc::ptr_eq(&v.name, &w.name));
        assert!(Arc::ptr_eq(&v.label, &w.label));
    }
}
