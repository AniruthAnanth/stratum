//! Characteristics, and notes as a projection over them (`04` §4.3).
//!
//! `auto.dta` proves the model rather than suggesting it. Its `characteristics`
//! block holds four `<ch>` records, all owned by `_dta`:
//!
//! ```text
//! _dta[_lang_c]    = "default"
//! _dta[_lang_list] = "default"
//! _dta[note0]      = "1"
//! _dta[note1]      = "From Consumer Reports with permission"
//! ```
//!
//! **Dataset notes are stored as characteristics, not as a separate section.**
//! So notes are not stored here at all — they are read and written *through*
//! the characteristics, which means they round-trip through `.dta` for free and
//! cannot drift out of sync with the `<ch>` records we actually write.
//!
//! Order is insertion order, preserved: Stata writes characteristics in the
//! order they were set and a reader that reorders them produces a file that
//! differs from Stata's for identical data.

use std::sync::Arc;

/// The dataset-level characteristic owner.
pub const DTA: &str = "_dta";

/// `(owner, name) -> value`, insertion-ordered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CharTable {
    entries: Vec<(Arc<str>, Arc<str>, String)>,
}

impl CharTable {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many `<ch>` records there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `owner[name]`.
    #[must_use]
    pub fn get(&self, owner: &str, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(o, n, _)| &**o == owner && &**n == name)
            .map(|(_, _, v)| v.as_str())
    }

    /// `char owner[name] value`. An empty value **deletes**, which is Stata's
    /// own rule (`char x[note1] ""` removes the characteristic).
    pub fn set(&mut self, owner: &str, name: &str, value: &str) {
        if value.is_empty() {
            self.remove(owner, name);
            return;
        }
        if let Some(slot) = self
            .entries
            .iter_mut()
            .find(|(o, n, _)| &**o == owner && &**n == name)
        {
            slot.2 = value.to_owned();
        } else {
            self.entries
                .push((Arc::from(owner), Arc::from(name), value.to_owned()));
        }
    }

    /// Remove `owner[name]`.
    pub fn remove(&mut self, owner: &str, name: &str) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|(o, n, _)| !(&**o == owner && &**n == name));
        self.entries.len() != before
    }

    /// Drop everything owned by `owner` — what `drop <var>` must do.
    pub fn remove_owner(&mut self, owner: &str) {
        self.entries.retain(|(o, _, _)| &**o != owner);
    }

    /// Move every characteristic from one owner to another — what `rename` must
    /// do, and the reason variable characteristics are keyed by name here
    /// rather than carried on `Variable`.
    pub fn rename_owner(&mut self, from: &str, to: &str) {
        let to: Arc<str> = Arc::from(to);
        for (o, _, _) in &mut self.entries {
            if &**o == from {
                *o = Arc::clone(&to);
            }
        }
    }

    /// Every `(owner, name, value)`, in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.entries
            .iter()
            .map(|(o, n, v)| (&**o, &**n, v.as_str()))
    }

    /// The notes of `owner`, read out of `note0..noteN`.
    ///
    /// `note0` is the count as a decimal string. A malformed or absent `note0`
    /// yields no notes rather than an error: a hostile `.dta` must not be able
    /// to make `notes` panic.
    #[must_use]
    pub fn notes(&self, owner: &str) -> Vec<&str> {
        let n: usize = self
            .get(owner, "note0")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        (1..=n)
            .filter_map(|i| self.get(owner, &format!("note{i}")))
            .collect()
    }

    /// Rewrite `note0..noteN` for `owner`, deleting any stale higher-numbered
    /// note left behind by a longer previous list.
    pub fn set_notes(&mut self, owner: &str, notes: &[String]) {
        let old: usize = self
            .get(owner, "note0")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        for i in notes.len() + 1..=old {
            self.remove(owner, &format!("note{i}"));
        }
        if notes.is_empty() {
            self.remove(owner, "note0");
            return;
        }
        self.set(owner, "note0", &notes.len().to_string());
        for (i, note) in notes.iter().enumerate() {
            self.set(owner, &format!("note{}", i + 1), note);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_auto_dta_characteristics_read_back_as_one_note() {
        // `04` §0.1, verbatim.
        let mut t = CharTable::new();
        t.set(DTA, "_lang_c", "default");
        t.set(DTA, "_lang_list", "default");
        t.set(DTA, "note0", "1");
        t.set(DTA, "note1", "From Consumer Reports with permission");
        assert_eq!(t.len(), 4);
        assert_eq!(t.notes(DTA), vec!["From Consumer Reports with permission"]);
    }

    #[test]
    fn shortening_a_note_list_deletes_the_stale_tail() {
        let mut t = CharTable::new();
        t.set_notes("price", &["a".into(), "b".into(), "c".into()]);
        assert_eq!(t.notes("price"), vec!["a", "b", "c"]);
        t.set_notes("price", &["z".into()]);
        assert_eq!(t.notes("price"), vec!["z"]);
        assert_eq!(t.get("price", "note2"), None);
        assert_eq!(t.get("price", "note3"), None);
    }

    #[test]
    fn clearing_the_notes_removes_note0_too() {
        let mut t = CharTable::new();
        t.set_notes("price", &["a".into()]);
        t.set_notes("price", &[]);
        assert!(t.notes("price").is_empty());
        assert_eq!(t.get("price", "note0"), None);
    }

    #[test]
    fn rename_moves_characteristics_and_drop_removes_them() {
        let mut t = CharTable::new();
        t.set("price", "units", "USD");
        t.set(DTA, "units", "mixed");
        t.rename_owner("price", "cost");
        assert_eq!(t.get("cost", "units"), Some("USD"));
        assert_eq!(t.get("price", "units"), None);
        assert_eq!(t.get(DTA, "units"), Some("mixed"));
        t.remove_owner("cost");
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn a_malformed_note0_yields_no_notes_rather_than_a_panic() {
        let mut t = CharTable::new();
        t.set(DTA, "note0", "not a number");
        assert!(t.notes(DTA).is_empty());
        t.set(DTA, "note0", "99");
        assert!(t.notes(DTA).is_empty(), "counts without bodies are dropped");
    }
}
