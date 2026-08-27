//! Segmentation throughput and the incremental gate — design 02 §5.5, A25.
//!
//! Four measurements, and the last two are the ones that decide whether the
//! editor is usable:
//!
//! * `cold/*` — a full pass. Design 02 §5.5 asks for **> 250 MB/s** and a 1 MB
//!   do-file re-segmenting in **< 4 ms**.
//! * `incremental/append` — the pre-audit gate: a small edit at the END of a
//!   2 MB file, where "rescan from the last region before the edit" is trivially
//!   fast. Kept so the two can be compared.
//! * `incremental/five_percent` — the A25 gate: three statements inserted 5 %
//!   into the same file, which is where the naive rule rescans ~2 MB per
//!   keystroke. The budget is **< 250 µs**, and the correctness half (≤ 8 regions
//!   re-hashed) is asserted in `tests/resegment.rs`, not here — a wall-clock
//!   number cannot tell a fast wrong answer from a fast right one.
//! * `incremental/typing` — one character typed inside an existing command at
//!   5 %, which is what a keystroke usually is. No region and no logical line is
//!   created, so nothing is inserted into either vector; the difference between
//!   this and `five_percent` is exactly what the insertion costs.
//!
//! * `tail/*` — the A/B behind `REBASE_CHUNK`: the same 2 MB region tail moved
//!   three slots and rebased, five ways. Not a gate; it is why `splice_rebase`
//!   is shaped the way it is, and it is here rather than in a comment because
//!   the answer changed twice as `Region` shrank.
//!
//! # The incremental gate is met; the throughput gate is not
//!
//! The targets are left exactly as the plan states them; the `floor/*` group
//! exists so that what is left is a measurement rather than an opinion. Every
//! bench there does a STRICT SUBSET of the work the gate above it requires — no
//! scanning, no grouping, no allocation — so whatever it reports is a lower
//! bound the implementation cannot go under.
//!
//! ## `< 250 µs` for a 3-region insert 5 % into 2 MB: **met, 226 µs**
//!
//! What is left after the rescan is not rescanning: it is moving and rebasing
//! the 95 % of the region vector and the line vector that the edit did not
//! touch, and that cost is BYTES. Three things bought the last 50 µs:
//!
//! * `LogicalLine` is 48 bytes and `Copy`. Its `Box<Derived>` moved out to a
//!   parallel [`stratum_parse::DerivedText`] table whose piece maps are relative
//!   to the line, so that table needs no rebase at all and moves with one
//!   `Vec::splice`. An element with drop glue cannot be bit-moved in safe code,
//!   and the line tail was paying 159 µs to be lifted out of its slots one at a
//!   time against a 59 µs `memmove` floor.
//! * `Region` is 88 bytes: `HeadInfo` holds a `CmdId` instead of a
//!   `&'static CommandSig`, which also drops the type's alignment to 4.
//! * The rebase is eight `u32` wrapping adds with the rare cases (`ord_delta`
//!   renumbering, diagnostic indices) hoisted out of the loop, and it runs a
//!   chunk behind `copy_within` while the chunk is still in L1.
//!
//! `floor/memmove_*_tail_2mb` is what those two moves cost with no rebase at
//! all — 69 µs and 59 µs — so the pass is now within ~45 % of `memmove` and the
//! next lever really is the one 02 §5.5 names: chunked or base-relative storage,
//! which moves cost onto the read path `stratum-exec` walks. It is no longer
//! needed for the gate.
//!
//! ## `> 250 MB/s`, i.e. 1 MB in < 4 ms: **missed, 9.5 ms (105 MiB/s)**
//!
//! Missed by 2.4x, and BOTH halves of the pass are over the budget on their own:
//!
//! * **The hash alone is the whole budget.** `CodeHash` is normatively blake3-128
//!   over a per-region canonical token stream, and CONTRACTS §1.2 rule 6 fixes
//!   the encoding at five header bytes per token. The 1 MB corpus is 21 438
//!   regions and 346 581 tokens, so that encoding is 2.6 MB — **2.49x the
//!   source, two thirds of it length prefixes** — landing in 21 438 separate
//!   blake3 instances. `floor/blake3_canonical_1mb` hashes exactly those buffers,
//!   pre-built, and does nothing else: **4.0 ms**, before a byte is scanned.
//!   `floor/blake3_one_stream_1mb` hashes the same bytes as ONE input, so the gap
//!   between the two is what "many small hashes" costs and the rest is blake3's
//!   own rate here. aarch64 is what makes it that expensive: the `blake3` crate
//!   accelerates `hash_many` (multiple chunks) with NEON and has **no vectorised
//!   single-compression path**, so a sub-kilobyte input runs the portable code —
//!   `phase/blake3_64` is 73 ns for ONE 64-byte block.
//! * **The scanner alone is over too.** Subtract the hash and the pass is still
//!   5.5 ms, i.e. 190 MB/s: `phase/read_lines_only` 1.73 ms,
//!   `phase/canonicalise_all_regions` 1.30 ms, the rule-6 encode ~0.85 ms
//!   (`phase/hash_all_regions` minus the other two), `phase/line_index` 0.18 ms,
//!   and ~1.5 ms of grouping, head parsing, marker scanning and per-region
//!   `hash_ordinal` bookkeeping.
//!
//! So a cheaper `CodeHash` alone does not reach 4 ms and a faster scanner alone
//! does not either. **This needs an architect ruling on both numbers** — see the
//! unit report; it is not a tuning question at this scale.
//!
//! # Where the implementation stands
//!
//! Measured back to back on one machine:
//!
//! | | wave gate | now |
//! |---|---|---|
//! | `incremental/five_percent` (gate: < 250 µs) | 277–295 µs | **226 µs** |
//! | `incremental/typing`                        | 149–153 µs | 123 µs |
//! | `incremental/middle`                        | 199–205 µs | 162 µs |
//! | `incremental/append`                        | 10.3 µs    | 3.2 µs |
//! | `cold/1024` (gate: < 4 ms)                  | 9.6–10.5 ms| 9.5 ms |
//!
//! Absolute numbers drift by 20–30 % with machine load, so the ratios and the
//! `floor/*` comparisons are the part worth quoting, not the microseconds.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;
use std::time::{Duration, Instant};
use stratum_parse::{resegment, segment, SourceEdit};
use stratum_proto::Span;

