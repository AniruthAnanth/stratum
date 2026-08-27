/**
 * Generated editor programs, driven against every backend at once.
 *
 * `conformance.ts` next door checks a list of things W11a thought to check. That
 * is evidence about those things and about nothing else, and the acceptance
 * bullet is a stronger claim than that: *no* consumer may be able to tell one
 * backend from another. A hand-written list cannot support a claim about every
 * consumer, because the consumer that finds the difference is by definition the
 * one nobody wrote a check for — and the consumer of record, W13, does not exist
 * yet to be asked.
 *
 * So this file generates the consumers instead of enumerating the checks. Each
 * seed produces one editor session — the calls a CodeMirror-backed editor makes,
 * in an order and with arguments nobody chose — as plain data. That one program
 * then runs against every backend in lockstep, and each step is compared two
 * ways:
 *
 * * **Laws**, between every pair of backends. The document mirror, generation
 *   discipline, the environment generation, and the A11 truncation triple hold
 *   whatever segmentation rule a backend runs, so they survive W11b relinking
 *   one side to `stratum-parse`.
 * * **Output verbatim**, between two backends running the *same* rule — in wave
 *   1 the TypeScript stub against `ReferenceSegmenter` in wasm. Region rows and
 *   hashes, tokens, sections, narrative runs, lints, quick fixes and the range a
 *   completion would replace, on documents nobody hand-picked, over edit
 *   sequences nobody hand-picked. This is the half that crosses real wasm linear
 *   memory, so a stride, endianness or slot-order mistake surfaces here rather
 *   than in W13.
 *
 * Beyond comparing backends, every step asserts what holds of a segmenter on its
 * own — `regionCount()` against `regions()`, `region(i)` against the list, the
 * tiling of outer spans, `regionAt` against what the list says is there, the
 * declared key set of every object handed back, and the mirror against an
 * independently computed model of the document. A session then ends by opening
 * its final text in a fresh segmenter and demanding the same segmentation. Those
 * are what catch a backend that is *consistently* wrong on both sides, which no
 * amount of comparing the two to each other can.
 *
 * Seeds are fixed, not drawn from the clock: a fuzzer that fails on a different
 * input every night is a fuzzer nobody bisects. A failure prints the seed, the
 * step, and the whole program as JSON, so it replays exactly.
 *
 * `conformance.ts` imports {@link runDifferential} and folds its results into
 * the one report; the type imports back the other way are erased by
 * `verbatimModuleSyntax`, so the module cycle is in the type graph only.
 */

import type { BackendUnderTest, CheckResult } from "./conformance.ts";
import type { Diagnostic, DocChange, RegionView, StratumSegmenter } from "./types.ts";

/**
 * The two diagnostics whose entire purpose is to say which backend you are
 * looking at: the stub's dev banner, and the harness-only build's "no segmenter
 * linked". They are the *one* sanctioned way the backends differ, because
 * W11a's other acceptance bullet requires the stub to be loud about being the
 * stub — a silent stub is a stub that ships. Everything else a backend says has
 * to match, key sets and banners included, and that is asserted below rather
 * than assumed.
 */
const SELF_IDENTIFYING = new Set(["WASM0900", "WASM0006"]);

/** The `CompletionEnv` fields a generated session varies. */
export interface EnvSpec {
  /** Engine generation the environment was captured at. */
  generation: number;
  /** How many variable names it carries. */
  varCount: number;
  /** How many exist in the frame (A11: the engine decides what was shed). */
  total: number;
  /** Whether the engine capped it. */
  truncated: boolean;
}

/** One call in a generated editor session. */
type Op =
  | { kind: "open"; text: string }
  | { kind: "edit"; changes: DocChange[] }
  | { kind: "resegment" }
  | { kind: "env"; spec: EnvSpec }
  | { kind: "complete"; pos: number }
  | { kind: "tokens"; from: number; to: number };

/** Calls per generated session. */
const STEPS = 24;

/**
 * The seeds, fixed so a failure is reproducible from the check name alone.
 * Adjacent seeds are fine: mulberry32 decorrelates them in one round.
 */
const SEEDS: readonly number[] = Array.from({ length: 12 }, (_, i) => 0x517a_7a00 + i);

/** Bytes of one bulk paste, chosen to force wasm linear memory to grow. */
const BULK_LINES = 700;

