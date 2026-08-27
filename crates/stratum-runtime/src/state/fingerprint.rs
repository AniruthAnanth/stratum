//! The 128-bit accumulator, and the whole-session fingerprint — `03` §§4.5, 4.6.
//!
//! Two ideas carry this module.
//!
//! **The accumulator makes interning O(changed).** `acc` is an XOR fold over
//! every `(name, version)` pair a state contains. XOR is commutative and
//! self-inverse, so a command that bumps one column updates it with two mixes
//! and no walk — `acc ^= mix(old); acc ^= mix(new)`. It is an *index*, never an
//! answer: an interning hit is always verified by full structural equality, so a
//! collision costs a comparison and cannot cost a wrong `DatasetStateId`.
//!
//! **Completeness is the invariant.** [`StateFingerprint`] enumerates everything
//! a block can depend on. INV-1 ("a block shown ✓ Current was produced by
//! exactly this code against exactly this state") holds *because* this list is
//! complete; a namespace missing from it is a silent under-marking bug, which is
//! the research-integrity failure ADR-008 exists to prevent. Anything that
//! cannot be versioned — `shell`, `python`, a plugin — sets `Taint::EXTERNAL`
//! instead and downgrades the block to `CurrentUnverifiable`.

use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use stratum_proto::{ColumnDigest, ExecutionId, FrameId, SessionEpoch, StateId, VarId};

use crate::state::dataset::DatasetFingerprint;

/// The 128-bit multiply-xor mixer's odd multiplier (`03` §4.5).
///
/// **Stable across releases, by contract.** It is part of how a
/// `DatasetStateId` recurs, and a `.workspace` sidecar written by one build is
/// read by the next. [`tests::the_mixer_is_pinned`] holds it.
const MIX_MULT: u128 = 0x9E3779B97F4A7C15_F39CC0605CEDC835;

/// A 128-bit XOR-fold over a state's `(entry, version)` pairs — `03` §4.5.
///
/// 128 bits puts the birthday bound over 2^64 distinct states, and we never rely
/// on it alone: see the module header.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
#[repr(transparent)]
pub struct FingerprintAcc(pub u128);

impl FingerprintAcc {
    /// Fold one column's `(VarId, gen)` in — or, applied twice, back out.
    #[inline]
    pub fn toggle_var(&mut self, var: VarId, gen: u32) {
        self.0 ^= mix_var(var, gen);
    }

    /// Replace a column's contribution in O(1).
    #[inline]
    pub fn revise_var(&mut self, var: VarId, old_gen: u32, new_gen: u32) {
        self.toggle_var(var, old_gen);
        self.toggle_var(var, new_gen);
    }

    /// Fold one named namespace entry in, or back out.
    #[inline]
    pub fn toggle_named(&mut self, ns: Ns, name: &str, version: u64) {
        self.0 ^= mix_named(ns, name, version);
    }

    /// Replace a named entry's contribution in O(1).
    #[inline]
    pub fn revise_named(&mut self, ns: Ns, name: &str, old: u64, new: u64) {
        self.toggle_named(ns, name, old);
        self.toggle_named(ns, name, new);
    }

    /// Fold a whole-namespace counter (`e()`, `r()`, `cwd`, …) in or back out.
    #[inline]
    pub fn toggle_scalar(&mut self, ns: Ns, version: u64) {
        self.0 ^= mix_named(ns, "", version);
    }

    /// Replace a whole-namespace counter's contribution.
    #[inline]
    pub fn revise_scalar(&mut self, ns: Ns, old: u64, new: u64) {
        self.toggle_scalar(ns, old);
        self.toggle_scalar(ns, new);
    }
}

/// Which namespace a mixed entry belongs to.
///
/// The tag is in the hash input so `local x` and `scalar x` at the same version
/// do not cancel each other out of the fold.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Ns {
    /// `$name`.
    Global = 1,
    /// `` `name' `` in the top scope.
    Local = 2,
    /// `scalar name`.
    Scalar = 3,
    /// `matrix name`.
    Matrix = 4,
    /// `program define name`.
    Program = 5,
    /// `char` on the dataset or a variable.
    Chars = 6,
    /// `e()` plus the `estimates store` table.
    Estimates = 7,
    /// `r()`.
    RClass = 8,
    /// `s()`.
    SClass = 9,
    /// `c()` / `set`.
    Setting = 10,
    /// The random-number stream.
    Rng = 11,
    /// The working directory.
    Cwd = 12,
    /// The tempname counter.
    Tempname = 13,
    /// An external input file.
    File = 14,
    /// A frame's dataset fingerprint, folded into the session accumulator.
    Frame = 15,
}

