/**
 * The Command window's acceptance bullets — plan W16, 06 §9.1, §10.
 *
 * Four of the section's bullets are decided here and each has a test whose name
 * is the bullet:
 *
 *  * PgUp/PgDn step through command history, **unfiltered**;
 *  * Tab completes a variable name; after a `"` it completes a filename;
 *  * a command submitted here lands in History with its `_rc` and can be
 *    promoted into the do-file (spec §10, §11);
 *  * `Mod+.` breaks.
 *
 * Counters, never durations (ADR-017). The one that carries the unfiltered rule
 * is `recallCounters.scanned`: on the default path it must equal `steps`,
 * because an unfiltered PgUp examines exactly one entry. A prefix-matching
 * implementation cannot make that equality hold, so the counter proves the
 * behaviour rather than sampling it.
 */

import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { appendHistory, historyState, resetHistoryState } from "../state/history";
import { setCandidateSource } from "./complete";
import {
  completeAt,
  completeCounters,
  longestCommonPrefix,
  resetCompleteCounters,
  tokenAtCaret,
} from "./complete";
import { functionKeyAction, resetFunctionKeys, setFunctionKey } from "./fkeys";
import { commandBar, resetCommandBarHandle, sendToCommand } from "./handle";
import { interruptCounters, recordedBreaks, requestBreak, resetInterruptState } from "./interrupt";
import { historyBlockText, resetPromoteCounters, setDoFileInserter } from "./promote";
import { addAsNewBlock, addToDoFile, promoteCounters, sendHistoryToDoFile } from "./promote";
import {
  recallCounters,
  recallNext,
  recallPrevious,
  resetRecall,
  resetRecallCounters,
  setHistoryPrefixMatch,
} from "./recall";
import {
  recordedSubmissions,
  resetSubmitState,
  setSubmitSink,
  submitCommand,
  submitCounters,
} from "./submit";
import { CommandBar } from "./view";

const roots: (() => void)[] = [];

function mountBar(): HTMLElement {
  const host = document.createElement("div");
  document.body.append(host);
  roots.push(render(() => <CommandBar />, host));
  return host;
}

/** A real keydown at the CodeMirror content, so the keymap decides, not us. */
function press(host: HTMLElement, key: string, mods: Partial<KeyboardEventInit> = {}): void {
  const content = host.querySelector(".cm-content");
  if (content === null) throw new Error("no CodeMirror content in the command bar");
  content.dispatchEvent(
    new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...mods }),
  );
}

function seed(commands: readonly (string | [string, number])[]): void {
  commands.forEach((entry, i) => {
    const [command, rc] = typeof entry === "string" ? [entry, 0] : entry;
    appendHistory({ seq: i + 1, command, rc, origin: "commandbar" });
  });
}

beforeEach(() => {
  resetHistoryState();
  resetSubmitState();
  resetRecall("");
  resetRecallCounters();
  resetCompleteCounters();
  resetPromoteCounters();
  resetInterruptState();
  resetFunctionKeys();
  setCandidateSource(null);
  setHistoryPrefixMatch(false);
});

afterEach(() => {
  while (roots.length > 0) roots.pop()?.();
  document.body.replaceChildren();
  resetCommandBarHandle();
  setDoFileInserter(null);
});

// ---------------------------------------------------------------------------
// PgUp/PgDn step through command history, unfiltered
// ---------------------------------------------------------------------------

