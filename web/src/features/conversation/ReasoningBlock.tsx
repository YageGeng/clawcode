import { BrainCircuit, ChevronDown } from "lucide-react";
import { useState } from "react";

import type { MessageEntity } from "../../domain/model";

export type ReasoningBlockProps = Readonly<{
  message: MessageEntity;
}>;

export function ReasoningBlock({ message }: ReasoningBlockProps) {
  const [open, setOpen] = useState(true);
  if (message.reasoning.length === 0) return null;
  const elapsed = BigInt(message.endedAtMs) - BigInt(message.startedAtMs);
  return (
    <details className="reasoning-block" open={open}>
      <summary onClick={(event) => {
        // Prevent the native toggle and update controlled state exactly once;
        // mirroring `onToggle` can bounce when React changes the open attribute.
        event.preventDefault();
        setOpen((current) => !current);
      }}>
        <BrainCircuit size={15} aria-hidden="true" />
        <span>推理</span>
        <span className="reasoning-block__status">{message.streaming ? "进行中" : `${elapsed} ms`}</span>
        <ChevronDown className="reasoning-block__chevron" size={14} aria-hidden="true" />
      </summary>
      <div className="reasoning-block__content">{message.reasoning}</div>
    </details>
  );
}
