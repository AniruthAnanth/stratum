//! `FrameSet` — Stata 16 frames, from day one (`04` §7).
//!
//! The engine is frame-aware from the start because retrofitting frames onto a
//! single-dataset engine is the single most expensive mistake available here,
//! and it is free to avoid now. There is no global "the dataset" in this
//! codebase: every API takes a `&Frame`, and the *current* frame is a name in
//! this set rather than a hidden global.
//!
//! Insertion-ordered, because `frames dir` prints it that way.
//!
//! # Not in v1
//!
//! `frlink` / `frget` / `frval`. They need a persisted join index and **alias
//! variables**, which is what `.dta` format **120** exists to encode: supporting
//! them means a fourth file format and a second kind of column that is a view
//! into another frame. A user who reaches for `frlink` gets an unimplemented
//! command, which is honest and detectable; a half-working `frlink` would be
//! worse.

use indexmap::IndexMap;
use std::sync::Arc;

use crate::frame::Frame;
use crate::perf::{CapacityError, MemoryPolicy};

/// The default frame's name at session start.
pub const DEFAULT: &str = "default";

/// What a frame-set operation refused to do.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum FrameSetError {
    /// `frame change nosuchframe`.
    #[error("frame {0} not found")]
    NotFound(String),
    /// `frame create` over an existing name.
    #[error("frame {0} already exists")]
    Exists(String),
    /// `frame drop default` while it is the current frame.
    #[error("cannot drop the current frame")]
    CurrentFrame,
    /// Not a legal frame name.
    #[error("invalid frame name {0}")]
    InvalidName(String),
}

/// Every frame in a session, plus which one is current.
#[derive(Debug)]
pub struct FrameSet {
    frames: IndexMap<Arc<str>, Frame>,
    current: Arc<str>,
}

impl Default for FrameSet {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameSet {
    /// A session with one empty frame called `default`.
    #[must_use]
    pub fn new() -> Self {
        let mut frames = IndexMap::new();
        let name: Arc<str> = Arc::from(DEFAULT);
        frames.insert(Arc::clone(&name), Frame::new(DEFAULT));
        Self {
            frames,
            current: name,
        }
    }

    /// The current frame's name.
    #[must_use]
    pub fn current_name(&self) -> &Arc<str> {
        &self.current
    }

    /// The current frame.
    ///
    /// # Panics
    ///
    /// Never: `current` is only ever set to a name this map contains, and
    /// `drop_frame` refuses to remove it.
    #[must_use]
    pub fn current(&self) -> &Frame {
        self.frames
            .get(&self.current)
            .expect("the current frame always exists")
    }

    /// The current frame, mutably.
    ///
    /// # Panics
    ///
    /// Never, for the reason [`current`](Self::current) gives.
    pub fn current_mut(&mut self) -> &mut Frame {
        self.frames
            .get_mut(&self.current)
            .expect("the current frame always exists")
    }

    /// Look a frame up by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Frame> {
        self.frames.get(name)
    }

