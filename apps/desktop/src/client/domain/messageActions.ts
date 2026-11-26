import type { EntryId, ImageMimeType, MessageId, ToolCallId } from "../acp/protocol";
import type { PromptImage, PromptInput, SessionTree, SessionTreeEntry, TranscriptEntry } from "./model";
import { TranscriptRowKeys } from "./model";

export type BranchEditPlan = Readonly<{
  branchEntryId: EntryId | null;
  draft: PromptInput;
}>;

const IMAGE_EXTENSIONS: Readonly<Record<ImageMimeType, string>> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/gif": "gif",
  "image/webp": "webp"
};

function record(value: unknown): Readonly<Record<string, unknown>> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Readonly<Record<string, unknown>>
    : undefined;
}

/** Projects durable Tree entries onto the transcript rows that display them. */
const TreeEntryRows = {
  /** Reads every durable message identifier one Message Tree entry carries. */
  messageIds(entry: SessionTreeEntry): readonly MessageId[] {
    if (entry.kind !== "message") return [];
    const primary = entry.payload.message_id;
    const fallback = record(entry.payload.identity)?.message_id;
    const fallbackId = typeof fallback === "string" ? fallback : undefined;
    if (typeof primary !== "string") return fallbackId === undefined ? [] : [fallbackId as MessageId];
    const primaryId = primary as MessageId;
    return fallbackId === undefined || fallbackId === primary ? [primaryId] : [primaryId, fallbackId as MessageId];
  },

  /**
   * Picks the row an Assistant displays through. Mirrors MessageView: blank text
   * and reasoning render nothing, so unless the attempt failed it shows only as
   * its Tool cards, and a message with neither renders no row at all.
   */
  assistantRowKey(content: Readonly<Record<string, unknown>>, messageId: MessageId | undefined): string | undefined {
    const blocks = Array.isArray(content.blocks) ? content.blocks.map(record) : [];
    const failed = typeof record(content.metadata)?.error === "string";
    const hasVisibleBlock = blocks.some((block) => {
      if (block?.type === "tool_call") return false;
      return !((block?.type === "text" || block?.type === "reasoning") && block.text === "");
    });
    if (failed || hasVisibleBlock) return messageId === undefined ? undefined : TranscriptRowKeys.message(messageId);
    const toolCallId = blocks.find((block) => block?.type === "tool_call")?.tool_call_id;
    return typeof toolCallId === "string" ? TranscriptRowKeys.tool(toolCallId as ToolCallId) : undefined;
  },

  /** Maps one durable Tree entry to the key of the row that displays it, if any. */
  rowKey(entry: SessionTreeEntry): string | undefined {
    if (entry.kind === "compaction") return TranscriptRowKeys.compaction(entry.entryId);
    if (entry.kind !== "message") return undefined;
    const content = record(entry.payload.content);
    const messageId = TreeEntryRows.messageIds(entry)[0];
    if (content?.type === "tool_result") {
      return typeof content.tool_call_id === "string" ? TranscriptRowKeys.tool(content.tool_call_id as ToolCallId) : undefined;
    }
    if (content?.type === "assistant") return TreeEntryRows.assistantRowKey(content, messageId);
    if (messageId === undefined) return undefined;
    // A user `!` shell renders as an Execute Tool named after its message id.
    if (content?.type === "bash_execution") return TranscriptRowKeys.tool(String(messageId) as ToolCallId);
    if (content?.type === "extension") return TranscriptRowKeys.extension(messageId);
    return TranscriptRowKeys.message(messageId);
  }
} as const;

