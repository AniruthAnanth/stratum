//! A bitset with the one API the sample machinery actually needs: *runs*.
//!
//! `04` §3.1: "`BitSet` is ours, a `Vec<u64>` with the run-extraction API we
//! actually need. We do not take `bitvec`: its generality costs compile time
//! and its API does not give us 'next run of set bits' cheaply."
//!
//! Runs are the whole point. `if year > 2010` on sorted panel data selects one
//! contiguous block; iterating it bit-by-bit would turn a memory-bandwidth-bound
//! kernel into a branch-per-row one. [`BitSet::runs`] finds that block with two
//! word scans and hands the kernel a slice range (`04` §5.3).

use crate::sample::Run;

/// A fixed-length bitset over `u64` words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BitSet {
    words: Vec<u64>,
    nbits: u64,
}

#[inline]
fn n_words(nbits: u64) -> usize {
    usize::try_from(nbits.div_ceil(64)).expect("a bitset longer than usize::MAX bits")
}

impl BitSet {
    /// All bits clear.
    #[must_use]
    pub fn new(nbits: u64) -> Self {
        Self {
            words: vec![0; n_words(nbits)],
            nbits,
        }
    }

    /// All bits set. The tail of the last word stays clear, which is what makes
    /// [`count_ones`](Self::count_ones) a plain popcount with no masking.
    #[must_use]
    pub fn all(nbits: u64) -> Self {
        let mut s = Self {
            words: vec![u64::MAX; n_words(nbits)],
            nbits,
        };
        s.mask_tail();
        s
    }

    fn mask_tail(&mut self) {
        let rem = self.nbits % 64;
        if rem != 0 {
            if let Some(last) = self.words.last_mut() {
                *last &= (1u64 << rem) - 1;
            }
        }
    }

    /// Number of bits, set or clear.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.nbits
    }

    /// True when the set holds no bits at all (not "no bits set").
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nbits == 0
    }

    /// The backing words, for a caller that wants to hash or fold them.
    #[must_use]
    pub fn words(&self) -> &[u64] {
        &self.words
    }

    /// Is bit `i` set? Out-of-range answers `false` rather than panicking: a
    /// sample built against a frame that has since shrunk must not take the
    /// process down.
    #[inline]
    #[must_use]
    pub fn get(&self, i: u64) -> bool {
        if i >= self.nbits {
            return false;
        }
        let (w, b) = ((i / 64) as usize, i % 64);
        self.words[w] >> b & 1 == 1
    }

    /// Set bit `i`. Out-of-range is ignored, for the reason [`get`](Self::get)
    /// gives.
    #[inline]
    pub fn set(&mut self, i: u64, v: bool) {
        if i >= self.nbits {
            return;
        }
        let (w, b) = ((i / 64) as usize, i % 64);
        if v {
            self.words[w] |= 1u64 << b;
        } else {
            self.words[w] &= !(1u64 << b);
        }
    }

    /// Set every bit in the half-open range. One `memset`-shaped loop over
    /// whole words plus two partial words, never a loop over bits.
    pub fn set_range(&mut self, start: u64, end: u64) {
        let end = end.min(self.nbits);
        if start >= end {
            return;
        }
        let (fw, lw) = ((start / 64) as usize, ((end - 1) / 64) as usize);
        if fw == lw {
            let lo = start % 64;
            let hi = (end - 1) % 64;
            let mask = if hi == 63 {
                u64::MAX << lo
            } else {
                ((1u64 << (hi + 1)) - 1) & (u64::MAX << lo)
            };
            self.words[fw] |= mask;
            return;
        }
        self.words[fw] |= u64::MAX << (start % 64);
        for w in &mut self.words[fw + 1..lw] {
            *w = u64::MAX;
        }
        let hi = (end - 1) % 64;
        self.words[lw] |= if hi == 63 {
            u64::MAX
        } else {
            (1u64 << (hi + 1)) - 1
        };
    }

    /// How many bits are set.
    #[must_use]
    pub fn count_ones(&self) -> u64 {
        self.words.iter().map(|w| u64::from(w.count_ones())).sum()
    }

    /// In-place intersection. `other` shorter than `self` clears the remainder,
    /// which is the semantics `markout` on a narrowed sample wants.
    pub fn and_assign(&mut self, other: &BitSet) {
        for (i, w) in self.words.iter_mut().enumerate() {
            *w &= other.words.get(i).copied().unwrap_or(0);
        }
    }

    /// Index of the first set bit at or after `from`.
    #[must_use]
    pub fn next_set(&self, from: u64) -> Option<u64> {
        if from >= self.nbits {
            return None;
        }
        let mut w = (from / 64) as usize;
        let mut word = self.words[w] & (u64::MAX << (from % 64));
        loop {
            if word != 0 {
                let bit = w as u64 * 64 + u64::from(word.trailing_zeros());
                return (bit < self.nbits).then_some(bit);
            }
            w += 1;
            if w >= self.words.len() {
                return None;
            }
            word = self.words[w];
        }
    }

    /// Index of the first clear bit at or after `from`, saturating at `len()`.
    #[must_use]
    pub fn next_clear(&self, from: u64) -> u64 {
        if from >= self.nbits {
            return self.nbits;
        }
        let mut w = (from / 64) as usize;
        // Pretend the bits below `from` are set so `trailing_ones` skips them.
        let mut word = self.words[w] | !(u64::MAX << (from % 64));
        loop {
            if word != u64::MAX {
                let bit = w as u64 * 64 + u64::from(word.trailing_ones());
                return bit.min(self.nbits);
            }
            w += 1;
            if w >= self.words.len() {
                return self.nbits;
            }
            word = self.words[w];
        }
    }

    /// The maximal contiguous runs of set bits, ascending.
    #[must_use]
    pub fn runs(&self) -> BitRuns<'_> {
        BitRuns { bits: self, pos: 0 }
    }
}