/// Realistic Stata: comments, a loop, a continued command, macros.
fn doc(bytes: usize) -> String {
    const UNIT: &str = "\
* block {N}: describe and model
use panel{N}.dta, clear
gen ln_y{N} = log(y{N})
label variable ln_y{N} \"log outcome {N}\"
foreach v of varlist x1 x2 x3 {
    summarize `v', detail
    replace `v' = . if `v' < 0
}
regress ln_y{N} x1 x2 x3 ///
    if year > 2000, robust
predict yhat{N}, xb
";
    let mut out = String::with_capacity(bytes + UNIT.len());
    let mut n = 0usize;
    while out.len() < bytes {
        out.push_str(&UNIT.replace("{N}", &n.to_string()));
        n += 1;
    }
    out
}

fn cold(c: &mut Criterion) {
    let mut g = c.benchmark_group("cold");
    // 256 KiB is the largest file in StataCorp's own 3,807-file ado library
    // (median 2.0, p99 56, p99.9 127 KiB); 1024 and 2048 are synthetic
    // stress sizes that no shipped Stata program reaches.
    for kb in [64usize, 256, 1024, 2048] {
        let src = doc(kb * 1024);
        g.throughput(Throughput::Bytes(src.len() as u64));
        g.bench_with_input(BenchmarkId::from_parameter(kb), &src, |b, src| {
            b.iter(|| black_box(segment(black_box(src))).regions.len());
        });
    }
    g.finish();
}

/// Insert three statements at a line boundary, at `frac` of the way through.
fn edit_at(src: &str, frac: f64) -> (String, SourceEdit) {
    let target = ((src.len() as f64) * frac) as usize;
    let at = src[target.min(src.len() - 1)..]
        .find('\n')
        .map_or(src.len(), |o| o + target + 1);
    let ins = "di 1\ndi 2\ndi 3\n";
    let mut out = String::with_capacity(src.len() + ins.len());
    out.push_str(&src[..at]);
    out.push_str(ins);
    out.push_str(&src[at..]);
    (
        out,
        SourceEdit {
            range: Span {
                start: at as u32,
                end: at as u32,
            },
            new_len: ins.len() as u32,
        },
    )
}

/// One character typed inside an existing command, at `frac` of the way through.
///
/// This is what a keystroke actually is, and it is a different shape of work
/// from [`edit_at`]: the region count and the logical-line count are unchanged,
/// so nothing is inserted into either vector and the untouched 95 % of the
/// document is rebased where it lies rather than moved. Kept next to the A25
/// gate because the difference between the two numbers IS the cost of the move.
fn type_at(src: &str, frac: f64) -> (String, SourceEdit) {
    let target = ((src.len() as f64) * frac) as usize;
    // Inside a command, not at a line start: `use panel0.dta, clear` -> `...x`
    let at = src[target.min(src.len() - 1)..]
        .find('\n')
        .map_or(src.len() - 1, |o| o + target)
        - 1;
    let mut out = String::with_capacity(src.len() + 1);
    out.push_str(&src[..at]);
    out.push('x');
    out.push_str(&src[at..]);
    (
        out,
        SourceEdit {
            range: Span {
                start: at as u32,
                end: at as u32,
            },
            new_len: 1,
        },
    )
}

