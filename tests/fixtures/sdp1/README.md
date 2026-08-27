# SDP1 reference fixtures

The wire format `CONTRACTS.md` §8.1 defines, as bytes on disk, so that the two
independent implementations of it can be compared against a third thing rather
than against each other.

| Fixture | Bytes | `header_len` | SHA-256 |
|---|---:|---:|---|
| `auto_40x12.bin` | 4,928 | 936 | `aef6a2130f04ac8344052136d4f0e30e30d32a1557e611901a6808dbb737084b` |
| `auto_40x12_edit.bin` | 5,536 | 912 | `622d370014bf5025e74125a410b0a2c88854ab7490e66341790872c40f517142` |
| `strl_3x2_edit.bin` | 449 | 184 | `c729e0cf29ff6ddbf31f2a553b9d01b79abca513168f89bb556f43c6d9835ac1` |

**`auto_40x12.bin` is the normative one.** `CONTRACTS.md` §8.1 names it by path,
`IMPLEMENTATION_PLAN` W02b requires `stratum_data::page()` to emit it
byte-for-byte, and W12 requires `decodeDataPage` to parse it. The other two are
supplementary: `auto_40x12.bin` is all `text` columns, so on its own it leaves
the `num` and `blob` branches of every decoder untested, and a decoder branch
with no fixture is a decoder branch that is wrong.

These files are owned by **W00**, not by W02 and not by W12 (A29). W12 runs on
days 3–8 and W02 on days 9–18; the pre-audit plan had W12's acceptance test
depending on a file W02 would not check in for another nine days.

---

## 1. The layout

Little-endian throughout. `H` below is `header_len`.

```
offset   size  field
0        4     magic = b"SDP1"
4        4     header_len : u32
8        H     header : UTF-8 JSON, right-padded with ASCII spaces
8+H      …     column payloads
```

The JSON header, exactly as `CONTRACTS.md` §8.1 gives it:

```json
{ "state": u64, "row0": u64, "nrows": u32, "seq": u32,
  "cols": [ { "idx": u32, "kind": "text"|"num"|"blob",
              "off": u64, "len": u64,
              "aux_off": u64, "aux_len": u64 } ] }
```

`off` and `aux_off` are byte offsets **relative to `8+H`**, the first payload
byte. `len` and `aux_len` are byte lengths of those two regions.

Per kind, from §8.1:

| `kind` | `aux` region | `data` region |
|---|---|---|
| `text` | `(nrows+1) × u32` offsets into the arena | UTF-8 arena |
| `num` | `nrows × u8` missing tags | `nrows × f64` |
| `blob` | `(nrows+1) × u32` offsets into the arena | byte arena **+ trailing `ceil(nrows/8)`-byte bitmap** |

- **`text` offsets** are relative to the start of *that column's* arena,
  ascending, with `aux[0] == 0` and `aux[nrows] == len`. Cell `i` is
  `arena[aux[i] .. aux[i+1]]`. There is no terminator and no padding; an empty
  cell is two equal offsets.
- **`num` tags** are `255` for "not missing", `0` for `.`, and `1..=26` for
  `.a`..`.z`. The `f64` carries Stata's own sentinel bit pattern for a missing
  cell — `0x7FE0_0000_0000_0000 + (tag << 40)`, i.e. `stratum_core::missing::
  missing_f64(tag)` — so the tag column is redundant with the payload by
  construction and a decoder may check one against the other. It is carried
  anyway because JavaScript cannot cheaply pattern-match `f64` bits, and
  `06` §15 needs the distinction per cell.
- **`blob` values** may be arbitrary bytes; the bitmap says which. See §2.3.

## 2. What §8.1 does not say, and what this fixture decided

Four things are underdetermined by `CONTRACTS.md` §8.1 as written. A byte-exact
fixture cannot be agnostic about any of them, so W00 ruled, and these rulings
are as normative as the table above for anyone matching these bytes.

### 2.1 The header is padded so the payload starts 8-aligned

`header_len` counts the padding. The header is emitted as **compact JSON** — no
space after `:` or `,`, keys in exactly the order §8.1 lists them, integers as
plain decimal — and then right-padded with ASCII spaces (`0x20`) until
`(8 + header_len) % 8 == 0`.

