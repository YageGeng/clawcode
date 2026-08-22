import { BrainCircuit, ChevronDown } from "lucide-react";
import { useState } from "react";

import type { MessageEntity } from "../../domain/model";
import { MarkdownContent } from "./MarkdownContent";

export type ReasoningBlockProps = Readonly<{
  message: MessageEntity;
}>;

export function ReasoningBlock({ message }: ReasoningBlockProps) {
  const [open, setOpen] = useState(false);
  if (message.reasoning.length === 0) return null;
  const elapsedMs = Number(BigInt(message.endedAtMs) - BigInt(message.startedAtMs));
  // Keep active reasoning visible, then collapse it when streaming settles
  // unless the user explicitly chooses to leave the completed trace open.
  const expanded = message.streaming || open;
  return (
    <details className="reasoning-block" open={expanded}>
      <summary onClick={(event) => {
        // Prevent the native toggle and update controlled state exactly once;
        // mirroring `onToggle` can bounce when React changes the open attribute.
        event.preventDefault();
        if (!message.streaming) setOpen((current) => !current);
      }}>
        <BrainCircuit size={15} aria-hidden="true" />
        <span>Reasoning trace</span>
        <span className="reasoning-block__status">{message.streaming ? "进行中" : (elapsedMs < 0 ? "—" : `${elapsedMs} ms`)}</span>
        <ChevronDown className="reasoning-block__chevron" size={14} aria-hidden="true" />
      </summary>
      {expanded ? <div className="reasoning-block__content"><MarkdownContent text={message.reasoning} highlight={!message.streaming} /></div> : null}
    </details>
  );
}
