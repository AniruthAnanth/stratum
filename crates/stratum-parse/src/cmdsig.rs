//! `CmdId`, `CommandSig`, `OptionSpec`, `CommandTable` — design 02 §6.3.
//!
//! **AMENDED (A29).** These declarations moved from W04b (wave 2) into W04
//! (wave 1) so that `stratum-effects` and `stratum-intel` compile without the
//! full parser. W04b keeps the LOADER: `build.rs`, `data/commands.ron` and
//! `cmdtable.rs`, which will hand a generated `&'static [CommandSig]` to
//! [`CommandTable::new`].
//!
//! [`CommandTable::core`] is a small hand-written table so that wave 1 can
//! resolve a command word at all — the gutter's `canonical=` and `is_estimation`
//! come from it. It is marked [`CommandTable::is_provisional`] and it populates
//! only the fields segmentation consumes; see that method for why the marker
//! exists rather than a silently-empty table.

use std::cmp::Ordering;

use bitflags::bitflags;
use smallvec::SmallVec;

/// Index into the command table. Stable only within one build.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct CmdId(pub u16);

bitflags! {
    /// Which slots of the universal syntax (02 §6.1) a command accepts.
    #[derive(Copy, Clone, PartialEq, Eq, Debug)]
    pub struct SlotMask: u16 {
        /// `varlist` of existing variables.
        const VARLIST     = 1 << 0;
        /// `newvarlist` — names that must NOT already exist.
        const NEWVARLIST  = 1 << 1;
        /// The `= exp` slot.
        const ASSIGN      = 1 << 2;
        /// `if exp`.
        const IF          = 1 << 3;
        /// `in range`.
        const IN          = 1 << 4;
        /// `[weight]`.
        const WEIGHT      = 1 << 5;
        /// `using filename`.
        const USING       = 1 << 6;
        /// A command-specific positional tail the universal grammar cannot classify.
        const REST        = 1 << 7;
    }
}

bitflags! {
    /// Which weight kinds a command accepts.
    #[derive(Copy, Clone, PartialEq, Eq, Debug)]
    pub struct WeightMask: u8 {
        /// `fweight` — frequency weights.
        const FWEIGHT = 1 << 0;
        /// `pweight` — sampling weights.
        const PWEIGHT = 1 << 1;
        /// `aweight` — analytic weights.
        const AWEIGHT = 1 << 2;
        /// `iweight` — importance weights.
        const IWEIGHT = 1 << 3;
    }
}

bitflags! {
    /// Command properties the IDE and the executor branch on.
    #[derive(Copy, Clone, PartialEq, Eq, Debug)]
    pub struct CmdFlags: u8 {
        /// e-class. Drives spec §19 "Compare models" and the estimation gutter.
        const ESTIMATION  = 1 << 0;
        /// Legal under a `by` prefix.
        const BYABLE      = 1 << 1;
        /// Modifies the dataset in place.
        const DESTRUCTIVE = 1 << 2;
        /// Opens a block terminated by a bare `end` (02 §5.3).
        const BLOCK_END   = 1 << 3;
        /// May appear in the prefix chain before `:`.
        const PREFIX      = 1 << 4;
    }
}

/// Release tier — which commands this build promises to execute.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Tier {
    /// Parsed and executed in v1.
    V1,
    /// Parsed in v1, executed in v1.5.
    V1_5,
    /// Parsed to `Command::Unknown` in v1.
    V2,
}

/// Type of an option's argument, before the command signature re-parses it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum OptionArgKind {
    /// Flag option, no argument.
    None,
    /// `n(4)`.
    Int,
    /// `level(95)`.
    Real,
    /// `title("x")`.
    Str,
    /// `bin(1/10)`.
    Numlist,
    /// `by(rep78)`.
    Varlist,
    /// `exp(a + b)`.
    Exprs,
    /// `format(%9.2f)`.
    Fmt,
    /// Paren-balanced text, handed to a command-specific mini-parser.
    Raw,
}

