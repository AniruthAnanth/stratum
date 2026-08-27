#!/usr/bin/env python3
"""Differential comparison between Stata's log output and our runtime's output.

Stata log files carry a licence banner, absolute paths, timestamps and echoed
commands that are irrelevant to correctness. This normalizes both sides and
compares them line by line, treating numbers numerically (so 21.2973 and
21.29730 match, and a relative tolerance absorbs last-digit formatting) while
requiring non-numeric text to match exactly.

    scripts/stata_diff.py tests/golden/stata18/core_surface.log ours.txt

Exit status: 0 identical within tolerance, 1 differences found, 2 usage error.

This is developer tooling. Neither the normal build nor the normal CI pipeline
may depend on it or on Stata being installed (product spec section 32).
"""

import argparse
import re
import sys

# The banner runs from the start of the log to the first echoed `. do <file>`.
BANNER_END = re.compile(r"^\.\s+do\s+")
TRAILER = re.compile(r"^end of do-file\s*$")

# Volatile content that must never count as a difference.
VOLATILE = [
    (re.compile(r"/(?:Applications|Users|private|tmp|home)/\S+"), "<PATH>"),
    (re.compile(r"\b\d{1,2}\s+[A-Z][a-z]{2}\s+\d{4}\s+\d{2}:\d{2}(:\d{2})?"), "<TIMESTAMP>"),
    (re.compile(r"\b(?:Mon|Tue|Wed|Thu|Fri|Sat|Sun)\s+\w+\s+\d+.*\d{4}\b"), "<TIMESTAMP>"),
    (re.compile(r"^\s*Serial number:.*$"), "<LICENSE>"),
    (re.compile(r"^\s*Licensed to:.*$"), "<LICENSE>"),
    (re.compile(r"^\s*Stata license:.*$"), "<LICENSE>"),
]

# A number, including Stata's extended missing values and scientific notation.
NUMBER = re.compile(r"[-+]?(?:\d+\.?\d*(?:[eE][-+]?\d+)?|\.\d+(?:[eE][-+]?\d+)?|\.[a-z]\b)")


def strip_banner(lines):
    """Drop the licence banner and the trailing end-of-do-file marker."""
    start = 0
    for i, line in enumerate(lines):
        if BANNER_END.match(line):
            start = i + 1
            break
    out = lines[start:]
    while out and (TRAILER.match(out[-1]) or not out[-1].strip()):
        out.pop()
    return out


def normalize(lines, keep_echo):
    result = []
    for line in lines:
        line = line.rstrip("\n").rstrip()
        if not keep_echo and line.startswith(". "):
            # Echoed command; our runtime may not echo identically.
            continue
        for pattern, replacement in VOLATILE:
            line = pattern.sub(replacement, line)
        # Collapse run of spaces: column alignment is checked separately by the
        # layout tests, not by the numeric differential.
        result.append(line)
    while result and not result[-1].strip():
        result.pop()
    return result


def tokenize(line):
    """Split a line into alternating literal and numeric tokens."""
    tokens = []
    pos = 0
    for match in NUMBER.finditer(line):
        if match.start() > pos:
            tokens.append(("lit", line[pos:match.start()]))
        tokens.append(("num", match.group()))
        pos = match.end()
    if pos < len(line):
        tokens.append(("lit", line[pos:]))
    return tokens


def numbers_match(a, b, rtol, atol):
    if a == b:
        return True
    # Extended missing values compare as literals.
    if a.startswith(".") and len(a) == 2 and a[1].isalpha():
        return a == b
    if b.startswith(".") and len(b) == 2 and b[1].isalpha():
        return a == b
    try:
        x, y = float(a), float(b)
    except ValueError:
        return a == b
    if x == y:
        return True
    return abs(x - y) <= max(atol, rtol * max(abs(x), abs(y)))


def lines_match(a, b, rtol, atol, strict_layout):
    if a == b:
        return True
    if strict_layout:
        return False
    ta, tb = tokenize(a), tokenize(b)
    if len(ta) != len(tb):
        return " ".join(a.split()) == " ".join(b.split())
    for (ka, va), (kb, vb) in zip(ta, tb):
        if ka != kb:
            return False
        if ka == "num":
            if not numbers_match(va, vb, rtol, atol):
                return False
        elif va.split() != vb.split():
            return False
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("reference", help="Stata .log file")
    parser.add_argument("actual", help="our runtime's output")
    parser.add_argument("--rtol", type=float, default=1e-6,
                        help="relative tolerance for numeric comparison (default 1e-6)")
    parser.add_argument("--atol", type=float, default=1e-12,
                        help="absolute tolerance for numeric comparison (default 1e-12)")
    parser.add_argument("--keep-echo", action="store_true",
                        help="compare echoed '. command' lines too")
    parser.add_argument("--strict-layout", action="store_true",
                        help="require byte-identical lines (column alignment test)")
    parser.add_argument("--max-report", type=int, default=40,
                        help="stop reporting after this many differing lines")
    args = parser.parse_args()

    try:
        with open(args.reference, encoding="utf-8", errors="replace") as fh:
            ref = normalize(strip_banner(fh.readlines()), args.keep_echo)
        with open(args.actual, encoding="utf-8", errors="replace") as fh:
            # strip_banner is a no-op when there is no banner, so this works
            # both for our runtime's plain output and for a second Stata log.
            act = normalize(strip_banner(fh.readlines()), args.keep_echo)
    except OSError as exc:
        print("error: %s" % exc, file=sys.stderr)
        return 2

    diffs = 0
    for i in range(max(len(ref), len(act))):
        a = ref[i] if i < len(ref) else "<missing>"
        b = act[i] if i < len(act) else "<missing>"
        if not lines_match(a, b, args.rtol, args.atol, args.strict_layout):
            diffs += 1
            if diffs <= args.max_report:
                print("line %d:" % (i + 1))
                print("  stata : %s" % a)
                print("  ours  : %s" % b)

    if diffs:
        extra = " (%d more suppressed)" % (diffs - args.max_report) if diffs > args.max_report else ""
        print("\n%d differing line(s)%s" % (diffs, extra), file=sys.stderr)
        return 1

    print("identical within tolerance (%d lines compared)" % len(ref))
    return 0


if __name__ == "__main__":
    sys.exit(main())