export const MessageActionModel = {
  /** Resolves one transcript message to its durable tree entry. */
  entry(tree: SessionTree | undefined, messageId: MessageId): SessionTreeEntry | undefined {
    return tree?.entries.find((entry) => {
      if (entry.kind !== "message") return false;
      const identity = record(entry.payload.identity);
      return entry.payload.message_id === messageId || identity?.message_id === messageId;
    });
  },

  /** Builds a durable messageId to Tree entry index for O(1) transcript lookup. */
  index(tree: SessionTree | undefined): ReadonlyMap<MessageId, SessionTreeEntry> {
    const index = new Map<MessageId, SessionTreeEntry>();
    for (const entry of tree?.entries ?? []) {
      for (const messageId of TreeEntryRows.messageIds(entry)) {
        if (!index.has(messageId)) index.set(messageId, entry);
      }
    }
    return index;
  },

  /**
   * Maps each Tree node on the active branch to the row displaying it. Walking
   * from the leaf matters because a node on another branch can share a provider
   * tool call id with a row this transcript does render.
   *
   * A message that renders no row of its own, such as a hidden `todo_write`
   * call or its result, maps to the nearest row above it instead. That row is
   * in the same Run or an earlier one, never a later prompt.
   */
  revealableRows(tree: SessionTree | undefined, transcript: readonly TranscriptEntry[]): ReadonlyMap<EntryId, string> {
    const rows = new Map<EntryId, string>();
    if (tree === undefined) return rows;
    const rendered = new Set(transcript.map((row) => TranscriptRowKeys.of(row)));
    const entries = new Map(tree.entries.map((entry) => [entry.entryId, entry]));
    const visited = new Set<EntryId>();
    // The walk goes leaf to root, so hidden messages wait for the next rendered ancestor.
    let hidden: EntryId[] = [];
    let entry = tree.leafId === null || tree.leafId === undefined ? undefined : entries.get(tree.leafId);
    while (entry !== undefined && !visited.has(entry.entryId)) {
      visited.add(entry.entryId);
      const rowKey = TreeEntryRows.rowKey(entry);
      if (rowKey !== undefined && rendered.has(rowKey)) {
        rows.set(entry.entryId, rowKey);
        for (const entryId of hidden) rows.set(entryId, rowKey);
        hidden = [];
      } else if (entry.kind === "message") {
        hidden.push(entry.entryId);
      }
      entry = entry.parentId === null || entry.parentId === undefined ? undefined : entries.get(entry.parentId);
    }
    return rows;
  },

  /** Builds a same-Session Branch plan that reopens one user prompt for editing. */
  editPlan(entry: SessionTreeEntry): BranchEditPlan | undefined {
    if (entry.kind !== "message") return undefined;
    const content = record(entry.payload.content);
    if (content === undefined) return undefined;

    let draft: PromptInput;
    if (content.type === "expanded_user") {
      const expansion = record(content.expansion);
      const invocation = record(expansion?.invocation);
      if (typeof invocation?.original !== "string") return undefined;
      draft = { text: invocation.original, resources: [], images: [] };
    } else if (content.type === "user" && Array.isArray(content.blocks)) {
      let text = "";
      const resources: PromptInput["resources"][number][] = [];
      const images: PromptImage[] = [];
      for (const [index, value] of content.blocks.entries()) {
        const block = record(value);
        if (block?.type === "text" && typeof block.text === "string") {
          text += block.text;
          continue;
        }
        if (block?.type === "resource_link" && typeof block.name === "string" && typeof block.uri === "string") {
          resources.push({ name: block.name, uri: block.uri });
          continue;
        }
        const mimeType = block?.mime_type;
        if (
          block?.type === "image"
          && typeof block.data === "string"
          && (mimeType === "image/png" || mimeType === "image/jpeg" || mimeType === "image/gif" || mimeType === "image/webp")
        ) {
          const padding = block.data.endsWith("==") ? 2 : block.data.endsWith("=") ? 1 : 0;
          images.push({
            id: `${entry.entryId}:image:${index}`,
            name: `历史图片 ${images.length + 1}.${IMAGE_EXTENSIONS[mimeType]}`,
            size: Math.max(0, Math.floor(block.data.length * 3 / 4) - padding),
            data: block.data,
            mimeType
          });
          continue;
        }
        return undefined;
      }
      draft = { text, resources, images };
    } else {
      return undefined;
    }

    return { branchEntryId: entry.parentId ?? null, draft };
  }
} as const;