/// `03` §4.5, transcribed. Any strong 128-bit mixer would do; this one is fixed
/// so that a state id recurs across releases.
#[inline]
#[must_use]
pub fn mix_var(var: VarId, gen: u32) -> u128 {
    let x = ((var.0 as u128) << 32) | gen as u128;
    let x = x.wrapping_mul(MIX_MULT);
    x ^ (x >> 61)
}

/// The same fold for a string-keyed namespace.
///
/// The same multiply-xor as [`mix_var`], extended over bytes. blake3 would be
/// the obvious reach — it is the crate's hash everywhere else (CONTRACTS §1.1)
/// — but this is a fold *index*, never an answer: an interning hit is always
/// verified by full structural equality, so the strength that matters is
/// avalanche, not collision resistance against an adversary. Keeping it here
/// also keeps `blake3` out of `stratum-runtime`'s dependency list, which is
/// W06a's file.
///
/// **Version 0 contributes nothing.** 0 is the version of a name that does not
/// exist ([`StateFingerprint::named`]), and the definitional fold in
/// [`StateFingerprint::recompute_acc`] walks only names that *do* exist. If
/// version 0 mixed to something non-zero, the incremental path — which toggles
/// the old value out before toggling the new one in — would carry a term for
/// every name ever created and the two folds would disagree. Making the identity
/// element mix to the identity value is what keeps them equal by construction.
#[inline]
#[must_use]
pub fn mix_named(ns: Ns, name: &str, version: u64) -> u128 {
    if version == 0 {
        return 0;
    }
    let mut h = (u128::from(ns as u8) << 64) | u128::from(version);
    h = h.wrapping_mul(MIX_MULT);
    h ^= h >> 61;
    for b in name.as_bytes() {
        h = (h ^ u128::from(*b)).wrapping_mul(MIX_MULT);
        h ^= h >> 61;
    }
    h
}

/// The same mixer over an opaque 128-bit payload, for entries whose "version" is
/// itself a digest ([`RngFingerprint::key`], [`FileStamp::key`], a frame's
/// accumulator).
#[inline]
#[must_use]
fn mix_payload(ns: Ns, tag: u64, payload: u128) -> u128 {
    let mut h = (u128::from(ns as u8) << 64) | u128::from(tag);
    h = h.wrapping_mul(MIX_MULT);
    h ^= h >> 61;
    h = (h ^ payload).wrapping_mul(MIX_MULT);
    h ^ (h >> 61)
}

/// A monotone version counter for one named entry in a session namespace.
///
/// `03` §12 records an open question — should macros converge on value the way
/// columns converge on content? We version by assignment, so `local x = 1` twice
/// bumps twice. That over-marks, which INV-1 permits; the opposite would not.
pub type NameVersions = Arc<FxHashMap<Box<str>, u64>>;

/// The random-number stream's identity — `03` §4.6.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RngFingerprint {
    /// Which generator is selected.
    pub kind: RngKind,
    /// The execution that last ran `set seed`.
    pub seed_origin: ExecutionId,
    /// The seed it set.
    pub seed_value: u64,
    /// Draws consumed since that seed. This is why re-running an RNG block
    /// downstream of an inserted draw is correctly marked stale.
    pub draws: u64,
    /// `set sortseed`.
    pub sortseed: u64,
}

impl RngFingerprint {
    /// The clean-state stream: `set seed 123456789`, zero draws (W08b's
    /// checklist item, `03` §8).
    pub const DEFAULT_SEED: u64 = 123_456_789;

    /// The stream a fresh session starts with.
    #[must_use]
    pub fn fresh() -> Self {
        Self {
            kind: RngKind::Mt64,
            seed_origin: ExecutionId(0),
            seed_value: Self::DEFAULT_SEED,
            draws: 0,
            sortseed: 0,
        }
    }