/// One option a command accepts.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct OptionSpec {
    /// Full spelling.
    pub canonical: &'static str,
    /// Shortest legal abbreviation. `0` means no abbreviation.
    pub min_abbrev: u8,
    /// Argument shape.
    pub arg: OptionArgKind,
    /// `nodetail` is accepted as the negation of `detail`.
    pub negatable: bool,
}

/// One command's signature.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct CommandSig {
    /// Full spelling. Table rows are sorted by this.
    pub canonical: &'static str,
    /// Shortest legal abbreviation length. `0` = no abbreviation allowed
    /// (`replace`, `drop`, `discard`, every ado-implemented command — [U] 11.2.1).
    pub min_abbrev: u8,
    /// Slots of the universal syntax this command accepts.
    pub slots: SlotMask,
    /// Weight kinds accepted.
    pub weights: WeightMask,
    /// Options, sorted by `canonical`.
    pub options: &'static [OptionSpec],
    /// Properties.
    pub flags: CmdFlags,
    /// Release tier.
    pub tier: Tier,
    /// One-liner for the completion popup (spec §22).
    pub help: &'static str,
}

/// Result of resolving a command word, applying Stata's abbreviation rules.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CommandLookup {
    /// The word is a full command name.
    Exact(CmdId),
    /// A legal, unique abbreviation.
    Abbrev(CmdId),
    /// A legal abbreviation of more than one command — r(199) with a
    /// "did you mean" list.
    Ambiguous(SmallVec<[CmdId; 4]>),
    /// No command starts with this word.
    Unknown,
}

/// A set of command signatures with Stata's abbreviation rules on top.
#[derive(Copy, Clone, Debug)]
pub struct CommandTable {
    rows: &'static [CommandSig],
    provisional: bool,
}

impl CommandTable {
    /// Wrap a generated table. `rows` MUST be sorted by `canonical`: resolution
    /// is a `partition_point` over that order, and an unsorted table silently
    /// stops resolving abbreviations rather than failing loudly.
    pub const fn new(rows: &'static [CommandSig]) -> Self {
        Self {
            rows,
            provisional: false,
        }
    }

    /// The wave-1 hand-written table.
    ///
    /// It exists because segmentation must label a region with a canonical
    /// command name (`HeadInfo::canonical`, `RegionSummary::is_estimation`)
    /// before W04b's generated table lands, and a segmenter that labels nothing
    /// cannot be golden-tested at all.
    pub const fn core() -> Self {
        Self {
            rows: CORE_COMMANDS,
            provisional: true,
        }
    }

    /// True for [`CommandTable::core`].
    ///
    /// `canonical`, `min_abbrev`, `flags` and `tier` are populated and are what
    /// wave 1 consumes; `slots`, `weights` and `options` are EMPTY, because
    /// inventing them by hand is how two tables end up disagreeing about
    /// `summarize, detail`. A consumer that needs a slot mask must check this
    /// flag and refuse, rather than read an empty mask as "no slots".
    pub const fn is_provisional(&self) -> bool {
        self.provisional
    }

