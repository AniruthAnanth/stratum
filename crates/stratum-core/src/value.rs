//! `Value` — the two types a Stata expression can have.
//!
//! `02` §8.2: "Stata expressions have exactly **two** types. There is no
//! boolean: true is `1`, false is `0`." Everything else — dates, value labels,
//! `by`-group indices — is one of these two at the value level and metadata at
//! the column level.
//!
//! Truthiness lives here because it is the single most commonly mis-ported rule
//! in the language: `if exp` selects an observation iff `exp != 0`, and `.` is
//! the enormous number `2^1023`, so **missing is truthy** (`04` §2.4, measured:
//! `count if x` counts `.` and `.a` and skips `0`).

use crate::missing::{canon, is_missing};

/// A Stata expression value.
///
/// The string arm is a plain `String`: `02` §8.2 proposed `CompactString`, but
/// the workspace dependency table (W00's file) does not carry `compact_str`,
/// and the allocation only matters inside the data engine's string columns,
/// which store their own packed representation anyway.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Every numeric expression evaluates in `double`, always.
    Real(f64),
    /// Byte-wise comparable; `""` is string missing, and it sorts LOW.
    Str(String),
}

impl Value {
    /// Stata's `1`/`0`, ready to store.
    #[must_use]
    pub fn bool(b: bool) -> Self {
        Value::Real(if b { 1.0 } else { 0.0 })
    }

    /// `.` — the plain system missing.
    #[must_use]
    pub fn missing() -> Self {
        Value::Real(crate::missing::SYSMISS)
    }

    /// The numeric payload, or `None` for a string.
    #[must_use]
    pub fn as_real(&self) -> Option<f64> {
        match self {
            Value::Real(v) => Some(*v),
            Value::Str(_) => None,
        }
    }

    /// The string payload, or `None` for a number.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Real(_) => None,
        }
    }

    /// True when this is the type's missing value: `.`/`.a`..`.z`, or `""`.
    #[must_use]
    pub fn is_missing(&self) -> bool {
        match self {
            Value::Real(v) => is_missing(*v),
            Value::Str(s) => s.is_empty(),
        }
    }

    /// `if exp` semantics: an observation is selected iff the value is
    /// **nonzero**. Missing is nonzero, therefore missing is TRUE.
    ///
    /// A string in an `if` is `r(109) type mismatch` at the parse layer, so it
    /// never reaches here; the arm answers `false` rather than panicking.
    #[must_use]
    pub fn truthy(&self) -> bool {
        match self {
            Value::Real(v) => *v != 0.0,
            Value::Str(_) => false,
        }
    }

    /// Canonicalise a computed real into Stata's value domain (Invariant M).
    /// A no-op on strings.
    #[must_use]
    pub fn canonical(self) -> Self {
        match self {
            Value::Real(v) => Value::Real(canon(v)),
            s @ Value::Str(_) => s,
        }
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Real(v)
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::missing::{missing_f64, SYSMISS};

    #[test]
    fn missing_is_truthy() {
        // 04 §2.4, measured: `count if x` counted `.` and `.a`, skipped 0.
        assert!(Value::Real(SYSMISS).truthy());
        assert!(Value::Real(missing_f64(1)).truthy());
        assert!(Value::Real(3.0).truthy());
        assert!(!Value::Real(0.0).truthy());
    }

    #[test]
    fn string_missing_is_the_empty_string() {
        assert!(Value::from("").is_missing());
        assert!(!Value::from("a").is_missing());
    }

    #[test]
    fn string_order_is_bytewise() {
        // 02 §8.2: "cat" > "Zebra" is TRUE, "a" < "B" is FALSE.
        assert!(Value::from("cat").as_str() > Value::from("Zebra").as_str());
        assert!(Value::from("a").as_str() >= Value::from("B").as_str());
        // And "" sorts first, which is why string missing sorts LOW.
        assert!(Value::from("").as_str() < Value::from("a").as_str());
    }
}