/**
 * Lines a generated document is built from.
 *
 * Everything the naive rule gets wrong on purpose is in here — `///` folds,
 * `#delimit ;`, `/*md`, `//|`, braces, a macro in command position — because the
 * same-rule comparison is worth most exactly where the rule is worst: both
 * backends have to be wrong *identically*. The last four carry a 2-byte, a
 * 3-byte and a 4-byte character, so byte offsets and UTF-16 offsets disagree at
 * three different widths in the same document.
 */
const FRAGMENTS: readonly string[] = [
  "sysuse auto, clear\n",
  "regress price mpg weight\n",
  "// %% Section head\n",
  "* a star comment\n",
  "summarize price ///\n    , detail\n",
  "#delimit ;\nlist price ;\n#delimit cr\n",
  "/*md\nnarrative block\n*/\n",
  "//| a narrative line\n",
  "foreach v of varlist price mpg {\n    display `v'\n}\n",
  "local x = 1\n",
  "`cmd' price\n",
  "program define p\n    display 1\nend\n",
  "\n",
  "    \n",
  "generate prix = price * 1.1  // en euros é\n",
  'label var price "prix → euros"\n',
  "// 📊 a chart\n",
  'display "aéb→c📊"\n',
];

/** Short strings a generated edit inserts. */
const INSERTS: readonly string[] = [
  "",
  "x",
  " ",
  "\n",
  "list\n",
  "// c\n",
  "///\n",
  "{\n",
  "}\n",
  "#delimit ;\n",
  "é",
  "→",
  "📊",
  "regress y x\n",
];

/** What one step of one backend produced, in its two comparable projections. */
interface Observation {
  /** True of any backend, whatever rule it runs. */
  law: string;
  /** True only of two backends running the same rule. */
  strict: string;
}

/** A backend with a live segmenter on it. */
interface Live {
  backend: BackendUnderTest;
  seg: StratumSegmenter;
  generation: number;
}

/** How much a run actually compared, so a vacuous green is detectable. */
interface Compared {
  law: number;
  strict: number;
}

/**
 * Run every generated session against every backend.
 *
 * `encodeEnv` is passed in rather than imported so this module holds no second
 * msgpack writer: the harness has exactly one, in `conformance.ts`, and both
 * backends must be fed the same bytes the engine would broadcast.
 */
export async function runDifferential(
  backends: BackendUnderTest[],
  encodeEnv: (spec: EnvSpec) => Uint8Array,
): Promise<CheckResult[]> {
  const checks: CheckResult[] = [];
  const total: Compared = { law: 0, strict: 0 };
  const coverage = new Map<Op["kind"], number>();
  let bulk = 0;

  for (const seed of SEEDS) {
    const ops = generateSession(seed);
    for (const op of ops) coverage.set(op.kind, (coverage.get(op.kind) ?? 0) + 1);
    if (ops.some(isBulk)) bulk++;
    const name = `session ${seed.toString(16)}: ${ops.length} generated editor calls`;
    try {
      const compared = await driveSession(backends, ops, encodeEnv);
      total.law += compared.law;
      total.strict += compared.strict;
      checks.push({ subject: "differential", name, status: "pass" });
    } catch (e) {
      checks.push({
        subject: "differential",
        name,
        status: "fail",
        // The program, not just the failure: a generated session is only
        // evidence if the reader can replay the exact one that broke.
        detail: `${e instanceof Error ? e.message : String(e)}\n  replay: ${JSON.stringify(ops)}`,
      });
    }
  }

  // Anti-vacuity. A generator bug that emitted 288 `resegment`s would leave
  // every session above green while testing one method, and a pair loop that
  // compared nothing would too.
  checks.push(coverageCheck(coverage, bulk));
  if (backends.length < 2) {
    checks.push({
      subject: "differential",
      name: "compared every pair of backends",
      status: "skip",
      detail:
        "only one backend is present, so the sessions asserted per-backend " +
        "invariants and compared nothing; run `cargo xtask wasm` to build the " +
        "real module and get the cross-backend half",
    });
  } else {
    const sameRule = backends.some((a, i) =>
      backends.slice(i + 1).some((b) => a.rule === b.rule && a.segments && b.segments),
    );
    checks.push({
      subject: "differential",
      name: "compared every pair of backends",
      status: total.law > 0 && (!sameRule || total.strict > 0) ? "pass" : "fail",
      detail: `${total.law} law comparisons, ${total.strict} verbatim`,
    });
  }
  return checks;
}