    /// All rows, in canonical order.
    pub const fn rows(&self) -> &'static [CommandSig] {
        self.rows
    }

    /// Look up by id. Panics on an id from a different table.
    pub fn get(&self, id: CmdId) -> &'static CommandSig {
        &self.rows[id.0 as usize]
    }

    /// Resolve a command word.
    ///
    /// Abbreviation is legal iff `word.len() >= sig.min_abbrev`,
    /// `sig.min_abbrev > 0`, `sig.canonical.starts_with(word)`, and the match is
    /// unique. Case-sensitive: Stata command names are lower case and
    /// `Summarize` is not a command.
    pub fn resolve(&self, word: &str) -> CommandLookup {
        let Some(&w0) = word.as_bytes().first() else {
            return CommandLookup::Unknown;
        };
        // `str::cmp` is a `memcmp` call, and a binary search over 82 rows makes
        // six or seven of them. The FIRST BYTE settles all but the last level or
        // two, and settling it inline keeps `memcmp` off those levels entirely —
        // measured 44 ns -> 16 ns per lookup, once per region of the document.
        let by_word = |r: &CommandSig| {
            let rb = r.canonical.as_bytes();
            match rb[0].cmp(&w0) {
                Ordering::Equal => rb.cmp(word.as_bytes()),
                other => other,
            }
        };
        if let Ok(i) = self.rows.binary_search_by(by_word) {
            return CommandLookup::Exact(CmdId(i as u16));
        }
        let lo = self.rows.partition_point(|r| by_word(r) == Ordering::Less);
        let mut hits: SmallVec<[CmdId; 4]> = SmallVec::new();
        for (off, r) in self.rows[lo..].iter().enumerate() {
            if !r.canonical.starts_with(word) {
                break;
            }
            if r.min_abbrev > 0 && word.len() >= r.min_abbrev as usize {
                hits.push(CmdId((lo + off) as u16));
            }
        }
        match hits.len() {
            0 => CommandLookup::Unknown,
            1 => CommandLookup::Abbrev(hits[0]),
            _ => CommandLookup::Ambiguous(hits),
        }
    }

    /// The canonical name a word resolves to, or `None` when it is unknown or
    /// ambiguous. An ambiguous word deliberately yields `None`: labelling the
    /// gutter with one of two candidate commands is worse than labelling it with
    /// nothing.
    pub fn canonical(&self, word: &str) -> Option<&'static CommandSig> {
        self.canonical_id(word).map(|id| self.get(id))
    }

    /// [`CommandTable::canonical`] as the row's id. Segmentation stores the id
    /// rather than the reference — see `HeadInfo`.
    pub fn canonical_id(&self, word: &str) -> Option<CmdId> {
        match self.resolve(word) {
            CommandLookup::Exact(id) | CommandLookup::Abbrev(id) => Some(id),
            _ => None,
        }
    }
}

const NO_OPTS: &[OptionSpec] = &[];

/// Shorthand for a provisional row: slots/weights/options deliberately empty.
const fn row(
    canonical: &'static str,
    min_abbrev: u8,
    flags: CmdFlags,
    tier: Tier,
    help: &'static str,
) -> CommandSig {
    CommandSig {
        canonical,
        min_abbrev,
        slots: SlotMask::empty(),
        weights: WeightMask::empty(),
        options: NO_OPTS,
        flags,
        tier,
        help,
    }
}

const EST: CmdFlags = CmdFlags::ESTIMATION;
const NONE: CmdFlags = CmdFlags::empty();
const PFX: CmdFlags = CmdFlags::PREFIX;
const BLK: CmdFlags = CmdFlags::BLOCK_END;

