import { Buffer } from "buffer";
import { Picker } from "nsh";
import { t } from "nsh/i18n";

import type { PromptImage } from "../client/acp/prompt";

/** The prompt limits enforced by the ACP server in crates/acp/src/input.rs. */
export const imageLimit = 5;
const maxImageBytes = 10 * 1024 * 1024;
const maxTotalBytes = 20 * 1024 * 1024;
const imageFormats = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/** Reads only explicitly picked local files and validates the whole selection before changing a draft. */
export async function pickImages(existing: readonly PromptImage[]): Promise<readonly PromptImage[]> {
  const room = imageLimit - existing.length;
  if (room < 1) throw new Error(t("attachments.tooMany"));
  const files = await Picker.pickFiles({
    // Native pickers truncate at the requested limit. One extra result detects overflow before encoding any attachment.
    multiple: true, limit: room + 1, maxSize: maxImageBytes,
    filters: [{ name: t("attachments.formats"), extensions: ["png", "jpg", "jpeg", "gif", "webp"] }],
  });
  if (files.length > room) throw new Error(t("attachments.tooMany"));
  if (files.some((item) => item.status === "tooLarge" || item.size > maxImageBytes)) throw new Error(t("attachments.tooLarge"));
  if (files.some((item) => !item.file || !item.size || !imageFormats.includes(item.type))) throw new Error(t("attachments.unsupported"));
  const sum = files.reduce((bytes, item) => bytes + item.size, existing.reduce((bytes, item) => bytes + item.size, 0));
  if (sum > maxTotalBytes) throw new Error(t("attachments.totalSize"));
  const images: PromptImage[] = [];
  // Encode one file at a time so only one raw file buffer is needed in addition to the final attachment set.
  for (const item of files) {
    const data = Buffer.from(await item.file!.arrayBuffer()).toString("base64");
    images.push({ id: crypto.randomUUID(), name: item.name, size: item.size, mimeType: item.type, data });
  }
  return images;
}
