//! Value labels — dataset-scoped, not variable-scoped (`04` §4.2).
//!
//! Many variables may point at the same table: `auto.dta` has one table
//! `origin = {0: "Domestic", 1: "Foreign"}` and `foreign` names it. That is why
//! the tables live on the frame and `label drop` / `label dir` operate on the
//! set, not on a variable.
//!
//! Keys are the **`long` (i32) encoding** of the value, which is Stata's own
//! rule and is what makes extended missings labellable: `.a` is key
//! 2147483622. A value outside `i32` cannot carry a label at all.

use rustc_hash::FxHashMap;
use std::sync::Arc;

use stratum_core::missing::{tag_of, LONG_MAX, LONG_MISS};

/// One `label define` table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValueLabel {
    map: FxHashMap<i32, String>,
}

impl ValueLabel {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Label `key`.
    pub fn insert(&mut self, key: i32, text: String) {
        self.map.insert(key, text);
    }

    /// Remove a mapping.
    pub fn remove(&mut self, key: i32) {
        self.map.remove(&key);
    }

    /// How many values are labelled.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when nothing is labelled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The label for a `double` value, narrowing the way Stata does.
    ///
    /// A non-integral value is never labelled (`1.5` has no `long` encoding);
    /// a missing value is looked up under its `long` sentinel, so `.a` finds
    /// the label defined for 2147483622.
    #[must_use]
    pub fn get(&self, v: f64) -> Option<&str> {
        self.map.get(&long_key(v)?).map(String::as_str)
    }

    /// Iterate `(key, label)` in unspecified order.
    pub fn iter(&self) -> impl Iterator<Item = (i32, &str)> {
        self.map.iter().map(|(k, v)| (*k, v.as_str()))
    }
}

/// The `long` encoding of `v`, or `None` when it has none.
#[must_use]
pub fn long_key(v: f64) -> Option<i32> {
    if let Some(tag) = tag_of(v) {
        return Some(LONG_MISS + i32::from(tag));
    }
    if v.fract() != 0.0 || v < -2_147_483_647.0 || v > f64::from(LONG_MAX) {
        return None;
    }
    Some(v as i32)
}

/// Every value-label table in a frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValueLabelSet {
    tables: FxHashMap<Arc<str>, ValueLabel>,
}

impl ValueLabelSet {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Define or replace a table.
    pub fn insert(&mut self, name: &str, table: ValueLabel) {
        self.tables.insert(Arc::from(name), table);
    }

    /// Look a table up by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ValueLabel> {
        self.tables.get(name)
    }

    /// Mutable access, creating the table if it does not exist — which is what
    /// `label define newname 1 "x"` does.
    pub fn entry(&mut self, name: &str) -> &mut ValueLabel {
        if !self.tables.contains_key(name) {
            self.tables.insert(Arc::from(name), ValueLabel::new());
        }
        self.tables.get_mut(name).expect("just inserted")
    }

    /// `label drop`.
    pub fn drop_table(&mut self, name: &str) -> bool {
        self.tables.remove(name).is_some()
    }

    /// `label dir`, sorted so the output is reproducible.
    #[must_use]
    pub fn names(&self) -> Vec<Arc<str>> {
        let mut v: Vec<Arc<str>> = self.tables.keys().cloned().collect();
        v.sort_unstable();
        v
    }

    /// How many tables are defined.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tables.len()
    }

    /// True when no table is defined.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_core::missing::{missing_f64, SYSMISS};

    #[test]
    fn extended_missings_are_labellable_under_their_long_encoding() {
        // `04` §4.2: `.a` is key 2147483622.
        assert_eq!(long_key(SYSMISS), Some(2_147_483_621));
        assert_eq!(long_key(missing_f64(1)), Some(2_147_483_622));
        assert_eq!(long_key(missing_f64(26)), Some(2_147_483_647));

        let mut t = ValueLabel::new();
        t.insert(2_147_483_622, "refused".into());
        assert_eq!(t.get(missing_f64(1)), Some("refused"));
        assert_eq!(t.get(SYSMISS), None);
    }

    #[test]
    fn a_non_integral_value_has_no_label() {
        let mut t = ValueLabel::new();
        t.insert(1, "one".into());
        assert_eq!(t.get(1.0), Some("one"));
        assert_eq!(t.get(1.5), None);
        assert_eq!(t.get(1e300), None);
    }

    #[test]
    fn tables_are_dataset_scoped_and_listed_in_order() {
        let mut s = ValueLabelSet::new();
        s.entry("origin").insert(0, "Domestic".into());
        s.entry("origin").insert(1, "Foreign".into());
        s.entry("yesno").insert(1, "Yes".into());
        assert_eq!(s.len(), 2);
        assert_eq!(
            s.names().iter().map(|n| n.to_string()).collect::<Vec<_>>(),
            vec!["origin", "yesno"]
        );
        assert_eq!(s.get("origin").expect("origin").get(1.0), Some("Foreign"));
        assert!(s.drop_table("yesno"));
        assert!(!s.drop_table("yesno"));
    }
}
