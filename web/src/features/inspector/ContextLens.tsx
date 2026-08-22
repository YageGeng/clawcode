import { X } from "lucide-react";

import type { EntryId, MessageId, ToolCallId } from "../../acp/protocol";
import type { MessageEntity, ToolCallEntity } from "../../domain/model";
import { MarkdownContent } from "../conversation/MarkdownContent";
import { ToolCallCard } from "../conversation/ToolCallCard";

export type ContextSelection =
  | Readonly<{ type: "message"; messageId: MessageId; treeEntryId?: EntryId }>
  | Readonly<{ type: "tool"; toolCallId: ToolCallId; treeEntryId?: EntryId }>;

export type ContextLensProps = Readonly<{
  selection: ContextSelection;
  message?: MessageEntity;
  tool?: ToolCallEntity;
  onClose: () => void;
}>;

/** Renders selected transcript diagnostics in one non-modal floating window. */
export function ContextLens({ selection, message, tool, onClose }: ContextLensProps) {
  const unavailable = selection.type === "message" ? message === undefined : tool === undefined;
  return <section aria-label="Context Lens" aria-modal="false" className="context-lens" role="dialog" tabIndex={-1}>
    <header>
      <div><span>Context lens</span><strong>{selection.type === "message" ? "消息" : "工具调用"}</strong></div>
      <button autoFocus className="icon-button" type="button" aria-label="关闭 Context Lens" title="关闭 Context Lens" onClick={onClose}><X size={15} aria-hidden="true" /></button>
    </header>
    {unavailable ? <div className="inspector-empty">选中的内容已不在当前 Session 中。</div> : null}
    {message === undefined ? null : <>
      <dl className="diagnostics-list">
        <dt>Role</dt><dd>{message.role}</dd>
        <dt>MessageId</dt><dd>{message.messageId}</dd>
        <dt>TurnId</dt><dd>{message.turnId}</dd>
        <dt>TimestampMs</dt><dd>{message.timestampMs}</dd>
        {message.sequenceRange === undefined ? null : <><dt>SequenceRange</dt><dd>{message.sequenceRange.start}–{message.sequenceRange.end}</dd></>}
        {message.assistant === undefined ? null : <><dt>Model</dt><dd>{message.assistant.providerId}/{message.assistant.modelId}</dd><dt>Tokens</dt><dd>{message.assistant.usage.totalTokens}</dd></>}
      </dl>
      {message.text.length === 0 ? null : <div className="context-lens__preview"><MarkdownContent text={message.text} /></div>}
    </>}
    {tool === undefined ? null : <ToolCallCard tool={tool} />}
  </section>;
}