    /// Look a frame up by name, mutably.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Frame> {
        self.frames.get_mut(name)
    }

    /// `frames dir`, in creation order.
    #[must_use]
    pub fn names(&self) -> Vec<Arc<str>> {
        self.frames.keys().cloned().collect()
    }

    /// How many frames exist.
    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Always false — a session always has at least the current frame.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// `frame create`.
    ///
    /// # Errors
    ///
    /// [`FrameSetError::Exists`] or [`FrameSetError::InvalidName`].
    pub fn create(&mut self, name: &str) -> Result<(), FrameSetError> {
        self.check_name(name)?;
        if self.frames.contains_key(name) {
            return Err(FrameSetError::Exists(name.to_owned()));
        }
        self.frames.insert(Arc::from(name), Frame::new(name));
        Ok(())
    }

    /// `frame change` / `cwf`.
    ///
    /// # Errors
    ///
    /// [`FrameSetError::NotFound`].
    pub fn change(&mut self, name: &str) -> Result<(), FrameSetError> {
        let Some((key, _)) = self.frames.get_key_value(name) else {
            return Err(FrameSetError::NotFound(name.to_owned()));
        };
        self.current = Arc::clone(key);
        Ok(())
    }

    /// `frame drop`.
    ///
    /// # Errors
    ///
    /// [`FrameSetError::NotFound`] or [`FrameSetError::CurrentFrame`].
    pub fn drop_frame(&mut self, name: &str) -> Result<(), FrameSetError> {
        if !self.frames.contains_key(name) {
            return Err(FrameSetError::NotFound(name.to_owned()));
        }
        if &*self.current == name {
            return Err(FrameSetError::CurrentFrame);
        }
        // `shift_remove`, not `swap_remove`: `frames dir` order is user-visible
        // and swapping would reorder the list behind their back.
        self.frames.shift_remove(name);
        Ok(())
    }

    /// `frame rename`.
    ///
    /// # Errors
    ///
    /// [`FrameSetError::NotFound`], [`FrameSetError::Exists`] or
    /// [`FrameSetError::InvalidName`].
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), FrameSetError> {
        self.check_name(to)?;
        if !self.frames.contains_key(from) {
            return Err(FrameSetError::NotFound(from.to_owned()));
        }
        if self.frames.contains_key(to) {
            return Err(FrameSetError::Exists(to.to_owned()));
        }
        let key: Arc<str> = Arc::from(to);
        // Rebuilt rather than removed-and-reinserted: `frames dir` order is
        // user-visible, and a reinsert would move the frame to the end.
        let frames = std::mem::take(&mut self.frames);
        self.frames = frames
            .into_iter()
            .map(|(k, mut v)| {
                if &*k == from {
                    v.set_name(Arc::clone(&key));
                    (Arc::clone(&key), v)
                } else {
                    (k, v)
                }
            })
            .collect();
        if &*self.current == from {
            self.current = key;
        }
        Ok(())
    }

    /// `frame copy`. O(nvars) pointer work — never a cell copy.
    ///
    /// # Errors
    ///
    /// [`FrameSetError::NotFound`], [`FrameSetError::Exists`] or
    /// [`FrameSetError::InvalidName`].
    pub fn copy(&mut self, from: &str, to: &str) -> Result<(), FrameSetError> {
        self.check_name(to)?;
        let Some(src) = self.frames.get(from) else {
            return Err(FrameSetError::NotFound(from.to_owned()));
        };
        if self.frames.contains_key(to) {
            return Err(FrameSetError::Exists(to.to_owned()));
        }
        let copy = src.copy(to);
        self.frames.insert(Arc::from(to), copy);
        Ok(())
    }

    /// Resident bytes over every frame — what [`MemoryPolicy::admit`] weighs a
    /// prospective `use` against.
    ///
    /// Columns shared between frames by `frame copy` are counted once per
    /// frame, so this is an upper bound on real residency. Over-counting is the
    /// safe direction for a refusal.
    #[must_use]
    pub fn resident_bytes(&self) -> u64 {
        self.frames.values().map(Frame::resident_bytes).sum()
    }

    /// Q9's gate: would loading `required` more bytes fit?
    ///
    /// # Errors
    ///
    /// [`CapacityError`], carrying required, resident and the ceiling, so the
    /// message can name all three instead of saying "out of memory".
    pub fn admit(&self, policy: &MemoryPolicy, required: u64) -> Result<(), CapacityError> {
        policy.admit(self.resident_bytes(), required)
    }

    fn check_name(&self, name: &str) -> Result<(), FrameSetError> {
        if crate::variable::is_valid_name(name) {
            Ok(())
        } else {
            Err(FrameSetError::InvalidName(name.to_owned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_proto::{StorageType, VarIdx};

    #[test]
    fn a_session_starts_with_one_frame_called_default() {
        let s = FrameSet::new();
        assert_eq!(s.len(), 1);
        assert_eq!(&**s.current_name(), DEFAULT);
        assert_eq!(s.current().n_vars(), 0);
    }

    #[test]
    fn frames_dir_keeps_creation_order_through_a_drop() {
        let mut s = FrameSet::new();
        s.create("alpha").expect("fresh");
        s.create("beta").expect("fresh");
        s.create("gamma").expect("fresh");
        s.drop_frame("beta").expect("exists and is not current");
        let names: Vec<String> = s.names().iter().map(|n| n.to_string()).collect();
        assert_eq!(names, vec!["default", "alpha", "gamma"]);
    }

    #[test]
    fn the_current_frame_cannot_be_dropped() {
        let mut s = FrameSet::new();
        s.create("alpha").expect("fresh");
        s.change("alpha").expect("exists");
        assert_eq!(s.drop_frame("alpha"), Err(FrameSetError::CurrentFrame));
        s.change("default").expect("exists");
        assert!(s.drop_frame("alpha").is_ok());
    }

    #[test]
    fn a_copy_shares_columns_until_one_is_written() {
        let mut s = FrameSet::new();
        {
            let f = s.current_mut();
            f.set_n_obs(4);
            f.add_var("x", StorageType::Double).expect("fresh");
            f.col_mut(VarIdx(0))
                .expect("exists")
                .set_f64(0, 42.0)
                .expect("double");
        }
        s.copy("default", "backup").expect("fresh name");
        let before = s
            .get("backup")
            .expect("copied")
            .col(VarIdx(0))
            .expect("column")
            .digest();
        s.current_mut()
            .col_mut(VarIdx(0))
            .expect("exists")
            .set_f64(0, 7.0)
            .expect("double");
        let after = s
            .get("backup")
            .expect("copied")
            .col(VarIdx(0))
            .expect("column")
            .digest();
        assert_eq!(before, after, "writing one frame must not touch the other");
        assert_eq!(
            s.get("backup")
                .expect("copied")
                .col(VarIdx(0))
                .expect("column")
                .get_f64(0),
            Some(42.0)
        );
    }

    #[test]
    fn renaming_the_current_frame_follows_it() {
        let mut s = FrameSet::new();
        s.create("alpha").expect("fresh");
        s.change("alpha").expect("exists");
        s.rename("alpha", "beta").expect("fresh target");
        assert_eq!(&**s.current_name(), "beta");
        assert!(s.get("alpha").is_none());
        let names: Vec<String> = s.names().iter().map(|n| n.to_string()).collect();
        assert_eq!(names, vec!["default", "beta"], "order survives a rename");
    }

    #[test]
    fn admission_refuses_with_the_numbers_attached() {
        let s = FrameSet::new();
        let policy = MemoryPolicy::default();
        policy.set_limit_bytes(1_024);
        let e = s
            .admit(&policy, 4_096)
            .expect_err("4 KiB over a 1 KiB limit");
        assert_eq!(e.required_bytes, 4_096);
        assert_eq!(CapacityError::RC, 909);
    }
}