function coverageCheck(coverage: Map<Op["kind"], number>, bulk: number): CheckResult {
  const kinds: Op["kind"][] = ["open", "edit", "resegment", "env", "complete", "tokens"];
  const thin = kinds.filter((k) => (coverage.get(k) ?? 0) < 5);
  const detail = kinds.map((k) => `${k}=${coverage.get(k) ?? 0}`).join(" ");
  if (thin.length > 0) {
    return {
      subject: "differential",
      name: "the generated sessions exercise the whole surface",
      status: "fail",
      detail: `barely generated: ${thin.join(", ")} (${detail})`,
    };
  }
  if (bulk === 0) {
    return {
      subject: "differential",
      name: "the generated sessions exercise the whole surface",
      status: "fail",
      // Without one, `reserve` never grows linear memory and the detached-view
      // failure mode — the one only the real module can have — goes untested.
      detail: `no session pasted a bulk document (${detail})`,
    };
  }
  return {
    subject: "differential",
    name: "the generated sessions exercise the whole surface",
    status: "pass",
    detail: `${detail} bulk=${bulk}`,
  };
}

/**
 * Drive one program against every backend, comparing after every call.
 *
 * Lockstep rather than record-then-diff: it costs one live segmenter per backend
 * and it means a divergence is reported at the step that caused it, with both
 * payloads in hand, instead of as two multi-megabyte transcripts that differ
 * somewhere.
 */
async function driveSession(
  backends: BackendUnderTest[],
  ops: readonly Op[],
  encodeEnv: (spec: EnvSpec) => Uint8Array,
): Promise<Compared> {
  const lives: Live[] = [];
  for (const backend of backends) {
    const seg = await backend.load();
    lives.push({ backend, seg, generation: seg.generation });
  }

  const compared: Compared = { law: 0, strict: 0 };
  let model = "";
  // Whether `regions()` describes the document as it is now. Between an edit and
  // the next `resegment` it describes the previous one, which is legitimate and
  // is why the tiling law is not asserted there.
  let clean = true;

  try {
    for (const [index, op] of ops.entries()) {
      model = applyToModel(model, op);
      if (op.kind === "open" || op.kind === "edit") clean = false;
      if (op.kind === "resegment") clean = true;

      const seen: Array<{ name: string; observation: Observation }> = [];
      for (const live of lives) {
        try {
          const fromOp = execute(live, op, encodeEnv);
          const fromReads = observe(live.seg, model, clean, live.backend.segments);
          seen.push({
            name: live.backend.name,
            observation: {
              law: `${fromOp.law} ${fromReads.law}`,
              strict: `${fromOp.strict} ${fromReads.strict}`,
            },
          });
        } catch (e) {
          throw new Error(
            `${live.backend.name} failed at step ${index} (${describe(op)}): ` +
              `${e instanceof Error ? e.message : String(e)}`,
          );
        }
      }

      for (let i = 0; i < seen.length; i++) {
        for (let j = i + 1; j < seen.length; j++) {
          const a = seen[i];
          const b = seen[j];
          const ra = lives[i];
          const rb = lives[j];
          if (!a || !b || !ra || !rb) continue;
          compared.law++;
          if (a.observation.law !== b.observation.law) {
            throw new Error(
              `${a.name} and ${b.name} are distinguishable at step ${index} ` +
                `(${describe(op)}):\n    ${a.name}: ${a.observation.law}\n    ` +
                `${b.name}: ${b.observation.law}`,
            );
          }
          if (ra.backend.rule !== rb.backend.rule || !ra.backend.segments || !rb.backend.segments) {
            continue;
          }
          compared.strict++;
          if (a.observation.strict !== b.observation.strict) {
            throw new Error(
              `two ${ra.backend.rule} backends produced different output at step ` +
                `${index} (${describe(op)}):\n    ${a.name}: ${a.observation.strict}\n    ` +
                `${b.name}: ${b.observation.strict}`,
            );
          }
        }
      }
    }
    for (const live of lives) await checkResync(live, model);
  } finally {
    for (const live of lives) live.seg.destroy();
  }
  return compared;
}

/**
 * A session of edits lands on the same segmentation as opening the result.
 *
 * The one law here that neither the model nor the other backend can supply.
 * `segmenter.ts` keeps the document twice — a JS mirror it answers `docText()`
 * from, and the engine's own buffer, which it maintains with `splice` calls and
 * a running delta. Both backends run that same code, so a delta bug puts the
 * *same* wrong bytes in both engines: the mirror still matches the model, the
 * two backends still agree with each other, the outer spans still tile, and
 * every comparison in this file passes over a document the engine never had.
 * `checkSync` compares lengths per transaction, which is the right cost in
 * production and blind to a same-length drift.
 *
 * Segmenting is a pure function of the text, so opening the same text in a fresh
 * segmenter has to produce the same regions. It is the cheapest available oracle
 * for the engine-side buffer, and it needs nothing but the public surface.
 */
