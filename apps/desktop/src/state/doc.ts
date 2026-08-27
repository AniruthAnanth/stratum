/**
 * Open documents — 06 §13.2, ARCHITECTURE C20.
 *
 * A webview holds no authoritative state except the text of documents it owns.
 * This store is therefore two things and nothing else: the per-document record
 * the shell needs (path, version, EOL/BOM policy, ownership), and the **display
 * status** rule — `displayed = worseOf(local, kernel)` — which is the one piece
 * of block-status logic that is genuinely the frontend's, because only the
 * frontend knows whether the text has changed since the kernel last saw it.
 *
 * The document TEXT lives in W13's CodeMirror `EditorState`, not here. Two
 * copies of a 2 MB buffer that must agree is a bug waiting for a race.
 */

import { createStore, produce } from "solid-js/store";
import type { CodeHash, DocumentId, HasBlockState } from "../ipc/hand";
import { worseOf } from "../ipc/hand";

export interface DocRecord {
  readonly doc: DocumentId;
  readonly path: string | undefined;
  version: number;
  /** A24: the byte-fidelity policy recorded on open and reproduced on save. */
  eol: "lf" | "crlf";
  bom: boolean;
  /** The window label that owns this document. §13.2: exactly one. */
  ownerLabel: string;
  dirty: boolean;
}

interface DocState {
  docs: Record<string, DocRecord>;
  active: DocumentId | undefined;
}

const [docs, setDocs] = createStore<DocState>({ docs: {}, active: undefined });

export const documents = docs;

export function openDocument(record: DocRecord): void {
  setDocs(
    produce((s) => {
      s.docs[String(record.doc)] = record;
      s.active ??= record.doc;
    }),
  );
}

export function closeDocument(doc: DocumentId): void {
  setDocs(
    produce((s) => {
      delete s.docs[String(doc)];
      if (s.active === doc) {
        const first = Object.values(s.docs)[0];
        s.active = first?.doc;
      }
    }),
  );
}

export function setActiveDocument(doc: DocumentId): void {
  setDocs("active", doc);
}

export function activeDocument(): DocRecord | undefined {
  const id = docs.active;
  return id === undefined ? undefined : docs.docs[String(id)];
}

export function markDirty(doc: DocumentId, dirty: boolean): void {
  if (docs.docs[String(doc)] === undefined) return;
  setDocs("docs", String(doc), "dirty", dirty);
}

export function bumpVersion(doc: DocumentId, version: number): void {
  if (docs.docs[String(doc)] === undefined) return;
  setDocs("docs", String(doc), "version", version);
}

// ---------------------------------------------------------------------------
// Block status — the display rule (ARCHITECTURE C20)
// ---------------------------------------------------------------------------

/** What the kernel last told us about a block. */
const kernelStatus = new Map<string, HasBlockState>();
/** The code hash recorded on the block's last execution, per block key. */
const executedHash = new Map<string, CodeHash>();

const key = (doc: DocumentId, block: number): string => `${doc}:${block}`;

export function setKernelStatus(doc: DocumentId, block: number, status: HasBlockState): void {
  kernelStatus.set(key(doc, block), status);
}

export function setExecutedHash(doc: DocumentId, block: number, hash: CodeHash): void {
  executedHash.set(key(doc, block), hash);
}

const LOCAL_STALE: HasBlockState = { state: "stale" };

/**
 * `displayed = worseOf(local, kernel)`.
 *
 * `local` is `Stale` iff the locally computed `CodeHash` differs from the one
 * recorded on the last execution, and no opinion otherwise. The local check may
 * only ever move a block TOWARD more stale — which `worseOf` guarantees, since
 * the only local verdict this function can produce is `stale` and `worseOf`
 * takes the lower rank. It never fabricates `current`.
 */
export function displayedStatus(
  doc: DocumentId,
  block: number,
  localHash: CodeHash | undefined,
): HasBlockState {
  const kernel = kernelStatus.get(key(doc, block)) ?? { state: "never_run" };
  if (localHash === undefined) return kernel;
  const executed = executedHash.get(key(doc, block));
  if (executed === undefined || executed === localHash) return kernel;
  return worseOf(LOCAL_STALE, kernel);
}

/** Test seam. */
export function resetDocState(): void {
  kernelStatus.clear();
  executedHash.clear();
  setDocs({ docs: {}, active: undefined });
}
