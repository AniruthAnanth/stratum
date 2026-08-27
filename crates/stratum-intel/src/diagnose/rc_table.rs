//! The compiled return-code cards — design 07 §6.1.
//!
//! # This file is GENERATED-BY-HAND FROM `data/rc_table.toml`, and a test says so
//!
//! `data/rc_table.toml` is the authoring source: it carries the provenance
//! header explaining that every word is ours and none of it is StataCorp's, and
//! it is the file a human edits. This is the build the shipping binary reads.
//!
//! There is no build script and no run-time parse, for two reasons that are the
//! same reason. The quick-fix path runs synchronously on every failed execution
//! — design 07 §6.1 puts it "well under a millisecond" — and a TOML parse per
//! error would be most of that budget. And this crate reaches no filesystem at
//! all (it builds for `wasm32-unknown-unknown` and runs in the editor), so it
//! could not read the file at run time even if the parse were free.
//!
//! `tests/repro.rs` parses `data/rc_table.toml` and asserts card-for-card,
//! field-for-field equality with [`CARDS`]. The two cannot drift without a red
//! test.

/// One authored explanation card.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RcCard {
    /// The Stata return code.
    pub rc: u32,
    /// Four to six words: what went wrong.
    pub title: &'static str,
    /// One or two sentences: what the runtime was doing and why it stopped.
    pub explain: &'static str,
    /// Why this happens, most common first.
    pub causes: &'static [&'static str],
    /// What to do, in order of what usually works.
    pub fixes: &'static [&'static str],
    /// Verified against `tests/golden/stata18/errors.log`.
    pub golden: bool,
}