async function checkResync(live: Live, model: string): Promise<void> {
  live.seg.resegment();
  const incremental = canonical(live.seg.regions().map(row));
  const fresh = await live.backend.load();
  try {
    fresh.setDoc(model);
    fresh.resegment();
    const reopened = canonical(fresh.regions().map(row));
    if (incremental !== reopened) {
      throw new Error(
        `${live.backend.name}: the edit sequence and a fresh open of the same ` +
          `${model.length}-unit document disagree:\n    edited:   ${incremental}\n    ` +
          `reopened: ${reopened}`,
      );
    }
  } finally {
    fresh.destroy();
  }
}

/** Perform one call, and record what it returned. */
function execute(
  live: Live,
  op: Op,
  encodeEnv: (spec: EnvSpec) => Uint8Array,
): { law: string; strict: string } {
  const { seg } = live;
  switch (op.kind) {
    case "open":
      seg.setDoc(op.text);
      return { law: "open", strict: "open" };
    case "edit":
      seg.applyChanges(op.changes);
      return { law: "edit", strict: "edit" };
    case "resegment": {
      const before = live.generation;
      const after = seg.resegment();
      if (after < before) {
        throw new Error(`resegment() went backwards: ${before} then ${after}`);
      }
      if (seg.generation !== after) {
        throw new Error(`resegment() returned ${after} but .generation is ${seg.generation}`);
      }
      live.generation = after;
      // The delta, not the number: two engines counting from different bases are
      // still interchangeable, two engines disagreeing about *whether* an edit
      // happened are not.
      return { law: `resegment:${after === before ? "same" : "advanced"}`, strict: "resegment" };
    }
    case "env": {
      seg.setCompletionEnv(encodeEnv(op.spec));
      // The popup reads this to tell whether the `StateChanged` it just saw has
      // been applied yet; a backend that drops it shows stale variables with no
      // way for anyone to notice.
      const loaded = seg.completionEnvGeneration();
      if (loaded !== op.spec.generation) {
        throw new Error(`pushed environment ${op.spec.generation}, engine reports ${loaded}`);
      }
      return { law: "env", strict: "env" };
    }
    case "complete": {
      const list = seg.complete(op.pos);
      if (!Array.isArray(list.items)) throw new Error("complete() returned no item array");
      if (list.offered > list.total) {
        throw new Error(`complete() offered ${list.offered} of ${list.total}`);
      }
      if (list.from > list.to) {
        throw new Error(`complete() range is inverted: ${list.from}..${list.to}`);
      }
      // The candidate list is the completion *source*'s product, and the two
      // wave-1 backends implement it differently on purpose: the Rust reference
      // offers nothing at all until W04b's command table and W20's dataflow
      // index exist, and the TypeScript stub prefix-matches the pushed
      // environment so the popup can be built before either lands. Comparing
      // the items would be asserting that one of those decisions is wrong.
      //
      // The replace range is not a candidate. It is the token under the cursor —
      // offset arithmetic across the wasm boundary — so it is compared between
      // backends running the same rule; a real parser is entitled to a different
      // idea of where a token starts, which is why it is not a law.
      //
      // The A11 triple is a law, and only where it is one: when the environment
      // was capped, `offered`/`total` are the engine's numbers, stamped onto
      // whatever the source produced, and every backend must report them.
      // Uncapped, they are the length of the candidate list — the source's
      // product again, 24 keywords here and nothing there.
      checkShape("CompletionItem", list.items, ITEM_KEYS);
      const capped = list.truncated ? `:${list.offered}/${list.total}` : "";
      return {
        law: `complete(${op.pos}):truncated=${list.truncated}${capped}`,
        strict: `complete(${op.pos}):${list.from}..${list.to}`,
      };
    }
    case "tokens": {
      const tokens = seg.tokens(op.from, op.to);
      if (!Array.isArray(tokens)) throw new Error("tokens() is not an array");
      const len = seg.docText().length;
      for (const t of tokens) {
        if (t.from > t.to) throw new Error(`token ${t.from}..${t.to} is inverted`);
        if (t.from < 0 || t.to > len) throw new Error(`token ${t.from}..${t.to} escapes 0..${len}`);
      }
      return {
        law: `tokens(${op.from},${op.to}):${tokens.length >= 0}`,
        strict: `tokens(${op.from},${op.to}):${JSON.stringify(tokens.map((t) => [t.from, t.to, t.tagCode]))}`,
      };
    }
  }
}