    /// A 64-bit digest of the stream, for folding into an accumulator and for
    /// `DepKey::Rng`'s `u64` version slot.
    ///
    /// Never 0 for a real stream: 0 is the "absent" version that
    /// [`mix_named`] maps to the identity, and the RNG is never absent.
    #[must_use]
    pub fn key(&self) -> u64 {
        let mut h = mix_payload(Ns::Rng, self.kind as u64, u128::from(self.seed_value));
        h = mix_payload(Ns::Rng, self.seed_origin.0, h);
        h = mix_payload(Ns::Rng, self.draws, h);
        h = mix_payload(Ns::Rng, self.sortseed, h);
        ((h >> 64) as u64) | 1
    }
}

/// Stata's three generators.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum RngKind {
    /// `set rng mt64` — the default.
    Mt64 = 1,
    /// `set rng mt64s`.
    Mt64s = 2,
    /// `set rng kiss32`.
    Kiss32 = 3,
}

/// Key for an external input file. The path as the user wrote it is not enough —
/// two spellings of one file must be one dependency.
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PathKey(pub Utf8PathBuf);

impl PathKey {
    /// Wrap a path that the caller has already resolved against the session cwd.
    ///
    /// Resolution is deliberately not done here: reaching the filesystem is
    /// `ExecCtx`'s monopoly (ARCHITECTURE §5) precisely so that every ambient
    /// read is recorded, and a `canonicalize` hidden in a hash key would be an
    /// unrecorded one.
    #[must_use]
    pub fn new(resolved: impl AsRef<Utf8Path>) -> Self {
        Self(resolved.as_ref().to_owned())
    }
}

impl std::fmt::Display for PathKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// What we know about an external input at the moment a block read it.
///
/// `digest` is filled only below [`FILE_DIGEST_MAX_BYTES`]; above it the stamp
/// is `(mtime, size, inode)`, which is exactly the guarantee `make` gives, and
/// `03` §4.6 requires the UI to say so rather than imply a content check.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FileStamp {
    /// Modification time in nanoseconds since the epoch.
    pub mtime_ns: i128,
    /// Size in bytes.
    pub size: u64,
    /// Inode, or 0 where the platform has none.
    pub inode: u64,
    /// blake3-128 of the contents, when the file was small enough to read.
    pub digest: Option<ColumnDigest>,
}

/// Files at or below this size get a real content digest (`03` §4.6), so
/// `use auto.dta` is content-checked and a 40 GB panel is not.
pub const FILE_DIGEST_MAX_BYTES: u64 = 32 * 1024 * 1024;

impl FileStamp {
    /// Build a stamp from metadata the caller has already read.
    #[must_use]
    pub fn from_parts(mtime_ns: i128, size: u64, inode: u64, digest: Option<ColumnDigest>) -> Self {
        Self {
            mtime_ns,
            size,
            inode,
            digest,
        }
    }

    /// Has the file changed since this stamp was taken?
    ///
    /// One-directional on purpose: when both stamps carry a digest the digest
    /// decides (so a touched-but-identical file does not restale a block), and
    /// otherwise any metadata difference counts as changed. Never the reverse.
    #[must_use]
    pub fn differs_from(&self, now: &FileStamp) -> bool {
        match (self.digest, now.digest) {
            (Some(a), Some(b)) => a != b,
            _ => self.mtime_ns != now.mtime_ns || self.size != now.size || self.inode != now.inode,
        }
    }

    /// A 64-bit digest of the stamp, for folding into an accumulator and for
    /// `DepKey::File`'s `u64` version slot.
    ///
    /// Never 0, for the reason [`RngFingerprint::key`] gives.
    #[must_use]
    pub fn key(&self) -> u64 {
        let mut h = mix_payload(Ns::File, self.size, self.mtime_ns as u128);
        h = mix_payload(Ns::File, self.inode, h);
        if let Some(ColumnDigest(d)) = self.digest {
            h = mix_payload(Ns::File, 1, u128::from_le_bytes(d) ^ h);
        }
        ((h >> 64) as u64) | 1
    }
}