describe("PgUp/PgDn step through command history, unfiltered (06 §9.1, [U] 10.5)", () => {
  test("PgUp from a half-typed line returns the newest command, not the newest match", () => {
    seed(["sysuse auto, clear", "summarize price", "list make in 1/5"]);

    // The user has typed `su`. Stata's PgUp is unfiltered, so it does NOT jump
    // to `summarize price`: it walks back one entry from the newest.
    expect(recallPrevious("su")).toBe("list make in 1/5");
    expect(recallPrevious("su")).toBe("summarize price");
    expect(recallPrevious("su")).toBe("sysuse auto, clear");
    expect(recallPrevious("su")).toBeUndefined();
  });

  test("the unfiltered path examines exactly one entry per press", () => {
    seed(["a", "b", "c", "d", "e"]);
    resetRecallCounters();
    recallPrevious("");
    recallPrevious("");
    expect(recallCounters.steps).toBe(2);
    // The counter, not the strings: a prefix-matching walk cannot hold this.
    expect(recallCounters.scanned).toBe(recallCounters.steps);
  });

  test("PgDn walks forward and gives the draft back at the end", () => {
    seed(["describe", "summarize"]);
    expect(recallPrevious("half-typed")).toBe("summarize");
    expect(recallPrevious("half-typed")).toBe("describe");
    expect(recallNext()).toBe("summarize");
    expect(recallNext()).toBe("half-typed");
  });

  test("prefix matching exists, is off by default, and is opt-in", () => {
    seed(["sysuse auto, clear", "summarize price", "list make"]);
    // Default: unfiltered.
    expect(recallPrevious("sum")).toBe("list make");

    resetRecall("");
    setHistoryPrefixMatch(true);
    expect(recallPrevious("sum")).toBe("summarize price");
    setHistoryPrefixMatch(false);
  });

  test("PageUp in the mounted bar loads the previous command", () => {
    seed(["summarize price"]);
    const host = mountBar();
    press(host, "PageUp");
    expect(commandBar().text()).toBe("summarize price");
  });
});

// ---------------------------------------------------------------------------
// Tab completes a variable name; after a `"` it completes a filename
// ---------------------------------------------------------------------------

describe("Tab completion ([U] 10.6, 06 §9.1)", () => {
  beforeEach(() => {
    setCandidateSource({
      variables: (prefix) =>
        ["price", "priceadj", "mpg", "make", "foreign"].filter((n) => n.startsWith(prefix)),
      files: (prefix) =>
        ["auto.dta", "auto2.dta", "survey.dta"].filter((n) => n.startsWith(prefix)),
    });
  });

  test("a unique match completes the whole name", () => {
    const outcome = completeAt("summarize mp", 12);
    expect(outcome?.insert).toBe("mpg");
    expect(outcome?.ambiguous).toBe(false);
  });

  test("several matches insert the longest common prefix and report the list", () => {
    const outcome = completeAt("summarize pri", 13);
    // "complete as much as it can" — not the first match.
    expect(outcome?.insert).toBe("price");
    expect(outcome?.ambiguous).toBe(true);
    expect(outcome?.matches).toEqual(["price", "priceadj"]);
    expect(longestCommonPrefix(["price", "priceadj"])).toBe("price");
  });

  test("after a double quote the target is a filename, not a variable", () => {
    const token = tokenAtCaret('use "auto', 9);
    expect(token?.target).toBe("file");
    // The completed span begins AFTER the quote, so the quote survives.
    expect(token?.prefix).toBe("auto");
    expect(completeAt('use "auto', 9)?.insert).toBe("auto");
    expect(completeAt('use "auto', 9)?.matches).toEqual(["auto.dta", "auto2.dta"]);
  });

  test("a closed quote is not an open one", () => {
    // `"auto.dta"` is finished; the caret is back in variable-name territory.
    const token = tokenAtCaret('use "auto.dta", cle', 19);
    expect(token?.target).toBe("variable");
  });

  test("completion never scans more than the environment offers", () => {
    resetCompleteCounters();
    completeAt("summarize pri", 13);
    expect(completeCounters.requests).toBe(1);
    expect(completeCounters.candidates).toBe(2);
    expect(completeCounters.inserts).toBe(1);
  });

  test("Tab in the mounted bar inserts the completion", () => {
    const host = mountBar();
    sendToCommand("summarize mp");
    press(host, "Tab");
    expect(commandBar().text()).toBe("summarize mpg");
  });
});

