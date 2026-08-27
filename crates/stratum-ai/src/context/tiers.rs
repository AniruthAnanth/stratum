//! 07 §4.1 / ADR-012 — the tier ladder.
//!
//! `Ord` is the whole enforcement mechanism. The gate is one comparison
//! ([`crate::context::gate`]), the effective tier is one `min` fold
//! ([`crate::context::policy::effective_tier`]), and a new context source cannot
//! be added without declaring a tier at the type level because
//! [`crate::context::ContextItem`] has no constructor that omits it.

use serde::{Deserialize, Serialize};

/// What a surface is permitted to send.
///
/// The default is [`PrivacyTier::SchemaOnly`]. Not `Off`, because a tool that
/// sends nothing gives useless answers and users will turn it to `Full` in
/// frustration; not `SchemaAndStats`, because min/max on an administrative
/// variable can be disclosive — a maximum salary identifies one person.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum PrivacyTier {
    /// No context leaves the machine. AI surfaces that need context are
    /// disabled; generic help ("what does `xtreg, fe` do?") still works,
    /// because it carries no user data.
    Off = 0,
    /// **The default.** Code the user wrote, plus schema: variable names,
    /// storage types, display formats, variable labels, value-label *names*,
    /// macro *names*, `e()`/`r()` *names*, project-relative file names. No
    /// values. No statistics.
    #[default]
    SchemaOnly = 1,
    /// Adds aggregate statistics: N, missing counts, distinct counts, mean, sd,
    /// min, max, quartiles, category labels and frequencies, `e()`/`r()`
    /// numeric contents, coefficient tables.
    SchemaAndStats = 2,
    /// Adds literal cell values (bounded head rows), macro *contents*, absolute
    /// paths and verbatim result text.
    Full = 3,
}

impl PrivacyTier {
    /// Every tier, ascending. Drives the settings selector and the tier tests.
    pub const ALL: [Self; 4] = [
        Self::Off,
        Self::SchemaOnly,
        Self::SchemaAndStats,
        Self::Full,
    ];

    /// The stable key used in `.stratum/ai-policy.toml`, in settings and in the
    /// audit log.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::SchemaOnly => "schema_only",
            Self::SchemaAndStats => "schema_and_stats",
            Self::Full => "full",
        }
    }

    /// Parse the policy-file / settings spelling.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "off" | "none" => Some(Self::Off),
            "schema_only" | "schema" => Some(Self::SchemaOnly),
            "schema_and_stats" | "stats" => Some(Self::SchemaAndStats),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    /// The plain-language sentence the settings pane and the first-run card
    /// render. Not decoration: 07 §12 requires the tier selector to state
    /// exactly what each tier sends, and a description that lives next to the
    /// enum cannot drift from it.
    #[must_use]
    pub const fn describes(self) -> &'static str {
        match self {
            Self::Off => "Nothing about your session is sent. General questions still work.",
            Self::SchemaOnly => {
                "Your code, plus variable names, types, formats and labels. No values, no statistics."
            }
            Self::SchemaAndStats => {
                "Also summary statistics: counts, means, standard deviations, ranges, category \
                 frequencies and coefficient tables."
            }
            Self::Full => {
                "Also literal data values from the first rows, macro contents, absolute paths and \
                 verbatim output."
            }
        }
    }

    /// The title the UI shows.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Off => "Send nothing",
            Self::SchemaOnly => "Schema only",
            Self::SchemaAndStats => "Schema and summary statistics",
            Self::Full => "Full context",
        }
    }
}

impl std::fmt::Display for PrivacyTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ladder_is_ordered_and_that_ordering_is_the_gate() {
        assert!(PrivacyTier::Off < PrivacyTier::SchemaOnly);
        assert!(PrivacyTier::SchemaOnly < PrivacyTier::SchemaAndStats);
        assert!(PrivacyTier::SchemaAndStats < PrivacyTier::Full);
        assert_eq!(
            PrivacyTier::ALL.iter().copied().min(),
            Some(PrivacyTier::Off)
        );
    }

    #[test]
    fn the_default_is_schema_only() {
        // 07 §4.1 / ADR-012. Changing this is a privacy decision, not a
        // refactor, so it is asserted rather than assumed.
        assert_eq!(PrivacyTier::default(), PrivacyTier::SchemaOnly);
    }

    #[test]
    fn every_spelling_round_trips() {
        for t in PrivacyTier::ALL {
            assert_eq!(PrivacyTier::parse(t.as_str()), Some(t));
            let json = serde_json::to_string(&t).unwrap();
            assert_eq!(json, format!("\"{}\"", t.as_str()));
        }
        assert_eq!(
            PrivacyTier::parse("SCHEMA-ONLY"),
            Some(PrivacyTier::SchemaOnly)
        );
        assert_eq!(PrivacyTier::parse("everything"), None);
    }

    #[test]
    fn every_tier_has_a_plain_language_description() {
        for t in PrivacyTier::ALL {
            assert!(!t.describes().is_empty());
            assert!(!t.title().is_empty());
        }
    }
}