/// Everything a block can depend on — `03` §4.6.
///
/// One of these is retained per execution. Every map is behind an `Arc` and is
/// cloned on write, so an execution that touches one macro shares every other
/// namespace with its predecessor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateFingerprint {
    /// Interned identity. Assigned by [`crate::state::SessionState::intern`].
    pub id: StateId,
    /// Bumps on clear-all and on a clean run.
    pub epoch: SessionEpoch,

    /// One fingerprint per live frame.
    pub frames: Arc<FxHashMap<FrameId, DatasetFingerprint>>,
    /// The frame commands run against.
    pub current_frame: FrameId,

    /// `$name` → version.
    pub globals: NameVersions,
    /// `` `name' `` in the top scope → version.
    pub locals: NameVersions,
    /// `scalar name` → version.
    pub scalars: NameVersions,
    /// `matrix name` → version.
    pub matrices: NameVersions,
    /// `program define name` → version.
    pub programs: NameVersions,
    /// `char` on the dataset or any variable.
    pub chars: u64,

    /// `e()` and the `estimates store` table.
    pub estimates: u64,
    /// `r()`.
    pub rclass: u64,
    /// `s()`.
    pub sclass: u64,

    /// The random-number stream.
    pub rng: RngFingerprint,
    /// `c()` / `set` state, keyed by the name `DepKey::Setting` uses.
    pub settings: NameVersions,
    /// Bumps on `cd`.
    pub cwd: u64,
    /// `tempvar`/`tempname`/`tempfile` allocation counter.
    pub tempname_counter: u64,

    /// External inputs. What makes "the .dta on disk changed under me" a
    /// first-class staleness source — a case Jupyter cannot see at all.
    pub files: Arc<FxHashMap<PathKey, FileStamp>>,
    /// The XOR fold over everything above.
    pub acc: FingerprintAcc,
}

impl StateFingerprint {
    /// The fingerprint of a session that has run nothing.
    #[must_use]
    pub fn fresh(epoch: SessionEpoch, current_frame: FrameId) -> Self {
        let mut fp = Self {
            id: StateId(0),
            epoch,
            frames: Arc::default(),
            current_frame,
            globals: Arc::default(),
            locals: Arc::default(),
            scalars: Arc::default(),
            matrices: Arc::default(),
            programs: Arc::default(),
            chars: 0,
            estimates: 0,
            rclass: 0,
            sclass: 0,
            rng: RngFingerprint::fresh(),
            settings: Arc::default(),
            cwd: 0,
            tempname_counter: 0,
            files: Arc::default(),
            acc: FingerprintAcc::default(),
        };
        fp.acc = fp.recompute_acc();
        fp
    }

    /// The version of one named entry, or 0 when it does not exist.
    ///
    /// 0 for absent is deliberate and load-bearing: a block that read
    /// `` `undefined' `` (which Stata expands to nothing, not an error) depends
    /// on that macro *staying* undefined, and defining it later must restale the
    /// block. An `Option` here would have let the recorder drop the dependency.
    #[must_use]
    pub fn named(&self, ns: Ns, name: &str) -> u64 {
        let map = match ns {
            Ns::Global => &self.globals,
            Ns::Local => &self.locals,
            Ns::Scalar => &self.scalars,
            Ns::Matrix => &self.matrices,
            Ns::Program => &self.programs,
            Ns::Setting => &self.settings,
            _ => return self.scalar_ns(ns),
        };
        map.get(name).copied().unwrap_or(0)
    }

    /// The counter for a namespace that has no names of its own.
    #[must_use]
    pub fn scalar_ns(&self, ns: Ns) -> u64 {
        match ns {
            Ns::Chars => self.chars,
            Ns::Estimates => self.estimates,
            Ns::RClass => self.rclass,
            Ns::SClass => self.sclass,
            Ns::Cwd => self.cwd,
            Ns::Tempname => self.tempname_counter,
            Ns::Rng => self.rng.key(),
            _ => 0,
        }
    }

    /// Bump one named entry and keep `acc` in step. Returns the new version.
    pub fn bump_named(&mut self, ns: Ns, name: &str) -> u64 {
        let map = match ns {
            Ns::Global => &mut self.globals,
            Ns::Local => &mut self.locals,
            Ns::Scalar => &mut self.scalars,
            Ns::Matrix => &mut self.matrices,
            Ns::Program => &mut self.programs,
            Ns::Setting => &mut self.settings,
            other => {
                let old = self.scalar_ns(other);
                let new = old + 1;
                self.set_scalar_ns(other, new);
                self.acc.revise_scalar(other, old, new);
                return new;
            }
        };
        let m = Arc::make_mut(map);
        let old = m.get(name).copied().unwrap_or(0);
        let new = old + 1;
        m.insert(name.into(), new);
        self.acc.revise_named(ns, name, old, new);
        new
    }