/**
 * Every read method, plus the invariants that hold of a segmenter on its own.
 *
 * This runs after *every* call, including the ones that only read: a wrapper
 * whose offset tables desynchronise does it on some particular transaction, and
 * checking only after edits would let the next read find it and blame the
 * wrong step.
 */
function observe(
  seg: StratumSegmenter,
  model: string,
  clean: boolean,
  segments: boolean,
): Observation {
  const mirror = seg.docText();
  if (mirror !== model) {
    throw new Error(
      `the document mirror diverged from the model: ${brief(mirror)} vs ${brief(model)}`,
    );
  }

  const regions = seg.regions();
  const count = seg.regionCount();
  if (count !== regions.length) {
    throw new Error(`regionCount() says ${count}, regions() returned ${regions.length}`);
  }
  if (seg.region(-1) !== null) throw new Error("region(-1) is not null");
  if (seg.region(count) !== null) throw new Error(`region(${count}) is not null of ${count}`);
  for (const i of spread(regions.length)) {
    if (JSON.stringify(seg.region(i)) !== JSON.stringify(regions[i])) {
      throw new Error(`region(${i}) disagrees with regions()[${i}]`);
    }
  }

  if (clean && segments && regions.length > 0) {
    checkTiling(regions, model.length);
  }

  if (seg.regionAt(-1) !== null) throw new Error("regionAt(-1) is not null");
  // Only against a segmentation that describes the document as it is now.
  // Between an edit and the next `resegment`, `regions()` carries byte offsets
  // into a document that no longer exists, and converting them through the
  // current offset map is not order-preserving: an old boundary can land inside
  // a character the edit created, so the byte-space search `regionAt` runs and
  // the unit-space containment this asserts disagree by a code unit. That is a
  // property of asking a stale segmenter, not a difference between backends —
  // both run this same wrapper — and CM6 resegments inside the transaction
  // cycle, before anything paints.
  if (clean) {
    for (const pos of spread(model.length)) checkRegionAt(seg, regions, pos);
  }

  if (clean && segments && regions.length > 0) {
    // The cursor at end of file. Documented behaviour, so it is pinned: the
    // position is not inside any outer span, and `regionAt` answers the last
    // region rather than nothing.
    const last = regions[regions.length - 1];
    const eof = seg.regionAt(model.length);
    if (last && eof?.index !== last.index) {
      throw new Error(`regionAt(${model.length}) is ${String(eof?.index)}, not the last region`);
    }
  }

  const sections = seg.sections();
  const narrative = seg.narrativeRegions();
  const lints = seg.lints();
  // Drains the accumulated splice faults, so exactly once per step and in the
  // same place for every backend.
  const diagnostics = seg.diagnostics();
  const fixes = seg.quickFixes(Math.min(model.length, Math.max(0, model.length >> 1)));
  for (const [what, value] of [
    ["sections", sections],
    ["narrativeRegions", narrative],
    ["lints", lints],
    ["diagnostics", diagnostics],
    ["quickFixes", fixes],
  ] as Array<[string, unknown]>) {
    if (!Array.isArray(value)) throw new Error(`${what}() is not an array`);
  }
  if (seg.abi <= 0) throw new Error(`abi is ${seg.abi}`);
  if (typeof seg.completionEnvGeneration() !== "number") {
    throw new Error("completionEnvGeneration() is not a number");
  }

  // Not the content — a backend is allowed to say that it is the stub — but the
  // shape. A diagnostic missing `related`, or carrying `undefined` where the
  // type says `null`, is one an editor can fingerprint.
  checkDiagnostics([...diagnostics, ...lints]);
  checkShape("Suggestion", fixes, SUGGESTION_KEYS);

  const banners = diagnostics.filter((d) => SELF_IDENTIFYING.has(d.code));
  if (banners.length > 1) {
    throw new Error(`${banners.length} backend-identity diagnostics in one segmentation`);
  }
  const spoken = diagnostics.filter((d) => !SELF_IDENTIFYING.has(d.code));

  return {
    law: [
      `doc=${model.length}`,
      `abi=${seg.abi}`,
      `envGen=${seg.completionEnvGeneration()}`,
      `arrays=${sections.length >= 0}`,
    ].join(" "),
    strict: [
      `regions=${canonical(regions.map(row))}`,
      `sections=${canonical(sections)}`,
      `narrative=${canonical(narrative)}`,
      `lints=${canonical(lints)}`,
      `diagnostics=${canonical(spoken)}`,
      `fixes=${canonical(fixes)}`,
    ].join(" "),
  };
}

