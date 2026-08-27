//! `05` §17.5 / ADR-017: the cross-platform determinism gate.
//!
//! A SHA-256 over the `%21x` hex of **every** `f64` this crate produces for the
//! whole golden matrix — `e(b)`, `e(V)` and every `r()` set — asserted against
//! a committed hash. `%21x` round-trips a double bit for bit (F16), so the
//! digest is over the bits, not over a rendering of them.
//!
//! **A mismatch is a release blocker.** It means the same source produced
//! different doubles on a different machine, thread count, or compiler, which
//! is precisely what spec §38 Scenario E promises cannot happen.
//!
//! ## Why the counter, not a duration
//!
//! ADR-017 forbids asserting wall-clock time. The quantity asserted here is a
//! hash — a function of the values and of nothing else — plus two counters:
//! the number of doubles in the digest, and the number of rows the estimators
//! touched. Durations may be recorded; none is asserted.
//!
//! ## Thread counts
//!
//! `05` §3's reduction discipline claims the answer does not depend on the
//! thread count. Rayon's global pool is built once per process from
//! `RAYON_NUM_THREADS`, so proving that needs separate processes: the driver
//! test re-executes this binary with the variable set to 1, 2 and 8 and
//! requires all three children to agree with the committed hash.
//!
//! ## SHA-256 is hand-written here, deliberately
//!
//! The workspace has no SHA-256 dependency and `Cargo.lock` belongs to W00
//! (R0). Sixty lines of FIPS 180-4 with the standard vectors asserted beside
//! them costs less than an edit to a file this unit does not own, and matches
//! the repo's existing choice to hand-parse hex floats rather than take
//! `hexf-parse` (`05` §17.2).

mod common;

use common::cases;
use stratum_core::fmt::fmt_hex;

/// The committed digest. Regenerate ONLY with `STRATUM_BLESS=1`, and only when
/// a numeric change is intended and reviewed — this constant moving is the
/// loudest signal in the crate.
const GOLDEN_HEX_FILE: &str = "determinism.hex";

/// Every `f64` of the golden matrix, in a fixed order, as `%21x`.
///
/// The order is: cases in matrix order; within a case, the scalars in insertion
/// order then each matrix row-major — exactly `ResultSet::all_f64`. Insertion
/// order is itself asserted by `eresults.rs`, so this sequence is pinned from
/// both ends.
fn hex_stream() -> (String, usize) {
    let mut out = String::new();
    let mut n = 0usize;
    for c in cases() {
        out.push_str(c.name);
        out.push('\n');
        for v in c.results.all_f64() {
            out.push_str(&fmt_hex(v));
            out.push('\n');
            n += 1;
        }
    }
    (out, n)
}

/// The gate.
#[test]
fn digest_matches_the_committed_hash() {
    let (stream, doubles) = hex_stream();
    let digest = hex(&sha256(stream.as_bytes()));

    if common::blessing() {
        common::bless(GOLDEN_HEX_FILE, &format!("{digest}\n{doubles}\n{stream}"));
        return;
    }

    let committed = common::golden(GOLDEN_HEX_FILE);
    let mut lines = committed.lines();
    let want = lines.next().expect("golden hash line");
    let want_n: usize = lines
        .next()
        .expect("golden count line")
        .parse()
        .expect("count");

    // The count is asserted separately so a matrix that silently lost a case
    // reports "23 doubles, expected 271" rather than an unreadable hash diff.
    assert_eq!(
        doubles, want_n,
        "the golden matrix produced {doubles} doubles, the committed stream has {want_n}"
    );
    let want_stream: String = committed.split_inclusive('\n').skip(2).collect::<String>();
    if stream != want_stream {
        // Name the first divergent value; a 12 kB hex diff names nothing.
        for (i, (g, w)) in stream.lines().zip(want_stream.lines()).enumerate() {
            assert_eq!(g, w, "determinism: stream line {} moved", i + 1);
        }
        panic!("determinism: the stream changed length");
    }
    assert_eq!(
        digest, want,
        "determinism: SHA-256 over the %21x stream moved"
    );
}