    /// Forget one named entry (`macro drop`, `scalar drop`, `program drop`).
    ///
    /// It is a *bump*, not a removal, because "was 3, now gone" and "never
    /// existed" must not be the same version — the second would let a block that
    /// read the macro before it was dropped stay Current.
    pub fn drop_named(&mut self, ns: Ns, name: &str) {
        self.bump_named(ns, name);
    }

    /// Set a whole-namespace counter directly, keeping `acc` in step.
    ///
    /// The stored-result singletons own their own counters
    /// ([`crate::results::StoredResults`]) and push them here at commit; going
    /// through a setter rather than a bump means the two cannot drift when a
    /// command forgets a call.
    pub fn set_ns(&mut self, ns: Ns, v: u64) {
        let old = self.scalar_ns(ns);
        if old == v {
            return;
        }
        self.set_scalar_ns(ns, v);
        self.acc.revise_scalar(ns, old, v);
    }

    fn set_scalar_ns(&mut self, ns: Ns, v: u64) {
        match ns {
            Ns::Chars => self.chars = v,
            Ns::Estimates => self.estimates = v,
            Ns::RClass => self.rclass = v,
            Ns::SClass => self.sclass = v,
            Ns::Cwd => self.cwd = v,
            Ns::Tempname => self.tempname_counter = v,
            _ => {}
        }
    }

    /// Record a new random-number stream state.
    pub fn set_rng(&mut self, rng: RngFingerprint) {
        self.acc.revise_scalar(Ns::Rng, self.rng.key(), rng.key());
        self.rng = rng;
    }

    /// Record an external input's stamp.
    pub fn stamp_file(&mut self, path: PathKey, stamp: FileStamp) {
        let files = Arc::make_mut(&mut self.files);
        let old = files.get(&path).map_or(0, FileStamp::key);
        self.acc
            .revise_named(Ns::File, path.0.as_str(), old, stamp.key());
        files.insert(path, stamp);
    }

    /// Install a frame's dataset fingerprint, folding its accumulator in.
    ///
    /// A frame contributes its own 128-bit accumulator, keyed by frame id so
    /// that two frames holding identical data do not cancel each other out of
    /// the fold. An *absent* frame contributes nothing — the same identity-
    /// element rule [`mix_named`] documents, and for the same reason: the
    /// definitional fold walks only frames that exist, so toggling a
    /// "`mix_frame(id, 0)`" term out on first insert would leave the two
    /// permanently apart.
    pub fn set_frame(&mut self, frame: FrameId, fp: DatasetFingerprint) {
        let frames = Arc::make_mut(&mut self.frames);
        if let Some(old) = frames.get(&frame) {
            self.acc.0 ^= mix_frame(frame, old.acc.0);
        }
        self.acc.0 ^= mix_frame(frame, fp.acc.0);
        frames.insert(frame, fp);
    }

    /// Drop a frame (`frame drop`).
    pub fn remove_frame(&mut self, frame: FrameId) {
        let frames = Arc::make_mut(&mut self.frames);
        if let Some(f) = frames.remove(&frame) {
            self.acc.0 ^= mix_frame(frame, f.acc.0);
        }
    }

    /// The current frame's fingerprint.
    #[must_use]
    pub fn current(&self) -> Option<&DatasetFingerprint> {
        self.frames.get(&self.current_frame)
    }

    /// Recompute `acc` from scratch.
    ///
    /// Only construction and the debug assertion in
    /// [`crate::state::SessionState`] call this: the whole point of the fold is
    /// that the incremental path never needs it. It exists so a test can prove
    /// the incremental path agrees with the definition.
    #[must_use]
    pub fn recompute_acc(&self) -> FingerprintAcc {
        let mut a = FingerprintAcc::default();
        for (frame, fp) in self.frames.iter() {
            a.0 ^= mix_frame(*frame, fp.acc.0);
        }
        for (ns, map) in [
            (Ns::Global, &self.globals),
            (Ns::Local, &self.locals),
            (Ns::Scalar, &self.scalars),
            (Ns::Matrix, &self.matrices),
            (Ns::Program, &self.programs),
            (Ns::Setting, &self.settings),
        ] {
            for (name, v) in map.iter() {
                a.toggle_named(ns, name, *v);
            }
        }
        for ns in [
            Ns::Chars,
            Ns::Estimates,
            Ns::RClass,
            Ns::SClass,
            Ns::Cwd,
            Ns::Tempname,
            Ns::Rng,
        ] {
            a.toggle_scalar(ns, self.scalar_ns(ns));
        }
        for (p, s) in self.files.iter() {
            a.toggle_named(Ns::File, p.0.as_str(), s.key());
        }
        a
    }
}