/** `stratum_proto::Diagnostic`, every field, as `types.ts` declares it. */
const DIAGNOSTIC_KEYS = [
  "severity",
  "code",
  "stata_rc",
  "message",
  "file",
  "span",
  "offending_token",
  "block",
  "related",
  "suggestions",
  "notes",
  "confidence",
];

/** `CompletionItem`. */
const ITEM_KEYS = ["label", "kind", "detail", "insert", "rank"];

/** `stratum_proto::Suggestion`. */
const SUGGESTION_KEYS = ["label", "kind", "edits"];

/**
 * Every object a backend hands back carries exactly the keys the contract
 * declares, and none of them `undefined`.
 *
 * Checked against the contract rather than against the other backend, because
 * the counts legitimately differ — one backend offering no candidates and
 * another offering forty is not a difference in *shape* — and because a law
 * that compares two backends to each other passes when both are wrong.
 *
 * The `undefined` half is the one that actually bit: `serde_wasm_bindgen` maps
 * `None` to `undefined` by default, `JSON.stringify` then drops the key
 * entirely, and every comparison that goes through JSON says the two backends
 * agree while `d.span === null` says they do not.
 */
function checkShape(what: string, values: readonly unknown[], keys: readonly string[]): void {
  const wanted = [...keys].sort().join(",");
  for (const value of values) {
    const record = value as Record<string, unknown>;
    const actual = Object.keys(record).sort().join(",");
    if (actual !== wanted) {
      throw new Error(`a ${what} has keys [${actual}]; the contract declares [${wanted}]`);
    }
    for (const key of keys) {
      if (record[key] === undefined) {
        throw new Error(
          `a ${what} carries undefined in \`${key}\`, which types.ts declares nullable`,
        );
      }
    }
  }
}

/** Diagnostics, and the suggestions hanging off them. */
function checkDiagnostics(diagnostics: readonly Diagnostic[]): void {
  checkShape("Diagnostic", diagnostics, DIAGNOSTIC_KEYS);
  for (const d of diagnostics) checkShape("Suggestion", d.suggestions, SUGGESTION_KEYS);
}

/**
 * `JSON.stringify` with object keys sorted.
 *
 * Two backends whose objects differ only in key order are still
 * interchangeable — nothing the editor does can see the difference — so
 * comparing raw `JSON.stringify` output would fail them for nothing. Key
 * *sets*, which an editor can see, are compared as a law above.
 */
function canonical(value: unknown): string {
  return JSON.stringify(value, (_key, v: unknown) => {
    if (v === null || typeof v !== "object" || Array.isArray(v)) return v;
    const record = v as Record<string, unknown>;
    const sorted: Record<string, unknown> = {};
    for (const key of Object.keys(record).sort()) sorted[key] = record[key];
    return sorted;
  });
}

/** Outer spans tile the document, and every span sits inside its own outer. */
function checkTiling(regions: readonly RegionView[], length: number): void {
  let cursor = 0;
  for (const r of regions) {
    if (r.outerFrom !== cursor) {
      throw new Error(
        `outer spans do not tile: region ${r.index} opens at ${r.outerFrom}, expected ${cursor}`,
      );
    }
    if (r.outerTo < r.outerFrom) throw new Error(`region ${r.index} has an inverted outer span`);
    if (r.to < r.from) throw new Error(`region ${r.index} has an inverted span`);
    if (r.from < r.outerFrom || r.to > r.outerTo) {
      throw new Error(`region ${r.index} escapes its outer span`);
    }
    if (r.headLine > r.lastLine) throw new Error(`region ${r.index} ends before it starts`);
    cursor = r.outerTo;
  }
  if (cursor !== length) throw new Error(`tiling stopped at ${cursor} of ${length}`);
}

