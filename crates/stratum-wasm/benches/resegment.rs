//! W11b's performance record: A25's incremental resegment, the cold pass, and
//! §14's 2 ms completion contract.
//!
//! # ADR-017, and what this file is allowed to assert
//!
//! **The gates are counters. The durations are records.** IMPLEMENTATION_PLAN
//! W11b states A25 as "< 400 µs p95 … with ≤ 8 regions re-hashed" and §14 states
//! completion as "< 2 ms, criterion-benched in CI". A wall clock cannot carry a
//! gate on this hardware — the same unchanged tree benchmarked 33 % apart an
//! hour apart purely from machine load — so what fails CI is the counter
//! expressing the same property, asserted in `tests/parity.rs` where it runs
//! natively *and* under `wasm-bindgen-test`:
//!
//! | Plan's wording | Counter that is gated | Where |
//! |---|---|---|
//! | ≤ 8 regions re-hashed | `PassStats::rescanned <= 8` | `parity.rs` |
//! | < 400 µs p95 incremental | recorded here | `incremental/…` |
//! | cold 10 k-line pass 3–8 ms | recorded here | `cold/…` |
//! | `complete()` < 2 ms at the cap | `scan_budget <= scan_ceiling` | `src/env.rs` |
//! | `complete()` < 2 ms at the cap | recorded here | `complete/…` |
//!
//! Criterion still prints the durations, and a regression shows up in them long
//! before it shows up anywhere else. What it does not do is fail a build because
//! a laptop was compiling something else.
//!
//! # A25's measurement point
//!
//! **At 5 % into the file, not at EOF.** An edit at the end of a document has no
//! suffix to reuse, so it measures the cold path wearing the incremental path's
//! name and reports a number three orders of magnitude too good. Every
//! incremental measurement below edits at 5 %.
//!
//! ```sh
//! cargo bench -p stratum-wasm --bench resegment
//! ```

// Everything below is `cfg`-gated off `wasm32-unknown-unknown`, because a bench
// target is BUILT for whatever target `cargo test --target …` is given and
// criterion is not in this crate's wasm dev-dependency set (it wants threads and
// `std::time`, neither of which exists there). Nothing is lost: what this file
// produces is a record, and every gate it describes is a counter asserted in
// `tests/parity.rs`, which does run in node.
#[cfg(not(target_arch = "wasm32"))]
use std::hint::black_box;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use criterion::{Criterion, Throughput};
#[cfg(not(target_arch = "wasm32"))]
use stratum_proto::CompletionEnv;
#[cfg(not(target_arch = "wasm32"))]
use stratum_wasm::{ParseSegmenter, Segmentation, Segmenter};

/// Roughly 2 MB of Stata that looks like Stata.
///
/// Calibrated against the real thing rather than against a repeat of one line:
/// StataCorp's own 3 807-program `ado` library has a median size of 2.0 KiB and
/// nothing above 512 KiB, so a 2 MB document is a *project* — dozens of files'
/// worth of code in one buffer, which is the shape a researcher's master do-file
/// actually has. The mix below is loops, program definitions, `#delimit ;`
/// stretches, continuations and comments in roughly the proportions those files
/// carry, because a corpus of nothing but `summarize price` measures the
/// grouper's fast path and nothing else.
#[cfg(not(target_arch = "wasm32"))]
fn corpus(bytes: usize) -> String {
    const UNIT: &str = "\
// %% Block ${N}
* Prepare the ${N}th slice of the analysis.
use \"data/panel_${N}.dta\", clear
keep if year >= 1990 & !missing(income)
generate loginc_${N} = log(income)
label variable loginc_${N} \"log income, slice ${N}\"

foreach v of varlist loginc_${N} educ exper {
    quietly summarize `v', detail
    display \"`v': mean = \" %9.4f r(mean) \" sd = \" %9.4f r(sd)
    if r(N) < 100 {
        display as error \"slice ${N}: too few observations for `v'\"
    }
}

local controls educ ///
    exper ///
    tenure ///
    i.industry

regress loginc_${N} `controls', vce(cluster firmid)
estimates store m_${N}

#delimit ;
margins,
    dydx(educ exper)
    at(tenure = (0 5 10))
    post;
#delimit cr

program define report_${N}, rclass
    version 18
    syntax varlist(min=1) [if] [in] [, Detail]
    quietly summarize `varlist' `if' `in'
    return scalar n = r(N)
    /* a block comment that spans
       several physical lines inside a program */
    if \"`detail'\" != \"\" {
        summarize `varlist' `if' `in', detail
    }