/// Every card, sorted by return code so [`card`] can binary-search.
pub static CARDS: &[RcCard] = &[
    RcCard {
        rc: 1,
        title: "Interrupted",
        explain: "The run stopped because you asked it to. Nothing after the interrupted command executed, and whatever the command had already changed in memory stays changed.",
        causes: &[
            "You pressed the stop button or Ctrl-C.",
            "A parent do-file was cancelled while this one was running.",
        ],
        fixes: &[
            "Re-run the block. If the dataset is now half-modified, re-run from the last command that loaded data.",
        ],
        golden: false,
    },
    RcCard {
        rc: 4,
        title: "No observations",
        explain: "The command needs at least one observation and the dataset — or the subset your `if`/`in` selected — is empty.",
        causes: &[
            "An `if` condition matched nothing.",
            "A previous `drop` or `keep` removed every row.",
            "The file loaded successfully but contains no data.",
        ],
        fixes: &[
            "`count` before the command to see how many rows survive the selection.",
            "Check the `if` condition for a missing-value trap: `x > 5` is true when `x` is missing, so `& !missing(x)` may be what you meant.",
        ],
        golden: false,
    },
    RcCard {
        rc: 7,
        title: "Wrong variable type",
        explain: "The command was given a variable of one type where it needs the other — a string where it needs a number, or the reverse.",
        causes: &[
            "`confirm numeric variable` on a string variable, or the reverse.",
            "A string variable read from a delimited file that looks numeric but is not.",
        ],
        fixes: &[
            "`describe` the variable to see its storage type.",
            "`destring` converts a numeric-looking string; `tostring` goes the other way.",
        ],
        golden: true,
    },
    RcCard {
        rc: 9,
        title: "Assertion is false",
        explain: "An `assert` you wrote did not hold. This is your own check firing, not a failure of the command — and the run stopped deliberately so that nothing downstream uses data that violates it.",
        causes: &[
            "The data changed since the assertion was written.",
            "A merge brought in rows the assertion did not anticipate.",
            "The assertion is stated over the wrong subset.",
        ],
        fixes: &[
            "`count if !(<your condition>)` shows how many rows fail.",
            "`list if !(<your condition>) in 1/20` shows which ones.",
        ],
        golden: true,
    },
    RcCard {
        rc: 100,
        title: "Varlist required",
        explain: "The command needs you to name at least one variable and none was given.",
        causes: &[
            "The variable names were left off.",
            "A macro that should have expanded to a varlist was empty.",
        ],
        fixes: &[
            "Name the variables the command should operate on.",
            "If a macro supplies the list, `display` it first — an empty macro produces exactly this error.",
        ],
        golden: true,
    },
    RcCard {
        rc: 101,
        title: "Varlist not allowed",
        explain: "The command takes no variables, but names were given after it.",
        causes: &[
            "The names belong in an option rather than in the command's varlist.",
            "A typo joined two commands onto one line.",
        ],
        fixes: &[
            "Check the command's syntax: many commands take their variables through an option such as `by()` or `over()`.",
        ],
        golden: false,
    },
    RcCard {
        rc: 102,
        title: "Too few variables",
        explain: "The command needs more variables than were supplied.",
        causes: &[
            "A two-variable command was given one.",
            "A macro expanded to a shorter list than expected.",
        ],
        fixes: &[
            "Count what the command needs and what it got.",
            "If a macro supplies the list, `display` it before the command.",
        ],
        golden: true,
    },
    RcCard {
        rc: 103,
        title: "Too many variables",
        explain: "More variables were supplied than the command accepts.",
        causes: &[
            "A wildcard such as `pr*` matched more variables than intended.",
            "An extra name was left on the line by an edit.",
        ],
        fixes: &[
            "`describe pr*` shows exactly what a wildcard matches before you use it.",
        ],
        golden: false,
    },
    RcCard {
        rc: 104,
        title: "Nothing to do",
        explain: "The command was well formed but had no work to perform.",
        causes: &[
            "Every selected observation was already in the requested state.",
        ],
        fixes: &[
            "This is usually harmless. If it is not, check that the selection is what you meant.",
        ],
        golden: false,
    },
    RcCard {
        rc: 107,
        title: "Not possible with a numeric variable",
        explain: "The command only accepts a string variable and was given a numeric one.",
        causes: &[
            "`encode` was applied to a variable that is already numeric.",
            "A variable was converted earlier in the file and the conversion is being applied twice.",
        ],
        fixes: &[
            "`describe` the variable: if it is already numeric, the conversion is redundant and the line can go.",
            "`decode` produces a string from a labelled numeric variable if that is what you want.",
        ],
        golden: true,
    },
    RcCard {
        rc: 108,
        title: "Not possible with a string variable",
        explain: "The command only accepts a numeric variable and was given a string one.",
        causes: &[
            "An arithmetic command was applied to a name that holds text.",
            "A delimited import left a numeric column stored as text.",
        ],
        fixes: &[
            "`destring <var>, replace` converts a numeric-looking string.",
            "If the column really is text, `encode` gives it a numeric code with value labels.",
        ],
        golden: false,
    },
    RcCard {
        rc: 109,
        title: "Type mismatch",
        explain: "An expression combined a string and a number in a way that has no meaning — for example adding text to a count.",
        causes: &[
            "A string variable used where a numeric one was intended.",
            "A quoted literal where a number belongs, or the reverse.",
            "A function given the wrong kind of argument.",
        ],
        fixes: &[
            "Check the storage type of every variable in the expression with `describe`.",
            "`real(\"...\")` converts text to a number; `string(...)` goes the other way.",
        ],
        golden: true,
    },
    RcCard {
        rc: 110,
        title: "Variable already defined",
        explain: "The command would create a variable that already exists, and it refuses to overwrite it silently.",
        causes: &[
            "The block has already been run once in this session.",
            "The name collides with a variable the dataset already had.",
        ],
        fixes: &[
            "`replace` changes an existing variable; `generate` only creates a new one.",
            "`capture drop <var>` before the `generate` makes the block re-runnable.",
        ],
        golden: true,
    },
    RcCard {
        rc: 111,
        title: "Variable not found",
        explain: "No variable of that name exists in the data currently in memory. Stata does not guess: a name that is not there is an error rather than a missing value.",
        causes: &[
            "A typo in the name.",
            "The variable is created by a block above that has not been run yet.",
            "A different dataset is loaded than the one the file expects.",
            "The variable was dropped earlier in the file.",
        ],
        fixes: &[
            "`describe` lists every variable currently in memory.",
            "Run the blocks above this one first if the variable is created there.",
            "Abbreviations must be unique: `pri` fails if both `price` and `printed` exist.",
        ],
        golden: true,
    },
    RcCard {
        rc: 119,
        title: "Weights not allowed",
        explain: "A weight was supplied to a command that does not accept one.",
        causes: &[
            "The weight belongs on a different command in the sequence.",
        ],
        fixes: &[
            "Check which of the commands in the block is meant to be weighted.",
        ],
        golden: false,
    },
    RcCard {
        rc: 120,
        title: "Weight type not allowed",
        explain: "The command accepts weights, but not the kind given — analytic, frequency, sampling and importance weights are not interchangeable.",
        causes: &[
            "`aweight` used where only `fweight` is accepted, or the reverse.",
        ],
        fixes: &[
            "The four weight types answer different questions; changing the type changes the estimate, so pick the one that matches your design rather than the one that runs.",
        ],
        golden: false,
    },
    RcCard {
        rc: 121,
        title: "Invalid numlist",
        explain: "A list of numbers could not be read. A numlist is written as bare values, as a `first/last` range, or as a `first(step)last` sequence, and something here matches none of the three.",
        causes: &[
            "A stray character inside the list.",
            "A range written backwards, such as `10/1`.",
        ],
        fixes: &[
            "Ranges go low to high: `1/10`. Steps are written `1(2)11`.",
        ],
        golden: false,
    },
    RcCard {
        rc: 122,
        title: "Invalid numlist — out of range",
        explain: "A list of numbers was well formed but contained a value the command cannot use.",
        causes: &[
            "A negative value where only positive ones make sense.",
            "A value larger than the number of observations.",
        ],
        fixes: &[
            "`count` first if the list is bounded by the data.",
        ],
        golden: false,
    },
    RcCard {
        rc: 130,
        title: "Expression too long or too complex",
        explain: "The expression exceeded the limit on how deeply it can nest or how long it can be.",
        causes: &[
            "A very long chain of `+` or `|` terms, often produced by a loop.",
            "Deeply nested function calls.",
        ],
        fixes: &[
            "Split the expression across two `generate` statements with a temporary variable.",
            "`inlist()` and `inrange()` replace long `|` chains and are faster.",
        ],
        golden: false,
    },
    RcCard {
        rc: 132,
        title: "Unbalanced parentheses or quotes",
        explain: "The expression has more opening than closing brackets, or an unterminated string.",
        causes: &[
            "A missing `)`.",
            "A missing closing `\"`.",
            "A macro whose value itself contains an unbalanced quote.",
        ],
        fixes: &[
            "If a macro is involved, `display` it: an unbalanced quote inside a macro value is the hardest form of this to see.",
            "Compound quotes `` `\"...\"' `` survive values that contain quotation marks.",
        ],
        golden: false,
    },
    RcCard {
        rc: 133,
        title: "Unknown function",
        explain: "The expression called a function this build does not have.",
        causes: &[
            "A typo in the function name.",
            "A function from a package that is not installed.",
            "A function from a newer Stata release.",
        ],
        fixes: &[
            "Function names never abbreviate — `ln` and `log` are different functions, and neither is `logn`.",
            "If the function comes from a package, install it before the file runs and record the dependency.",
        ],
        golden: true,
    },
    RcCard {
        rc: 134,
        title: "Too many values",
        explain: "The command hit its limit on the number of distinct values it can handle.",
        causes: &[
            "`tabulate` on a continuous variable.",
            "`encode` on a variable with more distinct strings than the limit allows.",
        ],
        fixes: &[
            "Group the values first, or use a command designed for continuous data such as `summarize`.",
        ],
        golden: false,
    },
    RcCard {
        rc: 190,
        title: "Required option missing",
        explain: "The command cannot run without an option that was not supplied.",
        causes: &[
            "An option that is optional in other commands is mandatory in this one.",
        ],
        fixes: &[
            "The error names the option. Supply it, or use a command that does not require it.",
        ],
        golden: false,
    },
    RcCard {
        rc: 197,
        title: "Invalid syntax for this Stata version",
        explain: "The syntax is valid in some Stata release, but not the one this file pinned with `version`.",
        causes: &[
            "A `version 12` line at the top of a file that uses newer syntax.",
        ],
        fixes: &[
            "Raise the `version` statement, or rewrite the line in the older syntax. Do not simply delete `version`: it is what makes the file reproduce.",
        ],
        golden: false,
    },
    RcCard {
        rc: 198,
        title: "Invalid syntax",
        explain: "The command was recognised but could not be parsed. This is the general syntax error, so the specific part of the line that is wrong is usually named in the message above.",
        causes: &[
            "A misspelled option.",
            "An option given to a command that does not take it.",
            "A comma missing before the options.",
            "An `in` range outside the data.",
        ],
        fixes: &[
            "Options go after a single comma, and there is only ever one comma.",
            "Check the option spelling: option names abbreviate, but only to a unique prefix.",
        ],
        golden: true,
    },
    RcCard {
        rc: 199,
        title: "Unrecognized command",
        explain: "The first word of the line is not a command this build knows, and no ado-file of that name was found on the search path.",
        causes: &[
            "A typo in the command name.",
            "A community-contributed command that is not installed.",
            "A user-written program defined later in the file than the line that calls it.",
            "A stray word left at the start of the line by an edit.",
        ],
        fixes: &[
            "If the command comes from a package, install it before the file runs and record the dependency so the file reproduces elsewhere.",
            "`program define` must come before the first call, not after.",
        ],
        golden: true,
    },
    RcCard {
        rc: 430,
        title: "Convergence not achieved",
        explain: "The estimator ran its iteration limit without meeting its convergence criterion. Any coefficients reported are from the last iteration and should not be interpreted.",
        causes: &[
            "The model is not identified by the data.",
            "Perfect or near-perfect prediction (separation) in a binary model.",
            "Wildly different variable scales.",
        ],
        fixes: &[
            "Check for separation: a predictor that perfectly predicts the outcome makes the maximum likelihood estimate infinite.",
            "Rescale predictors so they are of comparable magnitude.",
            "Simplify the model and add terms back one at a time.",
        ],
        golden: false,
    },
    RcCard {
        rc: 459,
        title: "Key variables do not uniquely identify observations",
        explain: "A command that needs one row per key found more than one. The operation stopped rather than silently picking a row.",
        causes: &[
            "A 1:1 merge on a key that repeats.",
            "`reshape wide` where the identifier is not unique within a group.",
        ],
        fixes: &[
            "`duplicates report <key>` shows how many rows share a key.",
            "`isid <key>` asserts uniqueness and fails loudly where you expect it to hold.",
            "If the duplicates are legitimate, an `m:1` or `1:m` merge is the honest form.",
        ],
        golden: false,
    },
    RcCard {
        rc: 498,
        title: "Estimation failed",
        explain: "The estimator could not produce a result for this specification and this sample.",
        causes: &[
            "Too few observations for the number of parameters.",
            "Collinear predictors.",
            "A subgroup with no variation in the outcome.",
        ],
        fixes: &[
            "`summarize` the estimation sample; the answer is usually visible there.",
            "Drop collinear terms — Stata usually names them above this error.",
        ],
        golden: false,
    },
    RcCard {
        rc: 601,
        title: "File not found",
        explain: "The file named does not exist at the path given, relative to the working directory the file is running in.",
        causes: &[
            "A path that is relative to a different working directory.",
            "A file that exists with different capitalisation — this works on macOS and Windows and fails on Linux.",
            "A typo in the name.",
            "A file that an earlier step was supposed to create and did not.",
        ],
        fixes: &[
            "`pwd` shows the working directory the path is resolved against.",
            "Prefer paths relative to the project root over absolute ones: an absolute path is a path that only works on your machine.",
            "Check capitalisation — this is the failure that only appears on a colleague's machine.",
        ],
        golden: true,
    },
    RcCard {
        rc: 602,
        title: "File already exists",
        explain: "The command would overwrite an existing file and refused to do so silently.",
        causes: &[
            "The block has already been run once.",
        ],
        fixes: &[
            "Add `, replace` if overwriting is what you want. Consider whether re-running should overwrite — a file that is an input elsewhere probably should not.",
        ],
        golden: false,
    },
    RcCard {
        rc: 603,
        title: "File could not be opened",
        explain: "The file exists and the path resolves, but the operating system refused to open it. This is a lock or a permission, not a missing file — a missing file is r(601).",
        causes: &[
            "The file is open in another program that holds an exclusive lock.",
            "Insufficient permissions.",
            "The file is on a network share that is not mounted.",
        ],
        fixes: &[
            "Close the file in any other application and retry.",
            "Check that the path is on a volume that is currently available.",
        ],
        golden: false,
    },
    RcCard {
        rc: 604,
        title: "Log file already open",
        explain: "A log is already running and a second one cannot be opened under the same name.",
        causes: &[
            "A previous run stopped before closing its log.",
            "Two `log using` statements with no `log close` between them.",
        ],
        fixes: &[
            "`log close _all` closes everything that is open.",
            "`log using <name>, replace` in a do-file makes the file re-runnable.",
        ],
        golden: false,
    },
    RcCard {
        rc: 608,
        title: "No log file open",
        explain: "`log close` was issued but no log was running.",
        causes: &[
            "A `log close` left over from an earlier version of the file.",
            "The `log using` above it failed.",
        ],
        fixes: &[
            "`capture log close` at the top of a do-file is the idiom that makes this harmless.",
        ],
        golden: false,
    },
    RcCard {
        rc: 610,
        title: "File is not a Stata dataset",
        explain: "The bytes at that path are not a `.dta` file this build can read.",
        causes: &[
            "A `.csv` or `.xlsx` given to `use` instead of `import`.",
            "A truncated or partially written file.",
            "A `.dta` written by a much newer Stata release.",
        ],
        fixes: &[
            "`import delimited` reads text data; `import excel` reads spreadsheets. `use` is only for `.dta`.",
            "If the file was produced by a newer release, ask for it to be saved with `saveold`.",
        ],
        golden: false,
    },
    RcCard {
        rc: 621,
        title: "Data have changed since last saved",
        explain: "The command would discard unsaved changes to the data in memory.",
        causes: &[
            "A `use` or `clear` after modifying the data.",
        ],
        fixes: &[
            "`use <file>, clear` says explicitly that discarding is intended.",
            "If the changes matter, `save` them first.",
        ],
        golden: false,
    },
    RcCard {
        rc: 631,
        title: "Host not found",
        explain: "A network operation could not resolve the host it was asked to reach.",
        causes: &[
            "No network connection.",
            "A proxy or firewall that blocks the request.",
            "A mistyped URL.",
        ],
        fixes: &[
            "A file that needs the network to run is a file that does not reproduce offline. Download the input once and commit it, or record the download step separately.",
        ],
        golden: false,
    },
    RcCard {
        rc: 672,
        title: "Not a valid variable name",
        explain: "The name given cannot be a Stata variable name.",
        causes: &[
            "The name starts with a digit.",
            "It contains a space, a hyphen or another character that is not a letter, a digit or an underscore.",
            "It is longer than the limit.",
            "It is a reserved word such as `_n` or `if`.",
        ],
        fixes: &[
            "Names start with a letter or an underscore and contain only letters, digits and underscores.",
            "A leading underscore is legal but is reserved by convention for Stata's own use.",
        ],
        golden: false,
    },
    RcCard {
        rc: 682,
        title: "Could not create the requested variable",
        explain: "The variable could not be added to the dataset.",
        causes: &[
            "The dataset already holds the maximum number of variables.",
            "Insufficient memory for another column at this row count.",
        ],
        fixes: &[
            "`drop` variables the analysis no longer needs before creating more.",
            "`compress` reduces storage types to the smallest that hold the data without loss.",
        ],
        golden: false,
    },
    RcCard {
        rc: 900,
        title: "No estimation results",
        explain: "The command needs the results of a previous estimation and none are stored.",
        causes: &[
            "The estimation above failed.",
            "The estimation was never run in this session — the file assumes something typed earlier.",
            "A command in between cleared the stored results.",
        ],
        fixes: &[
            "Run the estimation block first.",
            "A file that depends on results from outside itself does not reproduce; move the estimation into the file.",
        ],
        golden: false,
    },
    RcCard {
        rc: 903,
        title: "Matrix not found",
        explain: "No matrix of that name is stored. Matrix names are case-sensitive, and matrices are cleared by `clear all` and never persist between sessions, so a name that resolved earlier can genuinely be gone.",
        causes: &[
            "A typo in the matrix name.",
            "The matrix was created in a different session.",
            "`matrix drop` removed it earlier in the file.",
        ],
        fixes: &[
            "`matrix dir` lists every stored matrix.",
        ],
        golden: false,
    },
    RcCard {
        rc: 908,
        title: "Matrix too large",
        explain: "The matrix exceeds the largest this build can hold.",
        causes: &[
            "A matrix built from a variable with far more distinct values than expected.",
            "A model with more parameters than the matrix limit allows.",
        ],
        fixes: &[
            "This is almost always a sign that a variable being used as a factor is really continuous.",
        ],
        golden: false,
    },
    RcCard {
        rc: 909,
        title: "Matrices are not conformable",
        explain: "A matrix operation was given operands whose dimensions do not fit — the number of columns on the left does not match the number of rows on the right.",
        causes: &[
            "A transpose that was left out.",
            "A matrix whose size depends on the data changing shape between runs.",
        ],
        fixes: &[
            "`matrix list` shows dimensions.",
            "If the shape depends on the data, assert it: a matrix step that only works on one dataset is a reproducibility problem.",
        ],
        golden: false,
    },
    RcCard {
        rc: 920,
        title: "Macro nesting or length limit exceeded",
        explain: "A macro reference expanded into another macro reference too many times, or the result grew past the length limit. Stratum stops rather than looping.",
        causes: &[
            "A macro that refers to itself, directly or through another.",
            "A loop that appends to a macro on every iteration without bound.",
        ],
        fixes: &[
            "`display` the macro at the point it is built.",
            "Building a very long list in a macro is usually better done as a variable or a frame.",
        ],
        golden: false,
    },
    RcCard {
        rc: 1000,
        title: "System limit exceeded",
        explain: "The operation needs more of some fixed resource than this build provides.",
        causes: &[
            "More variables than the maximum.",
            "A width per observation larger than the limit.",
        ],
        fixes: &[
            "`compress` reduces storage types without losing information.",
            "`drop` variables the analysis no longer needs — the limit is usually reached by columns nobody reads.",
        ],
        golden: false,
    },
    RcCard {
        rc: 2000,
        title: "No observations",
        explain: "The estimation sample is empty. Every observation was excluded by the `if`, by the `in`, or by missing values in one of the variables in the model.",
        causes: &[
            "A missing value in any model variable drops the whole row.",
            "An `if` condition that matches nothing.",
            "A merge that produced no matched rows.",
        ],
        fixes: &[
            "`misstable summarize <varlist>` shows which variable is doing the dropping.",
            "`count if !missing(y, x1, x2)` gives the sample size before you fit.",
        ],
        golden: false,
    },
    RcCard {
        rc: 2001,
        title: "Insufficient observations",
        explain: "There are some observations, but fewer than the estimator needs for the number of parameters it must estimate.",
        causes: &[
            "More predictors than rows.",
            "A subgroup analysis on a very small group.",
            "Listwise deletion leaving too few complete cases.",
        ],
        fixes: &[
            "`summarize` the model variables to see the effective sample.",
            "Reduce the number of terms, or pool the subgroups.",
        ],
        golden: false,
    },
    RcCard {
        rc: 2002,
        title: "Everything is collinear",
        explain: "No independent variation is left in the predictors, so no coefficients can be identified.",
        causes: &[
            "A predictor that is constant within the estimation sample.",
            "A full set of indicator variables including the omitted category.",
            "The same variable entered twice under two names.",
        ],
        fixes: &[
            "`correlate` the predictors over the estimation sample.",
            "Factor-variable notation `i.group` handles indicators correctly and omits a base level automatically.",
        ],
        golden: false,
    },
    RcCard {
        rc: 3000,
        title: "Undefined name in an expression",
        explain: "A name in the expression is neither a variable, nor a scalar, nor a defined macro.",
        causes: &[
            "A macro that was never assigned expands to nothing, leaving a hole in the expression.",
            "A typo in a scalar name.",
        ],
        fixes: &[
            "`macro list` shows what is defined.",
            "An empty macro is the single most common cause of an expression that looks fine on screen.",
        ],
        golden: false,
    },
    RcCard {
        rc: 3200,
        title: "Conformability error",
        explain: "A Mata operation was given operands whose dimensions do not fit.",
        causes: &[
            "A row vector where a column vector belongs.",
            "A matrix built from data whose shape changed.",
        ],
        fixes: &[
            "Check `rows()` and `cols()` of each operand before the operation.",
        ],
        golden: false,
    },
    RcCard {
        rc: 3300,
        title: "Argument out of range",
        explain: "A function was given a value outside the range it accepts.",
        causes: &[
            "A logarithm of a non-positive number.",
            "A subscript past the end of a vector.",
            "A probability outside 0 to 1.",
        ],
        fixes: &[
            "Guard the call: `ln(x)` needs `x > 0`, and missing values are truthy in an `if`, so `if x > 0` alone does not exclude them.",
        ],
        golden: false,
    },
    RcCard {
        rc: 3498,
        title: "Numeric overflow",
        explain: "A computed value is too large for the storage type it is being written into.",
        causes: &[
            "A `byte` or `int` variable receiving a value outside its range.",
            "A product of two large numbers stored as `float`.",
        ],
        fixes: &[
            "`generate double <var> = ...` when the values can be large or need full precision.",
            "Stratum silently promotes nothing: the storage type you name is the one you get.",
        ],
        golden: false,
    },
];