This is not tidiness. §8.1 says the client decodes "with `DataView` +
typed-array views over one `ArrayBuffer`", and `new Float64Array(buf, byteOffset,
n)` throws a `RangeError` unless `byteOffset` is a multiple of 8. Without the
padding rule, whether a `num` column can be viewed at all depends on how many
digits the row count happens to have.

JSON tolerates trailing whitespace, so `JSON.parse` over the raw slice
`buf.slice(8, 8 + header_len)` is unaffected. A writer must emit spaces and
nothing else; a reader should not assume the padding is absent.

### 2.2 Every region is aligned to its element

Because `8+H` is 8-aligned, aligning the *relative* offset aligns the absolute
one:

| region | alignment |
|---|---|
| `num` `data` (f64) | 8 |
| `text` / `blob` `aux` (u32) | 4 |
| `num` `aux` (u8), `text` / `blob` `data` | 1 |

Bytes inserted to satisfy an alignment are zero and belong to no region. Columns
are laid out in `idx` order, and within one column the two regions are laid down
in the order §8.1's own table lists them for that kind: `aux` then `data` for
`text` and `blob`, `data` then `aux` for `num`.

The payload ends at the last region's end. There is no trailing slack, so
`file_len == 8 + header_len + payload_len` is checkable.

### 2.3 The `blob` bitmap lives inside `data`

§8.1 describes the bitmap as "a trailing `ceil(nrows/8)` byte bitmap" without
saying which region it belongs to. It is the **last `ceil(nrows/8)` bytes of the
column's `data` region**:

```
len       == aux[nrows] + ceil(nrows/8)
arena     == data[0 .. aux[nrows]]
bitmap    == data[aux[nrows] .. len]
```

Any other reading leaves the bitmap outside every declared extent, which a
decoder cannot bounds-check. This way both the arena length and the bitmap
length are derivable two ways and disagreement is detectable.

Bit `i & 7` of byte `i >> 3`, **LSB first**, so row 0 is bit 0 of byte 0. A set
bit means the value is binary — GSO type 129 — and must not be decoded as UTF-8.

### 2.4 A `Display` cell carries no format padding

`RenderMode::Display` means "cells already formatted per each variable's
`StataFormat`, value labels applied". It does not say whether `%8.0gc` pads the
result to eight columns.

It does not. A cell holds what Stata's own `string(x, "%fmt")` returns, which is
already trimmed, or — when the variable has a value label with an entry for that
value — the label text, or, for a string variable, the stored value verbatim.
`design/tokens.json` settles it from the other end: column alignment is
`text-align: right` plus a per-column ch padding computed from the display
format, "never a `text-align: '.'` hack and never per-cell measurement". Padding
inside the cell would fight that.

So `price` obs 1 is `4,099` and not `   4,099`; `headroom` obs 1 is `2.5` and not
`   2.5`; `foreign` is `Domestic`, not `0`. A labelled value with no matching
label entry falls back to the formatted number.

In `RenderMode::Edit` a string column is `text` holding the **raw** bytes, with
no format applied and no label substituted. For `make` (`str18`, `%-18s`) the
two renderings coincide, which is why the `make` column is byte-identical
between `auto_40x12.bin` and `auto_40x12_edit.bin`.

---

## 3. The fixtures

All three use `row0 = 0` and `seq = 1`. The two `auto` pages use
`state = 17` — spec §13's own worked example, "Dataset state: D17" — rather
than `0`, so that a decoder which forgets to read `state` fails visibly instead
of accidentally agreeing.

### `auto_40x12.bin` — `RenderMode::Display`

The first 40 observations of all 12 variables of `tests/fixtures/dta/auto.dta`,
in dataset order (which is `sorted by: foreign`, so all 40 rows are `Domestic`).
Twelve `text` columns, `idx` 0–11 in storage order:

| `idx` | variable | type | format | value label |
|---:|---|---|---|---|
| 0 | `make` | `str18` | `%-18s` | |
| 1 | `price` | `int` | `%8.0gc` | |
| 2 | `mpg` | `int` | `%8.0g` | |
| 3 | `rep78` | `int` | `%8.0g` | |
| 4 | `headroom` | `float` | `%6.1f` | |
| 5 | `trunk` | `int` | `%8.0g` | |
| 6 | `weight` | `int` | `%8.0gc` | |
| 7 | `length` | `int` | `%8.0g` | |
| 8 | `turn` | `int` | `%8.0g` | |
| 9 | `displacement` | `int` | `%8.0g` | |
| 10 | `gear_ratio` | `float` | `%6.2f` | |
| 11 | `foreign` | `byte` | `%8.0g` | `origin` |

It exercises the four formatting behaviours that actually differ between
implementations: comma insertion (`%8.0gc` → `4,099`, `2,930`), fixed decimals
including a trailing zero (`%6.1f` → `3.0`), value-label substitution
(`0` → `Domestic`), and a missing value rendered as `.` (`rep78`, observations 3
and 7).

All 480 cells were captured from **StataNow 18.5 MP** on this repository's own
`auto.dta`, not written by hand. They are the oracle, so a mismatch is a bug in
`stratum_core::fmt`, not in the fixture.

The first bytes of the payload are column 0's offset array, and the first three
strings follow it:

```
0x03B0  00 00 00 00  0b 00 00 00  14 00 00 00  1e 00 00 00   aux[0..4] = 0, 11, 20, 30
0x0454  "AMC Concord" "AMC Pacer" "AMC Spirit" …             the arena at off = 164
```

### `auto_40x12_edit.bin` — `RenderMode::Edit`

The same page, unformatted. Column 0 is `text` holding `make`'s raw bytes;
columns 1–11 are `num`. Every `num` column is 320 bytes of `f64` plus 40 tag
bytes.

The two missing cells are `rep78` observations 3 and 7. Both are the plain
system missing: tag `0` and payload bits `0x7FE0_0000_0000_0000`. No extended
missing (`.a`–`.z`) appears anywhere in `auto.dta`, so a decoder that hard-codes
"tag 0" passes this fixture — `alltypes.dta` is where the extended missings live,
and a fixture for it is worth adding when something needs one.

Note that `headroom` and `gear_ratio` are Stata `float`s widened to `f64`, so
`gear_ratio` obs 1 is `3.5799999237060547` and not `3.58`. That is correct and
required: `04` §2.6 says a `float` widens through its exact `f32` value.

### `strl_3x2_edit.bin` — `RenderMode::Edit`, the `blob` branch

All 3 observations × 2 variables of `tests/fixtures/dta/strl.dta`. `state = 1`.
`idx` 0 is `big` (`strL`) as `blob`; `idx` 1 is `small` (`str5`) as `text`.

Its whole header fits on one line, which makes it the one to read first:

```
{"state":1,"row0":0,"nrows":3,"seq":1,"cols":[{"idx":0,"kind":"blob","off":16,"len":214,"aux_off":0,"aux_len":16},{"idx":1,"kind":"text","off":248,"len":9,"aux_off":232,"aux_len":16}]}
```

Read it against §2.2 and §2.3: `aux_len = 16 = 4 × (3+1)`; the blob arena is
`aux[3] = 213` bytes and `len = 213 + ceil(3/8) = 214`; column 1's `aux_off` of
232 is 4-aligned where the running cursor was at 230.

Row 3 of `big` is the empty `strL`, so `aux[2] == aux[3]` — the case where a
decoder that treats "zero length" as "absent" gets it wrong. All three values
are text, so **the bitmap is all zeros**. That is faithful to `strl.dta` rather
than convenient: nothing in the repository has a genuinely binary `strL` yet.
The bitmap's *shape* is exercised; a set bit is not. Whoever adds a binary-`strL`
`.dta` fixture should add the matching page here.

---

## 4. Regenerating

`cargo xtask sdp1` writes all three from `tests/fixtures/dta/*.dta`. It is not
run in CI: these are checked-in oracles, and a generator that silently rewrites
its own oracle proves nothing.

Regenerating `auto_40x12.bin` needs the display strings, which come from Stata
and not from us. `scripts/capture-golden.sh` is the tool; the do-file writes one
`col<TAB>row<TAB>text` line per cell using `string(x, "%fmt")` for a plain
numeric, the `label` extended macro function for a labelled one, and the stored
value for a string. If Stata is not installed, `xtask sdp1` must refuse rather
than fall back to our own formatter — the entire value of this fixture is that
one side of the comparison did not come from our code.

Provenance of the current bytes: StataNow 18.5 MP, macOS, 2026-08-22, against
`tests/fixtures/dta/auto.dta` and `strl.dta` as committed.