end

report_${N} loginc_${N}
";
    let mut out = String::with_capacity(bytes + UNIT.len());
    let mut n = 0usize;
    while out.len() < bytes {
        // `replace` rather than `format!`: every unit gets distinct identifiers,
        // so the hash of every region differs and the corpus cannot accidentally
        // measure the hash-ordinal fast path for a document of clones.
        out.push_str(&UNIT.replace("${N}", &n.to_string()));
        n += 1;
    }
    out
}

/// A document of `lines` physical lines, for the cold-pass record.
#[cfg(not(target_arch = "wasm32"))]
fn corpus_lines(lines: usize) -> String {
    let mut out = corpus(lines * 40);
    let cut = out
        .match_indices('\n')
        .nth(lines.saturating_sub(1))
        .map_or(out.len(), |(i, _)| i + 1);
    out.truncate(cut);
    out
}

/// Byte offset of the start of the logical line 5 % of the way into `src`.
///
/// Snapped to a line start so the edit is a plausible keystroke rather than a
/// splice into the middle of a token, and so the measurement is reproducible.
#[cfg(not(target_arch = "wasm32"))]
fn five_percent(src: &str) -> usize {
    let target = src.len() / 20;
    src[target..]
        .find('\n')
        .map_or(target, |i| target + i + 1)
        .min(src.len())
}

/// The A11 cap, exactly: 2 048 variables and 512 of every other list.
///
/// §14's 2 ms contract is measured HERE and not on `auto.dta`. `auto` has 12
/// variables; a completer that is fast on twelve names and quadratic in the
/// count would pass that measurement and stall on a real panel.
#[cfg(not(target_arch = "wasm32"))]
fn capped_env() -> CompletionEnv {
    fn names(n: usize, tag: &str) -> Vec<String> {
        (0..n).map(|i| format!("{tag}{i:06}")).collect()
    }
    let mut env = CompletionEnv {
        generation: 1,
        varnames: names(32_767, "v"),
        var_total: 32_767,
        frames: names(4_096, "frame"),
        locals: names(4_096, "loc"),
        globals: names(4_096, "glob"),
        scalars: names(4_096, "sc"),
        matrices: names(4_096, "mat"),
        programs: names(4_096, "prog"),
        e_names: names(4_096, "e_"),
        r_names: names(4_096, "r_"),
        value_labels: names(4_096, "vl"),
        stored_estimates: names(4_096, "est"),
        ..CompletionEnv::default()
    };
    // The producer's obligation (A11). Measuring an UNCAPPED environment would
    // be measuring a bug, not the contract.
    env.enforce_bounds();
    env
}