/// `resegment` CONSUMES its previous segmentation — that is what lets a
/// keystroke reuse the allocation instead of copying a 2 MB document (see its
/// doc comment). That makes the naive criterion spelling wrong: handing each
/// iteration a fresh `prev.clone()` charges the measurement 15 MB of allocator
/// traffic per keystroke that a real editor never pays, and buries the number
/// A25 is about — measured, the clone-per-iteration harness reports 3.3 ms for
/// work that takes 1.0 ms.
///
/// So this measures what an editor does. ONE long-lived `Segmentation` is
/// carried across iterations; each iteration applies the edit (timed) and then
/// applies its inverse to put the document back (untimed, via `iter_custom`).
/// Nothing is allocated or freed between iterations and every timed call sees
/// exactly the state the one before it did.
fn incremental(c: &mut Criterion) {
    let src = doc(2 * 1024 * 1024);
    let mut g = c.benchmark_group("incremental");
    for (name, frac) in [
        ("five_percent", 0.05),
        ("middle", 0.5),
        ("append", 0.99),
        ("typing", -0.05),
    ] {
        let (new, edit) = if frac < 0.0 {
            type_at(&src, -frac)
        } else {
            edit_at(&src, frac)
        };
        let undo = SourceEdit {
            range: Span {
                start: edit.range.start,
                end: edit.range.start + edit.new_len,
            },
            new_len: 0,
        };
        g.bench_function(name, |b| {
            let mut seg = Some(segment(&src));
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let prev = seg.take().expect("segmentation carried forward");
                    let t = Instant::now();
                    let edited = black_box(resegment(prev, black_box(&new), black_box(edit)));
                    total += t.elapsed();
                    seg = Some(resegment(edited, &src, undo));
                }
                total
            });
        });
    }
    g.finish();
}

/// The keystroke path also re-hashes the edited region. Measured separately so a
/// regression in `blake3` cost is visible rather than folded into the total.
fn hashing(c: &mut Criterion) {
    let src = doc(64 * 1024);
    let seg = segment(&src);
    c.bench_function("code_hash/one_region", |b| {
        let r = &seg.regions[3];
        let run = r.logical_lines.start as usize..r.logical_lines.end as usize;
        let lines = &seg.lines[run.clone()];
        let der = &seg.derived[run];
        b.iter(|| {
            black_box(stratum_parse::code_hash(
                black_box(&src),
                black_box(lines),
                black_box(der),
            ))
        });
    });
}

/// Where the time in a cold pass actually goes. These are not gates; they exist
/// so that a regression can be attributed instead of guessed at, and so that the
/// numbers in this unit's report can be reproduced.
fn phases(c: &mut Criterion) {
    let src = doc(1024 * 1024);
    c.bench_function("phase/line_index", |b| {
        b.iter(|| black_box(stratum_parse::LineIndex::new(black_box(&src))).line_count());
    });
    c.bench_function("phase/blake3_64", |b| {
        let data = [7u8; 64];
        b.iter(|| black_box(blake3::hash(black_box(&data))));
    });
    let seg = segment(&src);
    c.bench_function("phase/clone_lines", |b| {
        b.iter(|| black_box(seg.lines.clone()).len());
    });
    c.bench_function("phase/clone_regions", |b| {
        b.iter(|| black_box(seg.regions.clone()).len());
    });
    c.bench_function("phase/read_lines_only", |b| {
        b.iter(|| {
            black_box(stratum_parse::scan::logical::read_all(black_box(&src)))
                .0
                .len()
        });
    });
    // Tokenizing without hashing: the difference between this and
    // `phase/hash_all_regions` is the encode-and-blake3 half.
    c.bench_function("phase/canonicalise_all_regions", |b| {
        b.iter(|| {
            let mut n = 0u64;
            for r in &seg.regions {
                let run = r.logical_lines.start as usize..r.logical_lines.end as usize;
                let ls = &seg.lines[run.clone()];
                stratum_parse::canon::for_each_canon_token(&src, ls, &seg.derived[run], |k, t| {
                    n += k as u64 + t.len() as u64;
                });
            }
            black_box(n)
        });
    });
    c.bench_function("phase/hash_all_regions", |b| {
        let mut buf = Vec::new();
        b.iter(|| {
            let mut n = 0u64;
            for r in &seg.regions {
                let run = r.logical_lines.start as usize..r.logical_lines.end as usize;
                let ls = &seg.lines[run.clone()];
                n += u64::from(
                    stratum_parse::canon::code_hash_into(&src, ls, &seg.derived[run], &mut buf).0
                        [0],
                );
            }
            black_box(n)
        });
    });
}

