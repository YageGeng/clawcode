import { ChevronDown, Mail } from "lucide-react";
import { useState } from "react";

import type { SessionId } from "../../acp/protocol";
import type { ExtensionEntity } from "../../domain/model";
import { AgentMails, TranscriptRowKeys } from "../../domain/model";
import { useWorkspaceStore } from "../../workspace/store";
import { MarkdownContent } from "./MarkdownContent";

const PAYLOAD_MARKER = "\nPayload:\n";

const KIND_LABELS: Readonly<Record<string, string>> = {
  message: "消息",
  new_task: "任务",
  result: "结果"
};

/**
 * Renders one inter-agent mail as a tool-card styled row.
 *
 * Messages and results are mostly noise for the human reader, so they keep the
 * payload folded behind the routing header (author → recipient plus the mail
 * kind). A `new_task` is the receiving agent's prompt, so it starts expanded.
 * Agents show the nicknames used by the pills and Sidebar, with the canonical
 * path as the title. Expanding is a per-card, in-memory choice; a remounted card
 * resets. The `<details>` summary intercepts the native toggle so React owns the
 * open state exactly once, mirroring `ReasoningBlock`.
 */
export function AgentMailCard({ sessionId, extension }: Readonly<{ sessionId: SessionId; extension: ExtensionEntity }>) {
  const [open, setOpen] = useState(() => AgentMails.isNewTask(extension));
  const author = AgentMails.detail(extension, "author") ?? "?";
  const recipient = AgentMails.detail(extension, "recipient") ?? "?";
  // Every mounted card's selector runs on each store commit, so it only picks
  // the list reference (changed by a list refresh); the scans below then run
  // on re-render, not on every streamed frame of any Session.
  const sessions = useWorkspaceStore((state) => state.sessions);
  const authorName = AgentMails.nickname(sessions, sessionId, author) ?? author;
  const recipientName = AgentMails.nickname(sessions, sessionId, recipient) ?? recipient;
  const text = extension.blocks
    .map((block) => typeof block === "object" && block !== null && "text" in block && typeof block.text === "string" ? block.text : "")
    .join("");
  // The Kernel writes a fixed header before `Payload:`; the payload is what people read.
  const marker = text.indexOf(PAYLOAD_MARKER);
  const payload = marker < 0 ? text : text.slice(marker + PAYLOAD_MARKER.length);
  const kind = AgentMails.detail(extension, "kind") ?? "message";
  return (
    <details
      className="tool-card agent-mail-card"
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
        <span className="tool-card__icon"><Mail size={14} aria-hidden="true" /></span>
        <span className="tool-card__title agent-mail-card__route" title={`${author} → ${recipient}`}>
          {authorName} → {recipientName}
        </span>
        <span className="tool-card__status">{KIND_LABELS[kind] ?? kind}</span>
        <ChevronDown className="tool-card__chevron" size={14} aria-hidden="true" />
      </summary>
      {open ? <div className="agent-mail-card__body"><MarkdownContent text={payload} /></div> : null}
    </details>
  );
}