/// A25: one ≤3-block edit at 5 % into a 2 MB document.
#[cfg(not(target_arch = "wasm32"))]
fn incremental(c: &mut Criterion) {
    let src = corpus(2 * 1024 * 1024);
    let at = five_percent(&src);
    let edited = format!("{}display \"probe\"\n{}", &src[..at], &src[at..]);

    let mut group = c.benchmark_group("incremental");
    group.throughput(Throughput::Bytes(src.len() as u64));
    // The measured operation is ONE resegment on a warm cache, into the SAME
    // output buffer the engine reuses. `Engine::resegment` calls
    // `Segmentation::clear`, which keeps the allocation; a fresh
    // `Segmentation::default()` per iteration would instead measure a 1.4 MB
    // vector growing from zero by doubling, which the real path never does — and
    // that measured 1.5 ms against 0.9 ms here, i.e. it would have been most of
    // the reported number.
    group.bench_function("resegment_2mb_at_5pct", |b| {
        b.iter_batched_ref(
            || {
                let mut seg = ParseSegmenter::default();
                let mut out = Segmentation::default();
                seg.resegment(&src, &mut out);
                (seg, out)
            },
            |(seg, out)| {
                out.clear();
                seg.resegment(&edited, out);
                black_box(out.len())
            },
            criterion::BatchSize::LargeInput,
        );
    });

    // The same edit on a document the size real Stata is written in. A25 names
    // 2 MB, and 2 MB is the right *worst case* — but StataCorp's own 3 807-file
    // `ado` library has a median of 2.0 KiB and nothing above 512 KiB, so this
    // is the size at which the keystroke path is actually lived in, and the
    // number a researcher feels. Recorded beside the worst case rather than
    // instead of it.
    let small = corpus(256 * 1024);
    let small_at = five_percent(&small);
    let small_edited = format!(
        "{}display \"probe\"\n{}",
        &small[..small_at],
        &small[small_at..]
    );
    group.throughput(Throughput::Bytes(small.len() as u64));
    group.bench_function("resegment_256k_at_5pct", |b| {
        b.iter_batched_ref(
            || {
                let mut seg = ParseSegmenter::default();
                let mut out = Segmentation::default();
                seg.resegment(&small, &mut out);
                (seg, out)
            },
            |(seg, out)| {
                out.clear();
                seg.resegment(&small_edited, out);
                black_box(out.len())
            },
            criterion::BatchSize::LargeInput,
        );
    });
    group.finish();

    // ATTRIBUTION. The number above is not the parser's number, and W11b's
    // report turns on the difference. This group is `stratum_parse::resegment`
    // with nothing of this crate around it — no cache, no rediscovered splice,
    // no projection — so the three costs the wasm harness adds can be read off
    // as differences rather than guessed at.
    let mut group = c.benchmark_group("attribution");
    group.throughput(Throughput::Bytes(src.len() as u64));
    let edit = stratum_parse::SourceEdit {
        range: stratum_proto::Span {
            start: at as u32,
            end: at as u32,
        },
        new_len: "display \"probe\"\n".len() as u32,
    };
    group.bench_function("parse_resegment_only_2mb", |b| {
        b.iter_batched(
            || stratum_parse::segment(&src),
            |prev| {
                let next = stratum_parse::resegment(prev, &edited, edit);
                black_box(next.regions.len())
            },
            criterion::BatchSize::LargeInput,
        );
    });
    group.bench_function("copy_the_document_2mb", |b| {
        let mut buf = String::with_capacity(src.len() + 64);
        b.iter(|| {
            buf.clear();
            buf.push_str(black_box(&edited));
            black_box(buf.len())
        });
    });
    group.finish();

    // The counter, printed beside the duration so the record and the gate are
    // read together. `tests/parity.rs` is what FAILS on it.
    let mut seg = ParseSegmenter::default();
    seg.resegment(&src, &mut Segmentation::default());
    let cold = seg.last_pass();
    seg.resegment(&edited, &mut Segmentation::default());
    let inc = seg.last_pass();
    println!(
        "\n[A25] 2 MB corpus, {} regions, edit at 5 % ({at} of {} bytes)\n\
         [A25]   rescanned      = {} (gate: <= 8)\n\
         [A25]   reused_prefix  = {}\n\
         [A25]   reused_suffix  = {}\n\
         [A25]   bytes_scanned  = {} ({:.2} % of the file)\n\
         [A25]   bytes_diffed   = {} (cost of rediscovering the splice)\n\
         [A25]   converged      = {}\n",
        cold.regions,
        src.len(),
        inc.rescanned,
        inc.reused_prefix,
        inc.reused_suffix,
        inc.bytes_scanned,
        100.0 * f64::from(inc.bytes_scanned) / src.len() as f64,
        inc.bytes_diffed,
        inc.converged,
    );
}