/// Lower bounds on the two gates. See the module header.
fn floors(c: &mut Criterion) {
    // ---- the cold gate: 1 MB in < 4 ms -------------------------------------
    let src = doc(1024 * 1024);
    let seg = segment(&src);
    // The contract-mandated canonical encoding of every region, pre-built, so
    // that what remains is exactly the `blake3::hash` calls CONTRACTS §1.2
    // rule 6 requires and nothing else.
    let mut bufs: Vec<Vec<u8>> = Vec::with_capacity(seg.regions.len());
    let mut tokens = 0u64;
    for r in &seg.regions {
        let run = r.logical_lines.start as usize..r.logical_lines.end as usize;
        let ls = &seg.lines[run.clone()];
        let mut buf = Vec::new();
        stratum_parse::canon::for_each_canon_token(&src, ls, &seg.derived[run], |k, t| {
            tokens += 1;
            let n = t.len() as u32;
            buf.extend_from_slice(&[
                k as u8,
                n as u8,
                (n >> 8) as u8,
                (n >> 16) as u8,
                (n >> 24) as u8,
            ]);
            buf.extend_from_slice(t);
        });
        bufs.push(buf);
    }
    let encoded: usize = bufs.iter().map(Vec::len).sum();
    // The expansion ratio is carried as integer hundredths and split by hand
    // rather than handed to a float precision spec. ARCHITECTURE §8.7 grants no
    // exemption for a bench, and this crate cannot reach the one formatter of
    // record: it is wasm-clean and does not depend on `stratum-core`. Hundredths
    // are what the old spec rendered, so the reported number is unchanged.
    let ratio_x100 = (encoded * 100 + src.len() / 2) / src.len();
    let (ratio_whole, ratio_cents) = (ratio_x100 / 100, ratio_x100 % 100);
    println!(
        "floor: 1 MB corpus = {} regions, {} logical lines, {tokens} canonical tokens, \
         {encoded} encoded bytes ({ratio_whole}.{ratio_cents:02}x source) in {} blake3 \
         instances",
        seg.regions.len(),
        seg.lines.len(),
        bufs.len(),
    );
    c.bench_function("floor/blake3_canonical_1mb", |b| {
        b.iter(|| {
            let mut n = 0u64;
            for buf in &bufs {
                n += u64::from(blake3::hash(black_box(buf)).as_bytes()[0]);
            }
            black_box(n)
        });
    });
    // The same bytes through ONE blake3 instance. Not a legal `CodeHash` — one
    // hash cannot identify 21 438 regions — but it separates blake3's rate on
    // this machine from what paying the per-instance setup 21 438 times costs,
    // which is the difference between "the gate needs a cheaper hash" and "the
    // gate needs fewer hashes".
    let one: Vec<u8> = bufs.concat();
    c.bench_function("floor/blake3_one_stream_1mb", |b| {
        b.iter(|| black_box(blake3::hash(black_box(&one))));
    });

    // ---- the incremental gate: a 3-block insert 5 % into 2 MB in < 250 µs ---
    // The `_at64` rows exist to show the cost is linear in bytes moved rather
    // than in elements, which is what makes shrinking `Region` the only lever.
    let big = doc(2 * 1024 * 1024);
    let bigseg = segment(&big);
    let regions = bigseg.regions.len();
    let lines = bigseg.lines.len();
    let region_bytes = std::mem::size_of::<stratum_parse::Region>();
    let line_bytes = std::mem::size_of::<stratum_parse::LogicalLine>();
    println!(
        "floor: 2 MB corpus = {regions} regions of {region_bytes} B and \
         {lines} logical lines of {line_bytes} B; a 5 % edit moves 95 % of each"
    );
    let mut g = c.benchmark_group("floor");
    for (name, elem, count) in [
        ("memmove_region_tail_2mb", region_bytes, regions * 95 / 100),
        ("memmove_line_tail_2mb", line_bytes, lines * 95 / 100),
        ("memmove_region_tail_2mb_at64", 64, regions * 95 / 100),
        ("memmove_line_tail_2mb_at64", 64, lines * 95 / 100),
    ] {
        let mut v: Vec<u8> = vec![0u8; elem * (count + 3)];
        g.bench_function(name, |b| {
            b.iter(|| {
                v.copy_within(0..elem * count, elem * 3);
                black_box(v[0])
            });
        });
    }
    g.finish();

    // What a BY-REFERENCE `resegment` would cost before doing any work at all:
    // 02 §5.5's signature forces the result into fresh allocations, so the
    // document's line and region vectors must be copied however little of them
    // was recomputed. This number is why `resegment` takes ownership.
    c.bench_function("floor/copying_resegment_2mb", |b| {
        b.iter(|| {
            let l = black_box(bigseg.lines.clone());
            let r = black_box(bigseg.regions.clone());
            let i = black_box(bigseg.line_index.clone().patch(
                &big,
                stratum_proto::Span { start: 0, end: 0 },
                0,
            ));
            l.len() + r.len() + i.line_count() as usize
        });
    });
}