/// Iterator over [`BitSet::runs`].
#[derive(Clone, Debug)]
pub struct BitRuns<'a> {
    bits: &'a BitSet,
    pos: u64,
}

impl Iterator for BitRuns<'_> {
    type Item = Run;

    fn next(&mut self) -> Option<Run> {
        let start = self.bits.next_set(self.pos)?;
        let end = self.bits.next_clear(start);
        self.pos = end;
        Some(Run {
            start,
            len: end - start,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_masks_its_tail_so_popcount_is_exact() {
        for n in [0u64, 1, 63, 64, 65, 1000] {
            assert_eq!(BitSet::all(n).count_ones(), n, "n = {n}");
        }
    }

    #[test]
    fn a_contiguous_selection_is_exactly_one_run() {
        // The `if year > 2010` case from `04` §5.3.
        let mut b = BitSet::new(1000);
        b.set_range(300, 700);
        let runs: Vec<Run> = b.runs().collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].start, 300);
        assert_eq!(runs[0].len, 400);
        assert_eq!(b.count_ones(), 400);
    }

    #[test]
    fn runs_reproduce_the_set_bits_exactly() {
        let mut b = BitSet::new(500);
        let want: Vec<u64> = (0..500)
            .filter(|i| i % 7 == 0 || (100..103).contains(i))
            .collect();
        for &i in &want {
            b.set(i, true);
        }
        let mut got = Vec::new();
        for r in b.runs() {
            got.extend(r.start..r.start + r.len);
        }
        assert_eq!(got, want);
    }

    #[test]
    fn set_range_spans_word_boundaries() {
        let mut b = BitSet::new(200);
        b.set_range(63, 129);
        assert_eq!(b.count_ones(), 66);
        assert!(!b.get(62) && b.get(63) && b.get(128) && !b.get(129));
    }

    #[test]
    fn out_of_range_access_is_false_not_a_panic() {
        let mut b = BitSet::new(10);
        b.set(99, true);
        assert!(!b.get(99));
        assert_eq!(b.count_ones(), 0);
    }
}
