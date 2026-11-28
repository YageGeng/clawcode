// How the trajectory words its records: the type label, the one-line content, durations and token counts.
import { t } from "nsh/i18n";

import type { TrajectoryRecord, TrajectoryRow } from "../client/trajectory";
import type { Tone } from "../ui/tokens";

/** A duration with sub-second precision: "120ms", "0.8s", "12.4s", then minutes and seconds. */
export function formatMs(milliseconds: number): string {
  const ms = Math.max(0, Math.round(milliseconds));
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return t("time.seconds", { s: (ms / 1000).toFixed(1) });
  const total = Math.round(ms / 1000);
  return t("time.minutesSeconds", { m: Math.floor(total / 60), s: total % 60 });
}

/** A token count in at most four characters: "980", "12.3k", "1.2M". */
export function formatTokens(count: number): string {
  if (count < 1000) return String(count);
  if (count < 1_000_000) return `${(count / 1000).toFixed(count < 10_000 ? 1 : 0)}k`;
  return `${(count / 1_000_000).toFixed(1)}M`;
}

/** One line of `text`, its first non-empty line. */
function firstLine(text: string): string {
  return text.split("\n").find((line) => line.trim() !== "")?.trim() ?? "";
}

/** A tool call's arguments in a few words: a known path or command field, else compact JSON. */
function argumentSummary(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value !== "object") return String(value);
  const record = value as Record<string, unknown>;
  for (const key of ["cmd", "command", "path", "file_path", "pattern", "query", "url"]) {
    if (typeof record[key] === "string") return firstLine(record[key] as string);
  }
  return JSON.stringify(value);
}

/** How the record table and details word a record. */
export const RecordWords = {
  /** The type column of a record. */
  type(record: TrajectoryRecord): string {
    return t(`trajectory.types.${record.kind.type}`);
  },

  /** The content column of a record, in one line. */
  content(record: TrajectoryRecord): string {
    const kind = record.kind;
    switch (kind.type) {
      case "user": {
        const tags = [
          kind.origin !== "input" ? t(`trajectory.${kind.origin === "follow_up" ? "follow_up" : kind.origin === "skill" ? "skill" : "steering"}`) : "",
          kind.imageCount ? t("trajectory.images", { count: kind.imageCount }) : "",
          kind.fileCount ? t("trajectory.files", { count: kind.fileCount }) : "",
        ].filter(Boolean);
        return [firstLine(kind.text), ...tags.map((tag) => `[${tag}]`)].filter(Boolean).join(" ");
      }
      case "command":
        return firstLine(kind.text);
      case "request":
        return t("trajectory.request", { n: kind.index, model: kind.options.modelId });
      case "assistant": {
        const body = firstLine(kind.text) || (kind.toolCallIds.length ? t("trajectory.toolsOnly") : firstLine(kind.reasoning));
        const state = kind.error !== undefined ? ` [${t("trajectory.failed")}: ${firstLine(kind.error)}]` : kind.cancelled ? ` [${t("trajectory.cancelled")}]` : "";
        return `${body}${state}`;
      }
      case "tool": {
        const result = kind.cancelled ? t("trajectory.cancelled") : kind.resultText === undefined ? "…" : firstLine(kind.resultText);
        return `${kind.name} ${argumentSummary(kind.arguments)} → ${result}`;
      }
      case "compaction":
        return [firstLine(kind.summary), kind.tokensBefore ? `(${t("trajectory.compactedFrom", { tokens: formatTokens(kind.tokensBefore) })})` : ""].filter(Boolean).join(" ");
      case "branch_summary":
        return firstLine(kind.summary);
      case "setting":
        return t(`trajectory.setting.${kind.change === "model" || kind.change === "thinking_level" ? kind.change : "active_tools"}`, { detail: kind.detail });
      case "extension":
        return `${kind.extensionId}/${kind.customType} ${firstLine(kind.text)}`;
    }
  },

  /** The system row's content: the initial prompt, a changed prompt, and tools added or removed. */
  system(record: TrajectoryRecord): string {
    if (record.kind.type !== "request") return "";
    const kind = record.kind;
    return [
      kind.first ? t("trajectory.initialPrompt") : kind.promptChanged ? t("trajectory.promptChanged") : "",
      kind.toolsAdded.length ? `${t("trajectory.toolsAdded", { n: kind.toolsAdded.length })} (${kind.toolsAdded.join(", ")})` : "",
      kind.toolsRemoved.length ? `${t("trajectory.toolsRemoved", { n: kind.toolsRemoved.length })} (${kind.toolsRemoved.join(", ")})` : "",
    ].filter(Boolean).join(" · ");
  },

  /** Whether a record ended badly: a failed or cancelled request or tool. */
  failed(record: TrajectoryRecord): boolean {
    const kind = record.kind;
    return (kind.type === "assistant" && (kind.error !== undefined || kind.cancelled)) || (kind.type === "tool" && (kind.isError || kind.cancelled));
  },

  /**
   * The status of a record: running while a tool waits for its result (`pending`, as the log judges it), no result
   * when it never got one, failed, cancelled, or completed.
   */
  status(record: TrajectoryRecord, pending: boolean): { label: string; tone: Tone } {
    const kind = record.kind;
    if (kind.type === "tool" && kind.resultText === undefined && !kind.cancelled) {
      return pending ? { label: t("record.status.running"), tone: "info" } : { label: t("record.status.noResult"), tone: "warning" };
    }
    if ((kind.type === "assistant" && kind.cancelled) || (kind.type === "tool" && kind.cancelled)) return { label: t("record.status.cancelled"), tone: "neutral" };
    if (RecordWords.failed(record)) return { label: t("record.status.failed"), tone: "danger" };
    return { label: t("record.status.done"), tone: "success" };
  },

  /** The record a row stands for, if any: selecting it shows that record's details. */
  ofRow(row: TrajectoryRow): TrajectoryRecord | undefined {
    return row.type === "record" || row.type === "request" || row.type === "system" ? row.record : undefined;
  },
} as const;