/// The wave-1 command table. SORTED BY `canonical` — `CommandTable::resolve`
/// binary-searches it, and `cmdsig_rows_are_sorted` in `tests/canon.rs` is the
/// check that keeps it that way.
///
/// `min_abbrev` is `0` — no abbreviation — for every command whose documented
/// minimum this unit could not verify. That is the safe direction: an
/// unabbreviable command spelled in full still resolves, whereas a wrong minimum
/// makes a real abbreviation resolve to the wrong command.
pub static CORE_COMMANDS: &[CommandSig] = &[
    row(
        "anova",
        0,
        EST,
        Tier::V2,
        "analysis of variance and covariance",
    ),
    row(
        "areg",
        0,
        EST,
        Tier::V2,
        "linear regression absorbing one factor",
    ),
    row("assert", 0, NONE, Tier::V1, "verify truth of claim"),
    row("browse", 3, NONE, Tier::V1, "browse the data"),
    row("by", 2, PFX, Tier::V1, "repeat command on subsets"),
    row(
        "bysort",
        3,
        PFX,
        Tier::V1,
        "sort then repeat command on subsets",
    ),
    row("capture", 3, PFX, Tier::V1, "capture return code"),
    row("cd", 0, NONE, Tier::V1, "change directory"),
    row("clear", 0, NONE, Tier::V1, "clear memory"),
    row("codebook", 0, NONE, Tier::V1, "describe data contents"),
    row("confirm", 0, NONE, Tier::V1, "argument verification"),
    row("correlate", 4, NONE, Tier::V1, "correlations of variables"),
    row(
        "count",
        0,
        NONE,
        Tier::V1,
        "count observations satisfying conditions",
    ),
    row("creturn", 0, NONE, Tier::V1, "return c-class values"),
    row(
        "describe",
        1,
        NONE,
        Tier::V1,
        "describe data in memory or in a file",
    ),
    row(
        "destring",
        0,
        NONE,
        Tier::V1_5,
        "convert string variables to numeric",
    ),
    row(
        "display",
        2,
        NONE,
        Tier::V1,
        "substitute for a hand calculator",
    ),
    row("do", 0, NONE, Tier::V1, "execute commands from a file"),
    row(
        "drop",
        0,
        CmdFlags::DESTRUCTIVE,
        Tier::V1,
        "drop variables or observations",
    ),
    row("egen", 0, NONE, Tier::V1, "extensions to generate"),
    row(
        "encode",
        0,
        NONE,
        Tier::V1_5,
        "encode string into numeric and vice versa",
    ),
    row("erase", 0, NONE, Tier::V1, "erase a disk file"),
    row("ereturn", 0, NONE, Tier::V1, "post the estimation results"),
    row(
        "estimates",
        3,
        NONE,
        Tier::V1,
        "save and manipulate estimation results",
    ),
    row("exit", 0, NONE, Tier::V1, "exit Stata"),
    row("foreach", 0, NONE, Tier::V1, "loop over items"),
    row("format", 0, NONE, Tier::V1, "set variables' output format"),
    row(
        "forvalues",
        4,
        NONE,
        Tier::V1,
        "loop over consecutive values",
    ),
    row(
        "generate",
        1,
        NONE,
        Tier::V1,
        "create or change contents of variable",
    ),
    row("global", 2, NONE, Tier::V1, "define a global macro"),
    row("graph", 2, NONE, Tier::V1_5, "graphics"),
    row("help", 0, NONE, Tier::V1, "display online help"),
    row(
        "histogram",
        4,
        NONE,
        Tier::V1_5,
        "histograms for continuous and categorical variables",
    ),
    row(
        "import",
        0,
        NONE,
        Tier::V1_5,
        "overview of importing data into Stata",
    ),
    row("input", 0, BLK, Tier::V1, "enter data from keyboard"),
    row(
        "inspect",
        0,
        NONE,
        Tier::V1,
        "display simple summary of data's attributes",
    ),
    row(
        "ivregress",
        0,
        EST,
        Tier::V2,
        "single-equation instrumental-variables regression",
    ),
    row("java", 0, BLK, Tier::V2, "Java plugins"),
    row(
        "keep",
        0,
        CmdFlags::DESTRUCTIVE,
        Tier::V1,
        "keep variables or observations",
    ),
    row("label", 3, NONE, Tier::V1, "manipulate labels"),
    row("list", 1, NONE, Tier::V1, "list values of variables"),
    row("local", 3, NONE, Tier::V1, "define a local macro"),
    row("log", 0, NONE, Tier::V1, "close, save, and open log files"),
    row(
        "logit",
        0,
        EST,
        Tier::V2,
        "logistic regression, reporting coefficients",
    ),
    row(
        "macro",
        3,
        NONE,
        Tier::V1,
        "macro definition and manipulation",
    ),
    row("mata", 0, BLK, Tier::V2, "Mata"),
    row(
        "matrix",
        3,
        NONE,
        Tier::V1_5,
        "introduction to matrix commands",
    ),
    row(
        "merge",
        0,
        CmdFlags::DESTRUCTIVE,
        Tier::V1_5,
        "merge datasets",
    ),
    row(
        "mlogit",
        0,
        EST,
        Tier::V2,
        "multinomial logistic regression",
    ),
    row("nbreg", 0, EST, Tier::V2, "negative binomial regression"),
    row("noisily", 3, PFX, Tier::V1, "run command showing output"),
    row("ologit", 0, EST, Tier::V2, "ordered logistic regression"),
    row("oprobit", 0, EST, Tier::V2, "ordered probit regression"),
    row("order", 0, NONE, Tier::V1_5, "reorder variables in dataset"),
    row("poisson", 0, EST, Tier::V2, "Poisson regression"),
    row(
        "predict",
        0,
        NONE,
        Tier::V1,
        "obtain predictions, residuals, etc.",
    ),
    row("preserve", 0, NONE, Tier::V1, "preserve and restore data"),
    row("probit", 0, EST, Tier::V2, "probit regression"),
    row(
        "program",
        2,
        BLK,
        Tier::V1,
        "define and manipulate programs",
    ),
    row(
        "pwcorr",
        0,
        NONE,
        Tier::V1,
        "pairwise correlations of variables",
    ),
    row(
        "pwd",
        0,
        NONE,
        Tier::V1,
        "display current working directory",
    ),
    row("python", 0, BLK, Tier::V2, "call Python from Stata"),
    row(
        "quietly",
        3,
        PFX,
        Tier::V1,
        "quietly and noisily perform Stata command",
    ),
    row(
        "recode",
        0,
        NONE,
        Tier::V1_5,
        "recode categorical variables",
    ),
    row("regress", 3, EST, Tier::V1, "linear regression"),
    row("rename", 3, NONE, Tier::V1, "rename variable"),
    row(
        "replace",
        0,
        CmdFlags::DESTRUCTIVE,
        Tier::V1,
        "change contents of variable",
    ),
    row("restore", 0, NONE, Tier::V1, "preserve and restore data"),
    row("return", 0, NONE, Tier::V1, "return stored results"),
    row("save", 0, NONE, Tier::V1, "save Stata dataset"),
    row("scalar", 0, NONE, Tier::V1, "scalar variables"),
    row("set", 0, NONE, Tier::V1, "overview of system parameters"),
    row("sort", 0, NONE, Tier::V1, "sort data"),
    row("summarize", 2, NONE, Tier::V1, "summary statistics"),
    row("sysuse", 0, NONE, Tier::V1, "use shipped dataset"),
    row(
        "tabstat",
        0,
        NONE,
        Tier::V1,
        "compact table of summary statistics",
    ),
    row(
        "tabulate",
        3,
        NONE,
        Tier::V1,
        "one- and two-way tables of frequencies",
    ),
    row("tempfile", 0, NONE, Tier::V1, "temporary file names"),
    row(
        "tempname",
        0,
        NONE,
        Tier::V1,
        "temporary scalar and matrix names",
    ),
    row("tempvar", 0, NONE, Tier::V1, "temporary variable names"),
    row("tobit", 0, EST, Tier::V2, "tobit regression"),
    row(
        "ttest",
        0,
        NONE,
        Tier::V1,
        "t tests (mean-comparison tests)",
    ),
    row("use", 1, NONE, Tier::V1, "load Stata dataset"),
    row(
        "webuse",
        0,
        NONE,
        Tier::V1_5,
        "use dataset from Stata website",
    ),
    row("while", 0, NONE, Tier::V1, "looping"),
    row(
        "xtreg",
        0,
        EST,
        Tier::V2,
        "fixed-, between-, and random-effects linear models",
    ),
];