/// The card for a return code, or `None` when we have not authored one.
///
/// `None` is a real answer and the caller must handle it: design 07 §6.1 sends
/// exactly that case to `[Explain]`, which is the only place the AI stack enters
/// the error path at all — "AI only if rc is not in the table **and** the user
/// clicks".
#[must_use]
pub fn card(rc: u32) -> Option<&'static RcCard> {
    CARDS
        .binary_search_by(|c| c.rc.cmp(&rc))
        .ok()
        .and_then(|i| CARDS.get(i))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn the_table_is_sorted_unique_and_searchable() {
        for w in CARDS.windows(2) {
            assert!(w[0].rc < w[1].rc, "out of order at r({})", w[0].rc);
        }
        for c in CARDS {
            assert_eq!(card(c.rc).map(|x| x.rc), Some(c.rc));
        }
        assert_eq!(card(4242), None);
    }

    /// The twelve error codes `tests/golden/stata18/errors.log` produces all
    /// have a card. (The log's thirteenth code is `r(0)`, which is success.)
    #[test]
    fn every_golden_return_code_has_a_card() {
        for rc in [7, 9, 100, 102, 107, 109, 110, 111, 133, 198, 199, 601] {
            let c = card(rc).unwrap_or_else(|| panic!("no card for r({rc})"));
            assert!(c.golden, "r({rc}) is in the golden and must be marked");
        }
        assert_eq!(CARDS.iter().filter(|c| c.golden).count(), 12);
    }

    #[test]
    fn every_card_is_filled_in() {
        for c in CARDS {
            assert!(!c.title.is_empty(), "r({}) has no title", c.rc);
            assert!(!c.title.ends_with('.'), "r({}) title is a title", c.rc);
            assert!(c.explain.len() > 40, "r({}) explanation is too thin", c.rc);
            assert!(!c.fixes.is_empty(), "r({}) offers no next step", c.rc);
        }
    }
}