#[inline]
fn mix_frame(frame: FrameId, acc: u128) -> u128 {
    mix_payload(Ns::Frame, u64::from(frame.0), acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mixer_is_pinned() {
        // A `DatasetStateId` must recur across releases (`03` §4.4), and a
        // sidecar written by one build is read by the next. Changing either
        // constant is therefore a schema break, and this test is where it gets
        // noticed.
        assert_eq!(MIX_MULT, 0x9E3779B97F4A7C15_F39CC0605CEDC835);
        assert_eq!(mix_var(VarId(0), 0), 0);
        assert_eq!(
            mix_var(VarId(1), 0),
            0x7F4A_7C15_F39C_C063_A6BE_289A_9CE6_0302
        );
        assert_eq!(
            mix_var(VarId(0), 1),
            0x9E37_79B9_7F4A_7C11_0227_0DAB_A6BE_289A
        );
        assert_eq!(
            mix_var(VarId(3), 2),
            0xBA4E_67B4_D96B_3949_2C71_E4F9_7282_5A0D
        );
    }

    #[test]
    fn the_fold_is_order_independent_and_reversible() {
        let mut a = FingerprintAcc::default();
        a.toggle_var(VarId(1), 0);
        a.toggle_var(VarId(2), 7);
        let mut b = FingerprintAcc::default();
        b.toggle_var(VarId(2), 7);
        b.toggle_var(VarId(1), 0);
        assert_eq!(a, b);
        a.toggle_var(VarId(2), 7);
        a.toggle_var(VarId(1), 0);
        assert_eq!(a, FingerprintAcc::default());
    }

    #[test]
    fn a_namespace_tag_stops_two_namespaces_cancelling() {
        let mut a = FingerprintAcc::default();
        a.toggle_named(Ns::Local, "x", 1);
        a.toggle_named(Ns::Scalar, "x", 1);
        assert_ne!(a, FingerprintAcc::default());
    }

    #[test]
    fn the_incremental_fold_agrees_with_the_definition() {
        let mut fp = StateFingerprint::fresh(SessionEpoch(1), FrameId(0));
        fp.bump_named(Ns::Global, "root");
        fp.bump_named(Ns::Local, "i");
        fp.bump_named(Ns::Local, "i");
        fp.bump_named(Ns::Setting, "type");
        fp.bump_named(Ns::Estimates, "");
        fp.set_rng(RngFingerprint {
            draws: 12,
            ..RngFingerprint::fresh()
        });
        fp.stamp_file(
            PathKey::new("/data/auto.dta"),
            FileStamp::from_parts(17, 4096, 9, None),
        );
        assert_eq!(fp.acc, fp.recompute_acc());
    }

    #[test]
    fn an_absent_name_is_version_zero_and_defining_it_moves_the_fold() {
        let mut fp = StateFingerprint::fresh(SessionEpoch(1), FrameId(0));
        assert_eq!(fp.named(Ns::Local, "never_set"), 0);
        let before = fp.acc;
        fp.bump_named(Ns::Local, "never_set");
        assert_ne!(
            fp.acc, before,
            "defining a macro a block read must restale it"
        );
    }

    #[test]
    fn dropping_a_name_is_not_the_same_state_as_never_having_had_it() {
        let mut a = StateFingerprint::fresh(SessionEpoch(1), FrameId(0));
        let virgin = a.acc;
        a.bump_named(Ns::Global, "path");
        a.drop_named(Ns::Global, "path");
        assert_ne!(a.acc, virgin);
    }

    #[test]
    fn a_digested_file_that_was_only_touched_is_not_a_change() {
        let d = Some(ColumnDigest([7; 16]));
        let a = FileStamp::from_parts(1, 100, 5, d);
        let b = FileStamp::from_parts(999, 100, 5, d);
        assert!(!a.differs_from(&b));
        let c = FileStamp::from_parts(1, 100, 5, Some(ColumnDigest([8; 16])));
        assert!(a.differs_from(&c));
        // Without a digest, metadata decides — the `make` guarantee.
        let e = FileStamp::from_parts(1, 100, 5, None);
        let f = FileStamp::from_parts(2, 100, 5, None);
        assert!(e.differs_from(&f));
    }
}
