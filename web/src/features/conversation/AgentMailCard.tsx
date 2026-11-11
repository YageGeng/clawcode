import { ChevronDown, Mail } from "lucide-react";
import { useState } from "react";

import type { ExtensionEntity } from "../../domain/model";
import { AgentMails, TranscriptRowKeys } from "../../domain/model";
import { MarkdownContent } from "./MarkdownContent";

const PAYLOAD_MARKER = "\nPayload:\n";

const KIND_LABELS: Readonly<Record<string, string>> = {
  message: "消息",
  new_task: "任务",
  result: "结果"
};

/**
 * Renders one inter-agent mail as a collapsed row.
 *
 * Agent-to-agent traffic is mostly noise for the human reader, so the card keeps
 * the payload folded and shows only the routing header (author → recipient plus
 * the mail kind). Expanding is a per-card, in-memory choice; a remounted card
 * folds again. The `<details>` summary intercepts the native toggle so React
 * owns the open state exactly once, mirroring `ReasoningBlock`.
 */
export function AgentMailCard({ extension }: Readonly<{ extension: ExtensionEntity }>) {
  const [open, setOpen] = useState(false);
  const text = extension.blocks
    .map((block) => typeof block === "object" && block !== null && "text" in block && typeof block.text === "string" ? block.text : "")
    .join("");
  // The Kernel writes a fixed header before `Payload:`; the payload is what people read.
  const marker = text.indexOf(PAYLOAD_MARKER);
  const payload = marker < 0 ? text : text.slice(marker + PAYLOAD_MARKER.length);
  const kind = AgentMails.detail(extension, "kind") ?? "message";
  return (
    <details
      className="extension-card agent-mail-card"
      data-kind={kind}
      data-transcript-key={TranscriptRowKeys.extension(extension.id)}
      open={open}
    >
      <summary onClick={(event) => {
        // Prevent the native toggle and flip the controlled state instead, so the
        // rendered `open` attribute never disagrees with React's state.
        event.preventDefault();
        setOpen((current) => !current);
      }}>
        <Mail size={13} aria-hidden="true" />
        <span className="agent-mail-card__route">
          {AgentMails.detail(extension, "author") ?? "?"} → {AgentMails.detail(extension, "recipient") ?? "?"}
        </span>
        <small className="agent-mail-card__kind">{KIND_LABELS[kind] ?? kind}</small>
        <ChevronDown className="agent-mail-card__chevron" size={14} aria-hidden="true" />
      </summary>
      {open ? <div className="agent-mail-card__body"><MarkdownContent text={payload} /></div> : null}
    </details>
  );
}
