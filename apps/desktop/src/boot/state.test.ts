/**
 * The client stores.
 *
 * These test `src/state/*.ts` but live under `src/boot/` because
 * `docs/ownership.toml` gives W12 those seven files as EXACT paths — there is no
 * glob under `src/state/`, so a sibling `*.test.ts` would be a file no unit owns
 * and `xtask ownership` would fail on it. Escalated in the unit's return; the
 * boot directory is where these stores are composed, so it is the least
 * misleading home available under the manifest as written.
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { type DocumentId, type ResultId, codeHash } from "../ipc/hand";
import {
  closeDocument,
  displayedStatus,
  documents,
  markDirty,
  openDocument,
  resetDocState,
  setExecutedHash,
  setKernelStatus,
} from "../state/doc";
import {
  appendHistory,
  historyState,
  nextCommand,
  previousCommand,
  resetCursor,
  resetHistoryState,
  setHistoryFilter,
  visibleHistory,
} from "../state/history";
import {
  clearBlockResults,
  clearResults,
  currentResultForBlock,
  latestResult,
  recordResult,
  rekeyBlock,
  resetResultState,
  resultsForBlock,
} from "../state/results";
import { acceptGeneration, onResync, resetSessionState } from "../state/session";
import {
  CODE_SIZE_MAX,
  CODE_SIZE_MIN,
  cycleInlineResults,
  effectiveInlineResults,
  effectiveTheme,
  resetSettings,
  updateSettings,
  userSettings,
} from "../state/settings";

const HASH_A = codeHash("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
const HASH_B = codeHash("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
const DOC = 1 as DocumentId;

beforeEach(() => {
  resetDocState();
  resetHistoryState();
  resetResultState();
  resetSettings();
  resetSessionState();
});

afterEach(() => {
  resetDocState();
  resetHistoryState();
  resetResultState();
  resetSettings();
  resetSessionState();
});

describe("doc — the display rule (ARCHITECTURE C20)", () => {
  it("shows the kernel's verdict when the text has not moved", () => {
    setKernelStatus(DOC, 1, { state: "current" });
    setExecutedHash(DOC, 1, HASH_A);
    expect(displayedStatus(DOC, 1, HASH_A).state).toBe("current");
  });

  it("moves a block toward stale when the local hash differs", () => {
    setKernelStatus(DOC, 1, { state: "current" });
    setExecutedHash(DOC, 1, HASH_A);
    expect(displayedStatus(DOC, 1, HASH_B).state).toBe("stale");
  });

  it("never moves a block toward healthier", () => {
    // The local check may only ever move a block TOWARD more stale. A kernel
    // `Failed` must survive an edit; showing `stale` there would tell the user
    // the numbers are merely old when in fact the block errored.
    setKernelStatus(DOC, 1, { state: "failed" });
    setExecutedHash(DOC, 1, HASH_A);
    expect(displayedStatus(DOC, 1, HASH_B).state).toBe("failed");
  });

  it("lets a running block outrank a local edit", () => {
    setKernelStatus(DOC, 1, { state: "running" });
    setExecutedHash(DOC, 1, HASH_A);
    expect(displayedStatus(DOC, 1, HASH_B).state).toBe("running");
  });

  it("is never_run for a block the kernel has never seen", () => {
    expect(displayedStatus(DOC, 99, HASH_A).state).toBe("never_run");
  });

  it("tracks open documents and their byte-fidelity policy", () => {
    openDocument({
      doc: DOC,
      path: "/p/analysis.do",
      version: 1,
      eol: "crlf",
      bom: true,
      ownerLabel: "main",
      dirty: false,
    });
    expect(documents.active).toBe(DOC);
    expect(documents.docs["1"]?.eol).toBe("crlf");
    markDirty(DOC, true);
    expect(documents.docs["1"]?.dirty).toBe(true);
    closeDocument(DOC);
    expect(documents.active).toBeUndefined();
  });
});

describe("history — Stata's PgUp/PgDn, which §9.1 calls non-negotiable", () => {
  beforeEach(() => {
    appendHistory({ seq: 1, command: "sysuse auto", rc: 0, origin: "commandbar" });
    appendHistory({ seq: 2, command: "summarize price", rc: 0, origin: "commandbar" });
    appendHistory({ seq: 3, command: "regres price mpg", rc: 199, origin: "commandbar" });
  });

  it("PgUp walks backward from the newest", () => {
    expect(previousCommand("")).toBe("regres price mpg");
    expect(previousCommand("")).toBe("summarize price");
    expect(previousCommand("")).toBe("sysuse auto");
    expect(previousCommand("")).toBeUndefined();
  });

  it("PgDn walks forward and restores the draft at the end", () => {
    // The half-typed command the user abandoned to look at history has to come
    // back. Losing it is the small betrayal that makes people stop using PgUp.
    previousCommand("gen inc = ");
    previousCommand("gen inc = ");
    expect(nextCommand()).toBe("regres price mpg");
    expect(nextCommand()).toBe("gen inc = ");
    expect(nextCommand()).toBeUndefined();
  });

  it("resets the cursor when a new entry lands", () => {
    previousCommand("");
    appendHistory({ seq: 4, command: "list in 1/5", rc: 0, origin: "commandbar" });
    expect(previousCommand("")).toBe("list in 1/5");
  });

  it("keeps the non-zero _rc, because the pane colours on it", () => {
    expect(historyState.entries.at(-1)?.rc).toBe(199);
  });

  it("filters without disturbing the underlying entries", () => {
    setHistoryFilter("price");
    expect(visibleHistory().map((e) => e.seq)).toEqual([2, 3]);
    expect(historyState.entries).toHaveLength(3);
    setHistoryFilter("");
    expect(visibleHistory()).toHaveLength(3);
  });

  it("starts a fresh cursor from an explicit reset", () => {
    resetCursor("draft");
    expect(nextCommand()).toBeUndefined();
  });
});

describe("results", () => {
  const envelope = (id: number): { id: ResultId } => ({ id: id as ResultId });

  it("keeps every version a block produced, newest last", () => {
    recordResult(envelope(1), { hash: HASH_A, ordinal: 0 });
    recordResult(envelope(2), { hash: HASH_A, ordinal: 0 });
    expect(resultsForBlock(HASH_A, 0)).toEqual([1, 2]);
    expect(currentResultForBlock(HASH_A, 0)).toBe(2);
    expect(latestResult()).toBe(2);
  });

  it("follows a block whose ordinal moved but whose code did not", () => {
    // An edit ABOVE a block changes its ordinal without changing its hash, and
    // the card must follow the code rather than the position.
    recordResult(envelope(1), { hash: HASH_A, ordinal: 3 });
    rekeyBlock({ hash: HASH_A, ordinal: 3 }, { hash: HASH_A, ordinal: 5 });
    expect(resultsForBlock(HASH_A, 3)).toEqual([]);
    expect(resultsForBlock(HASH_A, 5)).toEqual([1]);
  });

  it("clears one block without touching the others", () => {
    recordResult(envelope(1), { hash: HASH_A, ordinal: 0 });
    recordResult(envelope(2), { hash: HASH_B, ordinal: 1 });
    clearBlockResults(HASH_A, 0);
    expect(resultsForBlock(HASH_A, 0)).toEqual([]);
    expect(resultsForBlock(HASH_B, 1)).toEqual([2]);
  });

  it("clears the window without pretending the archive is gone", () => {
    recordResult(envelope(1), { hash: HASH_A, ordinal: 0 });
    clearResults();
    expect(latestResult()).toBeUndefined();
    expect(resultsForBlock(HASH_A, 0)).toEqual([]);
  });
});

describe("settings", () => {
  it("leaves a layout default in force until the user chooses", () => {
    expect(effectiveInlineResults("always")).toBe("always");
    expect(effectiveTheme("dark")).toBe("dark");
    updateSettings({ inlineResults: "compact", theme: "light" });
    // And once chosen, the user's choice survives a layout switch — which is the
    // entire reason these are two fields and not one.
    expect(effectiveInlineResults("always")).toBe("compact");
    expect(effectiveTheme("dark")).toBe("light");
  });

  it("cycles the four inline modes in the order §8.1 lists them", () => {
    expect(cycleInlineResults("always")).toBe("editor-run");
    expect(cycleInlineResults("always")).toBe("compact");
    expect(cycleInlineResults("always")).toBe("off");
    expect(cycleInlineResults("always")).toBe("always");
  });

  it("clamps the editor size to 06 §14.3's 11-18", () => {
    updateSettings({ codeSizePx: 40 });
    expect(userSettings().codeSizePx).toBe(CODE_SIZE_MAX);
    updateSettings({ codeSizePx: 2 });
    expect(userSettings().codeSizePx).toBe(CODE_SIZE_MIN);
  });
});

describe("session — generation gaps", () => {
  it("accepts a contiguous stream and a repeat", () => {
    expect(acceptGeneration("vars", 1)).toBe(true);
    expect(acceptGeneration("vars", 2)).toBe(true);
    expect(acceptGeneration("vars", 2)).toBe(true);
  });

  it("reports a gap and names the stream that skipped", () => {
    // A store that has silently diverged shows numbers that are WRONG rather
    // than absent, and in a statistics product those are not the same failure.
    const seen: string[] = [];
    const stop = onResync((stream) => seen.push(stream));
    acceptGeneration("vars", 1);
    expect(acceptGeneration("vars", 5)).toBe(false);
    expect(seen).toEqual(["vars"]);
    stop();
  });

  it("keeps one cursor per stream", () => {
    acceptGeneration("vars", 7);
    expect(acceptGeneration("log", 1)).toBe(true);
  });
});
