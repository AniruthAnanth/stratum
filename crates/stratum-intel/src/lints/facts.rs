//! Static command facts: what a command does, in the four dimensions the lints
//! and the reproducibility checks ask about.
//!
//! # Why this exists beside `stratum_effects::EffectTable`
//!
//! `EffectTable` is the authority, and it is deliberately **row-free**: A1 put
//! the trait in `stratum-effects` and the rows in `stratum-runtime` and
//! `stratum-stats`, so that a command cannot be added without someone declaring
//! its effects. `stratum-intel` links neither of those crates — it builds for
//! wasm and runs inside the editor, where no runtime exists — so it cannot read
//! those rows.
//!
//! What this table is, therefore, is the **conservative fallback**: a curated
//! answer for the ~120 command names the checks name explicitly, biased in the
//! same direction the trait requires (a MAY-set; when in doubt, say yes).
//! [`crate::repro::Audit::with_effects`] takes a `&dyn EffectTable` and prefers
//! it wherever it can answer, so a caller that *has* the rows gets the sharper
//! answer and a caller that does not still gets a sound one.
//!
//! Names are canonical command names as `stratum_parse::table()` spells them.

use rustc_hash::FxHashSet;

/// Commands that change the data in memory. Design 07 §6.3's `L004` (our
/// `L009`) asks "did the dataset change between the estimation and the
/// `predict`", and design 03 §10's `R016` asks "did a `capture` swallow a
/// command that writes data".
pub const MODIFIES_DATA: &[&str] = &[
    "append",
    "bysort",
    "collapse",
    "compress",
    "contract",
    "decode",
    "destring",
    "drop",
    "duplicates",
    "egen",
    "encode",
    "expand",
    "fillin",
    "format",
    "generate",
    "gsort",
    "import",
    "insobs",
    "joinby",
    "keep",
    "label",
    "merge",
    "mvencode",
    "order",
    "recast",
    "recode",
    "rename",
    "replace",
    "reshape",
    "sample",
    "separate",
    "set",
    "snapshot",
    "sort",
    "split",
    "stack",
    "sysuse",
    "tostring",
    "use",
    "webuse",
    "xpose",
];

/// Commands that write to the filesystem.
pub const WRITES_FILES: &[&str] = &[
    "copy",
    "erase",
    "export",
    "file",
    "graph",
    "log",
    "mkdir",
    "outfile",
    "outsheet",
    "putdocx",
    "putexcel",
    "putpdf",
    "rmdir",
    "save",
    "saveold",
    "translate",
];

/// Commands that read a file named in the statement.
pub const READS_FILES: &[&str] = &[
    "append", "do", "import", "include", "infile", "infix", "merge", "run", "sysuse", "use",
    "webuse",
];

/// Commands that consume the random-number stream. Design 03 §10's `R002`.
pub const RNG_COMMANDS: &[&str] = &[
    "bayes",
    "bayesmh",
    "bootstrap",
    "bsample",
    "elasticnet",
    "jackknife",
    "lasso",
    "permute",
    "ritest",
    "sample",
    "simulate",
    "splitsample",
    "sqrtlasso",
];

/// Functions that consume the random-number stream. The `r*` family of
/// design 03 §10's `R002`, spelled out rather than matched on a `r` prefix —
/// `round`, `regexm`, `real` and `reverse` all start with `r`.
pub const RNG_FUNCTIONS: &[&str] = &[
    "rbeta",
    "rbinomial",
    "rcauchy",
    "rchi2",
    "rexponential",
    "rgamma",
    "rhypergeometric",
    "rigaussian",
    "rlaplace",
    "rlogistic",
    "rnbinomial",
    "rnormal",
    "rpoisson",
    "rt",
    "runiform",
    "runiformint",
    "rweibull",
];

/// Commands that block or no-op in a headless run. Design 03 §10's `R009`.
pub const INTERACTIVE_ONLY: &[&str] = &["browse", "db", "edit", "more", "pause", "sleep", "window"];

/// Commands that hand control to something we cannot verify. Design 03 §10's
/// `R012`; sets `Taint::EXTERNAL`, which also blocks a ✓ on "runs from clean
/// state".
pub const EXTERNAL: &[&str] = &[
    "!", "java", "net", "plugin", "python", "shell", "ssc", "winexec",
];

/// `c()` keys whose value depends on the machine, the clock or the user.
/// Design 03 §10's `R011`.
pub const ENVIRONMENT_C_KEYS: &[&str] = &[
    "current_date",
    "current_time",
    "filename",
    "hostname",
    "machine_type",
    "os",
    "pathsep",
    "processors",
    "processors_lim",
    "processors_mach",
    "pwd",
    "stata_version",
    "sysdir_oldplace",
    "tmpdir",
    "username",
];