// ---------------------------------------------------------------------------
// A command run here appears in History, and can be promoted (spec §10, §11)
// ---------------------------------------------------------------------------

describe("submission (spec §10)", () => {
  test("a command lands in History with its _rc", async () => {
    setSubmitSink(({ text }) => ({ rc: text.includes("nosuchvar") ? 111 : 0 }));
    await submitCommand("summarize price");
    await submitCommand("summarize nosuchvar");

    expect(historyState.entries.map((e) => [e.command, e.rc])).toEqual([
      ["summarize price", 0],
      ["summarize nosuchvar", 111],
    ]);
    expect(submitCounters.submissions).toBe(2);
    expect(submitCounters.historyAppends).toBe(2);
  });

  test("a blank line is not a command", async () => {
    await submitCommand("   ");
    expect(historyState.entries).toHaveLength(0);
    expect(submitCounters.blanks).toBe(1);
    expect(recordedSubmissions()).toHaveLength(0);
  });

  test("`Add command to do-file` inserts at the caret of the active editor", () => {
    const inserted: string[] = [];
    setDoFileInserter({
      insertAtCaret(text) {
        inserted.push(text);
        promoteCounters.inserts += 1;
        return true;
      },
      insertBlock(text) {
        inserted.push(`block:${text}`);
        return true;
      },
    });

    expect(addToDoFile("summarize price")).toBe(true);
    expect(addAsNewBlock("regress price mpg")).toBe(true);
    expect(inserted).toEqual(["summarize price\n", "block:regress price mpg"]);
  });

  test("History's Send to do-file writes §11's commented block", () => {
    const inserted: string[] = [];
    setDoFileInserter({
      insertAtCaret(text) {
        inserted.push(text);
        return true;
      },
      insertBlock: () => true,
    });

    sendHistoryToDoFile(["use survey.dta, clear", "drop if missing(income)"], "2026-08-21 14:03");
    expect(inserted[0]).toBe(
      "// from History — 2026-08-21 14:03\nuse survey.dta, clear\ndrop if missing(income)\n",
    );
    // The checkbox is off by default, so the commands go in live.
    expect(historyBlockText(["list"], "now", true)).toBe("// from History — now\n// list\n");
  });
});

// ---------------------------------------------------------------------------
// Mod+. breaks
// ---------------------------------------------------------------------------

describe("Break (06 §9.1)", () => {
  test("Mod+. requests an interrupt", () => {
    const host = mountBar();
    // CodeMirror resolves `Mod` per platform — Cmd on macOS, Ctrl elsewhere —
    // and jsdom is not macOS, so the Ctrl form is the one that fires here. The
    // binding is `Mod-.` in exactly one place (`view.tsx`), which is what makes
    // this a platform question rather than a second binding to keep in sync.
    press(host, ".", { ctrlKey: true });
    expect(interruptCounters.breaks).toBe(1);
    expect(recordedBreaks().every((b) => b.level === "interrupt")).toBe(true);
  });

  test("the second break escalates only when the caller asks", () => {
    requestBreak("abort");
    expect(recordedBreaks().at(-1)?.level).toBe("abort");
  });
});

// ---------------------------------------------------------------------------
// Function keys ([U] 10.2)
// ---------------------------------------------------------------------------

describe("function keys ([U] 10.2)", () => {
  test("a trailing semicolon is an implied Enter", () => {
    expect(functionKeyAction(2)).toEqual({ insert: "describe", submit: true });
    expect(functionKeyAction(7)).toEqual({ insert: "save ", submit: false });
  });

  test("an unbound key falls through rather than being swallowed", () => {
    expect(functionKeyAction(5)).toBeUndefined();
    setFunctionKey(5, "list ");
    expect(functionKeyAction(5)).toEqual({ insert: "list ", submit: false });
  });
});
