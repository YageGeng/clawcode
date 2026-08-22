import type { EntryId, ImageMimeType, MessageId } from "../acp/protocol";
import type { PromptImage, PromptInput, SessionTree, SessionTreeEntry } from "./model";

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
    const entries = tree?.entries ?? [];
    for (const entry of entries) {
      if (entry.kind !== "message") continue;
      const identity = record(entry.payload.identity);
      const primaryMessageId = entry.payload.message_id;
      if (typeof primaryMessageId === "string" && !index.has(primaryMessageId as MessageId)) {
        index.set(primaryMessageId as MessageId, entry);
      }
      const identityMessageId = identity?.message_id;
      if (typeof identityMessageId === "string" && !index.has(identityMessageId as MessageId)) {
        index.set(identityMessageId as MessageId, entry);
      }
    }
    return index;
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