/// The cold pass over 10 000 lines. Plan's record: 3–8 ms.
#[cfg(not(target_arch = "wasm32"))]
fn cold(c: &mut Criterion) {
    let src = corpus_lines(10_000);
    let mut group = c.benchmark_group("cold");
    group.throughput(Throughput::Bytes(src.len() as u64));
    group.bench_function("segment_10k_lines", |b| {
        b.iter(|| {
            let mut seg = ParseSegmenter::default();
            let mut out = Segmentation::default();
            seg.resegment(black_box(&src), &mut out);
            black_box(out.len())
        });
    });
    group.finish();

    // What a keystroke costs when the parse itself is free: the projection onto
    // the flat rows is O(regions), because §14's `regions_view()` hands the
    // editor the WHOLE document's rows on every pass. It is the floor under the
    // incremental measurement above, and the reason that number is not the
    // parse's `bytes_scanned`.
    let mut group = c.benchmark_group("project");
    group.throughput(Throughput::Bytes(src.len() as u64));
    group.bench_function("flat_rows_10k_lines", |b| {
        let mut seg = ParseSegmenter::default();
        let mut out = Segmentation::default();
        seg.resegment(&src, &mut out);
        b.iter(|| {
            out.clear();
            seg.resegment(black_box(&src), &mut out);
            black_box(out.len())
        });
    });
    group.finish();
}

/// §14: `complete()` under 2 ms, measured at the A11 cap.
#[cfg(not(target_arch = "wasm32"))]
fn complete(c: &mut Criterion) {
    let env = capped_env();
    let seg = ParseSegmenter::default();

    let mut group = c.benchmark_group("complete");
    // Four cursor positions, because the cost is not uniform: an empty prefix in
    // expression position matches every name in the environment and is the worst
    // case the contract has to hold for. Measuring only `summ` would measure the
    // command table and nothing else.
    for (label, doc, pos) in [
        ("expr_empty_prefix", "summarize ", 10usize),
        ("expr_one_letter", "summarize v", 11),
        ("command", "summ", 4),
        ("local", "display `loc", 12),
    ] {
        group.bench_function(label, |b| {
            b.iter(|| {
                black_box(
                    seg.complete(black_box(doc), black_box(&env), pos)
                        .items
                        .len(),
                )
            });
        });
    }
    group.finish();

    let worst = seg.complete("summarize ", &env, 10);
    println!(
        "\n[§14] complete() at the A11 cap: {} candidates matched, {} returned, truncated = {}\n",
        worst.total, worst.offered, worst.truncated
    );
}

/// The seam cost A25's counter cannot see: `Segmenter::resegment` is handed the
/// whole document and not the splice, so the backend rediscovers the edit by
/// comparing bytes. Recorded on its own so the escalation in W11b's report has a
/// number rather than an adjective.
#[cfg(not(target_arch = "wasm32"))]
fn edit_discovery(c: &mut Criterion) {
    let src = corpus(2 * 1024 * 1024);
    let at = five_percent(&src);
    let edited = format!("{}display \"probe\"\n{}", &src[..at], &src[at..]);

    let mut group = c.benchmark_group("seam");
    group.throughput(Throughput::Bytes(src.len() as u64));
    group.bench_function("rediscover_the_splice_2mb", |b| {
        b.iter(|| {
            // The same chunked comparison `engine::derive_edit` runs, inlined
            // here because it is private: prefix, then suffix, 64 bytes at a
            // time. What is measured is the memcmp, which is what the seam
            // costs.
            let (o, n) = (black_box(src.as_bytes()), black_box(edited.as_bytes()));
            let max = o.len().min(n.len());
            let mut p = 0;
            while p + 64 <= max && o[p..p + 64] == n[p..p + 64] {
                p += 64;
            }
            let limit = max - p;
            let mut s = 0;
            while s + 64 <= limit
                && o[o.len() - s - 64..o.len() - s] == n[n.len() - s - 64..n.len() - s]
            {
                s += 64;
            }
            black_box(p + s)
        });
    });
    group.finish();
}

/// A wasm build of this target exists only so `cargo test --target
/// wasm32-unknown-unknown` links something. It measures nothing.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let mut c = Criterion::default()
        // A 2 MB corpus is built per benchmark group; the default 3 s warm-up
        // over it is minutes of wall clock for a number that is a record, not a
        // gate.
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(3))
        .configure_from_args();
    incremental(&mut c);
    cold(&mut c);
    complete(&mut c);
    edit_discovery(&mut c);
    c.final_summary();
}