/**
 * `regionAt` agrees with what `regions()` says is at that offset.
 *
 * Stated as two one-directional laws rather than as an index lookup, because
 * two answers are legitimate at a boundary and pinning either would be
 * inventing a contract §14 does not have:
 *
 * * A **zero-width outer span** — what an empty trailing region looks like —
 *   contains no offset under half-open containment, while a binary search over
 *   region starts may still land on it.
 * * The **end of the last region** is the cursor at end of file, which
 *   `segmenter.ts` answers with that region rather than with `null`. It is also
 *   where a position lands while the segmentation is one edit stale.
 */
function checkRegionAt(seg: StratumSegmenter, regions: readonly RegionView[], pos: number): void {
  const last = regions[regions.length - 1];
  const atEnd = last !== undefined && pos === last.outerTo;
  const hit = seg.regionAt(pos);
  if (hit === null) {
    const covered = regions.find((r) => pos >= r.outerFrom && pos < r.outerTo);
    if (covered) {
      throw new Error(
        `regionAt(${pos}) is null but region ${covered.index} covers ${covered.outerFrom}..${covered.outerTo}`,
      );
    }
    if (atEnd && regions.length > 0) {
      throw new Error(`regionAt(${pos}) is null at the end of the last region`);
    }
    return;
  }
  const contains = pos >= hit.outerFrom && pos < hit.outerTo;
  const empty = hit.outerFrom === hit.outerTo && pos === hit.outerFrom;
  if (!contains && !empty && !(atEnd && hit.index === last?.index)) {
    throw new Error(
      `regionAt(${pos}) returned region ${hit.index} at ${hit.outerFrom}..${hit.outerTo}`,
    );
  }
  if (JSON.stringify(hit) !== JSON.stringify(regions[hit.index])) {
    throw new Error(`regionAt(${pos}) returned something regions()[${hit.index}] disagrees with`);
  }
}

/** Every comparable field of a region, hashes included. */
function row(r: RegionView): unknown[] {
  return [
    r.index,
    r.from,
    r.to,
    r.outerFrom,
    r.outerTo,
    r.kindCode,
    r.entryDelimiter,
    r.exitDelimiter,
    r.headLine,
    r.lastLine,
    r.executable,
    r.isEstimation,
    r.hasMacroInHead,
    r.sectionHead,
    r.hashKey,
    r.hashOrdinal,
  ];
}

// ---------------------------------------------------------------------------
// The generator.
// ---------------------------------------------------------------------------

/**
 * One session, as data.
 *
 * Generated against a model of the document rather than against a live
 * segmenter, so the program does not depend on what any backend answered — the
 * same bytes reach every backend, which is what makes the comparison mean
 * anything.
 */
function generateSession(seed: number): Op[] {
  const rand = mulberry32(seed);
  const ops: Op[] = [];
  let model = randomDoc(rand);
  ops.push({ kind: "open", text: model });

  // One session in three pastes a document big enough to move the wasm heap.
  // `reserve` may grow linear memory, which detaches every existing typed-array
  // view; only the real module can fail that way, and only under a paste.
  const growAt = seed % 3 === 0 ? 2 + Math.floor(rand() * (STEPS - 4)) : -1;

  for (let step = 1; step < STEPS; step++) {
    if (step === growAt) {
      const at = snap(model, Math.floor(rand() * (model.length + 1)));
      const changes = [{ from: at, to: at, insert: bulkText() }];
      ops.push({ kind: "edit", changes });
      model = applyChangesToText(model, changes);
      continue;
    }
    const roll = rand();
    if (roll < 0.3) {
      const changes = randomChanges(rand, model);
      if (changes.length > 0) {
        ops.push({ kind: "edit", changes });
        model = applyChangesToText(model, changes);
        continue;
      }
      ops.push({ kind: "resegment" });
      continue;
    }
    if (roll < 0.5) {
      ops.push({ kind: "resegment" });
      continue;
    }
    if (roll < 0.6) {
      model = randomDoc(rand);
      ops.push({ kind: "open", text: model });
      continue;
    }
    if (roll < 0.72) {
      const varCount = Math.floor(rand() * 64);
      ops.push({
        kind: "env",
        spec: {
          generation: step,
          varCount,
          total: varCount + Math.floor(rand() * 4096),
          truncated: rand() < 0.5,
        },
      });
      continue;
    }
    if (roll < 0.86) {
      ops.push({ kind: "complete", pos: Math.floor(rand() * (model.length + 1)) });
      continue;
    }
    const from = Math.floor(rand() * (model.length + 1));
    // Past the end on purpose: the editor asks for the viewport, and the
    // viewport outlives a deletion by one frame.
    const to = from + Math.floor(rand() * (model.length + 64));
    ops.push({ kind: "tokens", from, to });
  }
  return ops;
}

