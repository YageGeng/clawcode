/** An image selected on the controller. Only its format and base64 bytes cross the ACP connection. */
export type PromptImage = Readonly<{ id: string; name: string; mimeType: string; data: string; size: number }>;

/** The complete local draft to restore if a prompt fails, including an image-only submission. */
export type PromptDraft = Readonly<{ text: string; images: readonly PromptImage[] }>;

/** Builds ACP v2 prompt blocks in attachment order without disclosing a controller path or file name. */
export function promptBlocks({ text, images }: PromptDraft) {
  return [
    ...(text.length > 0 || !images.length ? [{ type: "text", text }] : []),
    ...images.map(({ mimeType, data }) => ({ type: "image", mimeType, data })),
  ];
}
