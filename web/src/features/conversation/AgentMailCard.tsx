import { Mail } from "lucide-react";

import type { ExtensionEntity } from "../../domain/model";
import { AgentMails, TranscriptRowKeys } from "../../domain/model";
import { MarkdownContent } from "./MarkdownContent";

const PAYLOAD_MARKER = "\nPayload:\n";

const KIND_LABELS: Readonly<Record<string, string>> = {
  message: "消息",
  new_task: "任务",
  result: "结果"
};

/** Renders one inter-agent mail: who sent it to whom, its kind, and its payload. */
export function AgentMailCard({ extension }: Readonly<{ extension: ExtensionEntity }>) {
  const text = extension.blocks
    .map((block) => typeof block === "object" && block !== null && "text" in block && typeof block.text === "string" ? block.text : "")
    .join("");
  // The Kernel writes a fixed header before `Payload:`; the payload is what people read.
  const marker = text.indexOf(PAYLOAD_MARKER);
  const payload = marker < 0 ? text : text.slice(marker + PAYLOAD_MARKER.length);
  const kind = AgentMails.detail(extension, "kind") ?? "message";
  return (
    <article className="extension-card agent-mail-card" data-kind={kind} data-transcript-key={TranscriptRowKeys.extension(extension.id)}>
      <header>
        <span><Mail size={13} aria-hidden="true" /> {AgentMails.detail(extension, "author") ?? "?"} → {AgentMails.detail(extension, "recipient") ?? "?"}</span>
        <small>{KIND_LABELS[kind] ?? kind}</small>
      </header>
      <div className="agent-mail-card__body"><MarkdownContent text={payload} /></div>
    </article>
  );
}