function randomDoc(rand: () => number): string {
  let out = "";
  const n = Math.floor(rand() * 8);
  for (let i = 0; i < n; i++) out += pick(rand, FRAGMENTS);
  return out;
}

function bulkText(): string {
  let out = "";
  for (let i = 0; i < BULK_LINES; i++) out += `summarize v${i}, detail\n`;
  return out;
}

/**
 * One transaction: up to three non-overlapping spans in ascending
 * pre-transaction coordinates, which is what `ChangeSet.iterChanges` reports.
 */
function randomChanges(rand: () => number, text: string): DocChange[] {
  const wanted = 1 + Math.floor(rand() * 3);
  const cuts: number[] = [];
  for (let i = 0; i < wanted * 2; i++) cuts.push(Math.floor(rand() * (text.length + 1)));
  cuts.sort((a, b) => a - b);

  const changes: DocChange[] = [];
  for (let i = 0; i < wanted; i++) {
    const from = snap(text, cuts[i * 2] ?? 0);
    const to = snap(text, cuts[i * 2 + 1] ?? 0);
    if (to < from) continue;
    const previous = changes[changes.length - 1];
    if (previous && from < previous.to) continue;
    const insert = rand() < 0.3 ? "" : pick(rand, INSERTS);
    // A change that neither deletes nor inserts is not a change; it would make
    // the generation law ambiguous for no coverage in return.
    if (from === to && insert === "") continue;
    changes.push({ from, to, insert });
  }
  return changes;
}

/**
 * Move an offset off the middle of a surrogate pair.
 *
 * CodeMirror never reports a change boundary inside one, so a segmenter that
 * mangles a split pair is not wrong — it is being asked something the editor
 * cannot ask. Snapping keeps the generator inside the contract instead of
 * failing backends over an input they will never see.
 */
function snap(text: string, at: number): number {
  const i = Math.max(0, Math.min(text.length, at));
  const unit = text.charCodeAt(i);
  return i > 0 && unit >= 0xdc00 && unit <= 0xdfff ? i - 1 : i;
}

/** The model side of one op. */
function applyToModel(model: string, op: Op): string {
  if (op.kind === "open") return op.text;
  if (op.kind === "edit") return applyChangesToText(model, op.changes);
  return model;
}

/**
 * Apply a transaction to a plain string.
 *
 * Deliberately last-to-first: `segmenter.ts` walks the changes forwards and
 * carries a running delta, so working backwards — where no earlier splice can
 * have moved a later span — computes the same answer by different arithmetic.
 * An oracle that repeats the implementation's own reasoning is not an oracle.
 */
function applyChangesToText(text: string, changes: readonly DocChange[]): string {
  let out = text;
  for (let i = changes.length - 1; i >= 0; i--) {
    const c = changes[i];
    if (!c) continue;
    out = out.slice(0, c.from) + c.insert + out.slice(c.to);
  }
  return out;
}

/** Up to eight offsets spread across `[0, length)`, plus the ends. */
function spread(length: number): number[] {
  if (length <= 0) return [];
  if (length <= 8) return Array.from({ length }, (_, i) => i);
  const step = Math.floor(length / 8);
  const out: number[] = [];
  for (let i = 0; i < 8; i++) out.push(i * step);
  out.push(length - 1);
  return out;
}

function pick<T>(rand: () => number, pool: readonly T[]): T {
  const value = pool[Math.floor(rand() * pool.length)];
  if (value === undefined) throw new Error("pool is empty");
  return value;
}

/** mulberry32: one line, decorrelates adjacent seeds, and it is reproducible. */
function mulberry32(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function isBulk(op: Op): boolean {
  return op.kind === "edit" && op.changes.some((c) => c.insert.length > 4096 && c.from === c.to);
}

function describe(op: Op): string {
  switch (op.kind) {
    case "open":
      return `open ${op.text.length} units`;
    case "edit":
      return `edit ${op.changes.map((c) => `${c.from}..${c.to}+${c.insert.length}`).join(",")}`;
    case "resegment":
      return "resegment";
    case "env":
      return `env gen ${op.spec.generation}`;
    case "complete":
      return `complete(${op.pos})`;
    case "tokens":
      return `tokens(${op.from},${op.to})`;
  }
}

function brief(text: string): string {
  return JSON.stringify(text.length > 72 ? `${text.slice(0, 72)}…` : text);
}
