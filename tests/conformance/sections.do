// %% Setup
* Section markers, a block comment, a line continuation and non-ASCII text —
* all of it trivia, none of it in the plan (CONTRACTS §2). What this case adds
* over `envelope.do` is that the segmenter walks a file with real structure and
* still reports `plan_len` 0, so a platform that disagreed about UTF-8, about
* line endings, or about what counts as an executable region would show up as a
* different `plan_len` on the wire.

/* A block comment that spans
   several lines and mentions a path, C:\Users\ana\proj, which is inside a
   comment and must therefore never reach the stream at all. */

// %% Notes — αβγ, 東京, straße, 🌍
* A continuation inside a comment: ///
* still a comment.
