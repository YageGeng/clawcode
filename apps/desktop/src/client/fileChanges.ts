import { SnapshotDiff } from "clawcode-backend";

import { AcpProtocol } from "./acp/protocol";
import type { RecordedDiff, RecordedFileChange } from "./domain/model";

/** Reads the current single-file patch and legacy snapshot representations of an ACP diff item. */
function readDiff(item: unknown): { path: string; diff: RecordedDiff } | undefined {
  const record = AcpProtocol.record(item);
  if (record?.type !== "diff") return;
  if (typeof record.path === "string" && typeof record.newText === "string") {
    return { path: record.path, diff: { oldText: typeof record.oldText === "string" ? record.oldText : "", newText: record.newText } };
  }
  const patch = AcpProtocol.record(record.patch);
  const change = Array.isArray(record.changes) && record.changes.length === 1 ? AcpProtocol.record(record.changes[0]) : undefined;
  if (patch?.format === "git_patch" && typeof patch.text === "string" && typeof change?.path === "string") {
    return { path: change.path, diff: { patch: patch.text } };
  }
}

/** Compares wire payloads before doing any line diff or hunk scan. */
function sameDiff(left: RecordedDiff, right: RecordedDiff): boolean {
  return "patch" in left ? "patch" in right && left.patch === right.patch
    : "oldText" in right && left.oldText === right.oldText && left.newText === right.newText;
}

/** Counts Git hunk bodies or asks the native viewer's line-diff engine to compare two snapshots. */
function lineCounts(diff: RecordedDiff): { added: number; removed: number } {
  if ("oldText" in diff) return SnapshotDiff.counts(diff.oldText, diff.newText);
  let added = 0, removed = 0, inHunk = false;
  for (const line of diff.patch.split("\n")) {
    if (line.startsWith("diff --git ")) inHunk = false;
    else if (line.startsWith("@@ ")) inHunk = true;
    else if (inHunk && line.startsWith("+")) added++;
    else if (inHunk && line.startsWith("-")) removed++;
  }
  return { added, removed };
}

/** Reconciles one tool's authoritative content, retaining edit identities and already computed statistics. */
export function fileChanges(toolId: string, content: readonly unknown[], previous: readonly RecordedFileChange[] = []): readonly RecordedFileChange[] {
  const incoming = content.flatMap((item) => readDiff(item) ?? []);
  if (previous.length === incoming.length && previous.every((edit, i) =>
    edit.path === incoming[i]!.path && sameDiff(edit.diff, incoming[i]!.diff))) return previous;

  // ACP supplies no per-block ID. Reserve unchanged siblings before assigning a correction an old slot.
  const remaining = new Map<string, Set<RecordedFileChange>>();
  for (const edit of previous) {
    const siblings = remaining.get(edit.path);
    if (siblings) siblings.add(edit);
    else remaining.set(edit.path, new Set([edit]));
  }
  const matches = incoming.map(({ path, diff }) => {
    const siblings = remaining.get(path);
    if (siblings) {
      for (const edit of siblings) {
        if (sameDiff(edit.diff, diff)) {
          siblings.delete(edit);
          return edit;
        }
      }
    }
  });
  const usedIds = new Set(previous.map((edit) => edit.id));
  const nextSlot = new Map<string, number>();
  return incoming.map(({ path, diff }, index) => {
    const match = matches[index];
    if (match) return match;
    const siblings = remaining.get(path);
    const replaced = siblings?.values().next().value;
    const counts = lineCounts(diff);
    if (siblings && replaced) {
      siblings.delete(replaced);
      return { id: replaced.id, path, diff, ...counts };
    }
    let slot = nextSlot.get(path) ?? 0;
    let id: string;
    do { id = JSON.stringify([toolId, path, slot++]); } while (usedIds.has(id));
    nextSlot.set(path, slot);
    return { id, path, diff, ...counts };
  });
}