/// The same digest under `RAYON_NUM_THREADS` ∈ {1, 2, 8}.
///
/// Rayon's pool is process-global, so this spawns three children rather than
/// pretending an in-process loop proves anything.
#[test]
fn digest_is_independent_of_thread_count() {
    if std::env::var_os("STRATUM_DETERMINISM_CHILD").is_some() {
        // A child: it already ran the gate above as its own test.
        return;
    }
    let exe = std::env::current_exe().expect("current_exe");
    for threads in ["1", "2", "8"] {
        let out = std::process::Command::new(&exe)
            .args([
                "--exact",
                "digest_matches_the_committed_hash",
                "--nocapture",
            ])
            .env("RAYON_NUM_THREADS", threads)
            .env("STRATUM_DETERMINISM_CHILD", "1")
            .env_remove("STRATUM_BLESS")
            .output()
            .expect("re-exec the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "RAYON_NUM_THREADS={threads} changed the digest:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // `--exact` on a name that no longer exists runs nothing and exits 0,
        // which would make this whole gate vacuous after a rename. Require the
        // child to say it ran one test and that one passed.
        assert!(
            stdout.contains("1 passed"),
            "RAYON_NUM_THREADS={threads}: the child ran no test:\n{stdout}"
        );
    }
}

/// F16: `%21x` round-trips a Stata double exactly, which is the property that
/// makes the digest meaningful. `e(rmse)` from `regress price mpg weight
/// foreign` is the value the design measured.
#[test]
fn percent_21x_round_trips() {
    let x = 2_130.769_528_589_715_f64;
    let h = fmt_hex(x);
    assert_eq!(h, "+1.0a589ffa6bc53X+00b", "the %21x rendering of e(rmse)");
    // The hex is the bits: rebuild the double from the printed digits.
    assert_eq!(f64::from_bits(0x40A0_A589_FFA6_BC53), x);
    // The name is the width: `%21x` is 21 columns for every finite double —
    // sign, leading digit, point, 13 mantissa nibbles, `X`, exponent sign and
    // three exponent nibbles. A variable-width rendering would make the digest
    // depend on the values' magnitudes as well as their bits.
    for v in [
        0.0_f64,
        -1.0,
        1e300,
        f64::MIN_POSITIVE,
        stratum_core::SYSMISS,
    ] {
        assert_eq!(fmt_hex(v).len(), 21, "%21x is fixed width for {v:e}");
    }
}

/// `CHUNK_ROWS` is part of the wire format: changing it changes the fold
/// association order and therefore the last ulps of every reduction.
#[test]
fn chunk_size_is_frozen() {
    assert_eq!(stratum_core::reduce::CHUNK_ROWS, 65_536);
}

/// ADR-017's counter for the interaction path: the estimators scan each column
/// a bounded number of times, never once per interaction.
///
/// `regress price mpg weight foreign` gathers four columns of 74 rows once
/// each into the design buffer, and every later pass (the meat, the residuals,
/// the standardized coefficients) reads that buffer rather than the frame. So
/// the frame is touched 4 × 74 rows and no more, whatever the VCE.
#[test]
fn regress_touches_each_column_once() {
    // `stratum_data`'s counters are process-global and libtest runs the tests
    // in this binary concurrently, so a sibling building the whole golden
    // matrix would land inside the delta. Re-run alone in a child rather than
    // asserting a number that depends on the scheduler.
    if std::env::var_os("STRATUM_COUNTER_CHILD").is_none() {
        let exe = std::env::current_exe().expect("current_exe");
        let out = std::process::Command::new(exe)
            .args([
                "--exact",
                "regress_touches_each_column_once",
                "--test-threads",
                "1",
            ])
            .env("STRATUM_COUNTER_CHILD", "1")
            .output()
            .expect("re-exec the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // As above: a rename must not turn this into a test that runs nothing.
        assert!(
            stdout.contains("1 passed"),
            "the child ran no test:\n{stdout}"
        );
        return;
    }
    let a = common::auto();
    let before = stratum_data::counters().snapshot();
    let r = stratum_stats::regress(
        &stratum_stats::RegressSpec::new(
            "regress price mpg weight foreign",
            a.var("price"),
            vec![a.var("mpg"), a.var("weight"), a.var("foreign")],
        ),
        &a.all(),
    )
    .expect("regress");
    let d = stratum_data::counters().snapshot().since(before);
    assert_eq!(r.n, 74);
    assert_eq!(
        d.rows_touched,
        4 * 74,
        "regress gathered the design more than once"
    );
    // The frame is never written by an estimator.
    assert_eq!(d.chunks_cloned, 0, "an estimator copied a chunk");
    assert_eq!(d.journal_entries, 0, "an estimator journalled a write");
}

// ---------------------------------------------------------------------------
// SHA-256 (FIPS 180-4), hand-written. See the module docs for why.
// ---------------------------------------------------------------------------

const K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

fn sha256(msg: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let mut data = msg.to_vec();
    let bits = (msg.len() as u64) * 8;
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bits.to_be_bytes());

    let mut w = [0u32; 64];
    for block in data.chunks_exact(64) {
        for (i, slot) in w.iter_mut().take(16).enumerate() {
            *slot = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().expect("4 bytes"));
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(v);
        }
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The FIPS 180-4 vectors. Without these the gate could be a hash of nothing
/// in particular that happens to be stable.
#[test]
fn sha256_matches_the_published_vectors() {
    assert_eq!(
        hex(&sha256(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hex(&sha256(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hex(&sha256(
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
        )),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    // Crosses the 64-byte block boundary and the length-encoding edge.
    assert_eq!(
        hex(&sha256(&vec![b'a'; 1_000_000])),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}