/// Commands that require `tsset` (or `xtset`, which implies it).
pub const NEEDS_TSSET: &[&str] = &[
    "ac", "arch", "arima", "corrgram", "dfgls", "dfuller", "irf", "newey", "pac", "pergram",
    "pperron", "prais", "tsappend", "tsfill", "tsfilter", "tsline", "tsreport", "tssmooth", "var",
    "vec", "wntestq", "xcorr",
];

/// Commands that require `xtset`.
pub const NEEDS_XTSET: &[&str] = &[
    "xtabond",
    "xtdescribe",
    "xtgee",
    "xtivreg",
    "xtline",
    "xtlogit",
    "xtnbreg",
    "xtologit",
    "xtoprobit",
    "xtpoisson",
    "xtprobit",
    "xtreg",
    "xtset",
    "xtsum",
    "xttab",
    "xttobit",
    "xtunitroot",
];

/// Commands that require `svyset`.
pub const NEEDS_SVYSET: &[&str] = &["svy", "svydescribe", "svymarkout"];

/// Commands that establish the dataset from scratch. Design 03 §10's `R014`.
pub const ESTABLISHES_DATA: &[&str] = &[
    "clear", "import", "infile", "infix", "input", "sysuse", "use", "webuse",
];

/// Order-sensitive consumers. Design 03 §10's `R008` needs one of these in the
/// forward closure of a non-unique `sort` before it will fire.
pub const ORDER_SENSITIVE: &[&str] = &["drop", "duplicates", "export", "keep", "list", "outfile"];

/// Commands that store estimation results (`e(b)`).
pub const ESTIMATION: &[&str] = &[
    "anova",
    "arch",
    "areg",
    "arima",
    "biprobit",
    "clogit",
    "cnsreg",
    "glm",
    "gsem",
    "heckman",
    "intreg",
    "ivregress",
    "logistic",
    "logit",
    "mlogit",
    "mprobit",
    "nbreg",
    "nl",
    "ologit",
    "oprobit",
    "poisson",
    "probit",
    "qreg",
    "regress",
    "rreg",
    "sem",
    "stcox",
    "streg",
    "tobit",
    "truncreg",
    "xtreg",
];

/// Membership test over a sorted-at-authoring-time slice.
///
/// A linear scan, deliberately: these lists are tens of entries, the check runs
/// once per statement, and a `HashSet` built per call would cost more than it
/// saves. [`Set`] exists for the loops that ask thousands of times.
#[must_use]
pub fn in_list(list: &[&str], name: &str) -> bool {
    list.contains(&name)
}

/// A prebuilt membership set, for a check that asks per statement over a whole
/// file.
pub struct Set(FxHashSet<&'static str>);

impl Set {
    /// Build from one of the lists above.
    #[must_use]
    pub fn new(list: &[&'static str]) -> Self {
        Set(list.iter().copied().collect())
    }

    /// Membership.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.0.contains(name)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    /// Every list is sorted and duplicate-free. Not cosmetic: these are read by
    /// a human deciding whether a command belongs, and an unsorted list is one
    /// nobody can audit.
    #[test]
    fn every_list_is_sorted_and_unique() {
        let lists: [(&str, &[&str]); 14] = [
            ("MODIFIES_DATA", MODIFIES_DATA),
            ("WRITES_FILES", WRITES_FILES),
            ("READS_FILES", READS_FILES),
            ("RNG_COMMANDS", RNG_COMMANDS),
            ("RNG_FUNCTIONS", RNG_FUNCTIONS),
            ("INTERACTIVE_ONLY", INTERACTIVE_ONLY),
            ("EXTERNAL", EXTERNAL),
            ("ENVIRONMENT_C_KEYS", ENVIRONMENT_C_KEYS),
            ("NEEDS_TSSET", NEEDS_TSSET),
            ("NEEDS_XTSET", NEEDS_XTSET),
            ("NEEDS_SVYSET", NEEDS_SVYSET),
            ("ESTABLISHES_DATA", ESTABLISHES_DATA),
            ("ORDER_SENSITIVE", ORDER_SENSITIVE),
            ("ESTIMATION", ESTIMATION),
        ];
        for (name, list) in lists {
            for w in list.windows(2) {
                assert!(w[0] < w[1], "{name} is not sorted at {:?}", w);
            }
        }
    }

    #[test]
    fn the_rng_function_list_does_not_swallow_every_r_name() {
        for innocent in ["round", "regexm", "real", "reverse", "runningsum"] {
            assert!(!in_list(RNG_FUNCTIONS, innocent), "{innocent}");
        }
        assert!(in_list(RNG_FUNCTIONS, "runiform"));
    }
}