criterion_group!(
    benches,
    cold,
    incremental,
    hashing,
    phases,
    floors,
    tail_variants
);
criterion_main!(benches);

/// A/B on how the reused suffix is moved: the 2 MB region tail, three slots
/// along, rebased, five ways.
///
/// This is not a gate. It is the evidence for `splice_rebase`'s shape and for
/// `REBASE_CHUNK`, kept runnable because the answer moved as `Region` shrank:
/// at 96 bytes the element-wise loop was 129 µs against 108 µs chunked, and at
/// 88 bytes with a `u32` rebase it is 116 against 114. `move_only` and
/// `rebase_only` bracket both: neither half can be skipped, and their sum is
/// what a two-pass implementation actually costs (`two_pass`).
fn tail_variants(c: &mut Criterion) {
    let big = doc(2 * 1024 * 1024);
    let seg = segment(&big);
    let base = seg.regions.clone();
    let cut = base.len() * 5 / 100;
    let k = 3usize;
    let d = 15u32;

    #[inline(always)]
    fn rebase(r: &mut stratum_parse::Region, d: u32, k: u32) {
        r.index = r.index.wrapping_add(k);
        r.span.start = r.span.start.wrapping_add(d);
        r.span.end = r.span.end.wrapping_add(d);
        r.outer_span.start = r.outer_span.start.wrapping_add(d);
        r.outer_span.end = r.outer_span.end.wrapping_add(d);
        r.lines.start = r.lines.start.wrapping_add(k);
        r.lines.end = r.lines.end.wrapping_add(k);
        r.code_lines.start = r.code_lines.start.wrapping_add(k);
        r.code_lines.end = r.code_lines.end.wrapping_add(k);
        r.logical_lines.start = r.logical_lines.start.wrapping_add(k);
        r.logical_lines.end = r.logical_lines.end.wrapping_add(k);
    }

    let mut g = c.benchmark_group("tail");
    let n = base.len();
    let mut v = base.clone();
    v.resize(n + k, base[0]);
    g.bench_function("fused_elementwise", |b| {
        b.iter(|| {
            let s = v.as_mut_slice();
            for i in (cut..n).rev() {
                let mut e = s[i];
                rebase(&mut e, d, k as u32);
                s[i + k] = e;
            }
            black_box(s[cut].index)
        });
    });
    for chunk in [16usize, 64, 256, 1024] {
        g.bench_function(BenchmarkId::new("chunked", chunk), |b| {
            b.iter(|| {
                let mut hi = n;
                while hi > cut {
                    let lo = hi.saturating_sub(chunk).max(cut);
                    v.copy_within(lo..hi, lo + k);
                    for e in &mut v[lo + k..hi + k] {
                        rebase(e, d, k as u32);
                    }
                    hi = lo;
                }
                black_box(v[cut].index)
            });
        });
    }
    g.bench_function("two_pass", |b| {
        b.iter(|| {
            v.copy_within(cut..n, cut + k);
            for e in &mut v[cut + k..n + k] {
                rebase(e, d, k as u32);
            }
            black_box(v[cut].index)
        });
    });
    g.bench_function("move_only", |b| {
        b.iter(|| {
            v.copy_within(cut..n, cut + k);
            black_box(v[cut].index)
        });
    });
    g.bench_function("rebase_only", |b| {
        b.iter(|| {
            for e in &mut v[cut..n] {
                rebase(e, d, k as u32);
            }
            black_box(v[cut].index)
        });
    });
    g.finish();
}
