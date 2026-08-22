import { memo } from "react";

import type { MessageEntity, SessionTreeEntry } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";
import { CommandMessageCard } from "./CommandMessageCard";
import { MarkdownContent } from "./MarkdownContent";
import { MessageActions } from "./MessageActions";
import { ReasoningBlock } from "./ReasoningBlock";

export type MessageViewProps = Readonly<{
  message: MessageEntity;
  entry?: SessionTreeEntry;
  cwd?: string;
  running: boolean;
  inspecting: boolean;
  onInspect: (message: MessageEntity) => void;
  controller: WorkspaceController;
}>;

export const MessageView = memo(function MessageView({ message, entry, cwd, running, inspecting, onInspect, controller }: MessageViewProps) {
  const assistant = message.assistant;
  // ACP projects Tool Calls into dedicated transcript cards, which leaves a
  // settled tool-only assistant message with no independently visible content.
  // Keep that message in workspace state and the Session tree without rendering
  // an empty Agent shell in the conversation.
  if (
    message.role === "assistant"
    && message.slashCommand === undefined
    && !message.streaming
    && message.text.length === 0
    && message.images.length === 0
    && message.reasoning.length === 0
    && assistant?.error === undefined
  ) return null;

  const actions = (
    <MessageActions
      message={message}
      {...(entry === undefined ? {} : { entry })}
      {...(cwd === undefined ? {} : { cwd })}
      running={running}
      inspecting={inspecting}
      onInspect={onInspect}
      controller={controller}
    />
  );
  if (message.slashCommand !== undefined) {
    return <CommandMessageCard message={message} actions={actions} />;
  }
  return (
    <article className="message" data-role={message.role} data-error={assistant?.error === undefined ? undefined : "true"} data-streaming={message.streaming}>
      <div className="message__label">{message.role === "user" ? "你" : message.role === "assistant" ? "Agent" : "System"}</div>
      <ReasoningBlock message={message} />
      {assistant?.error === undefined ? null : <div className="message-error" role="alert"><strong>模型调用失败</strong><span>{assistant.error}</span></div>}
      {message.text.length === 0 ? null : (
        <div className="message__body">
          <MarkdownContent text={message.text} highlight={!message.streaming} />
          {message.streaming ? <span className="streaming-cursor" aria-label="正在生成" /> : null}
        </div>
      )}
      {message.images.length === 0 ? null : (
        <div className="message__images">
          {message.images.map((image, index) => <img key={`${image.mimeType}:${index}`} src={`data:${image.mimeType};base64,${image.data}`} alt={`消息图片 ${index + 1}`} />)}
        </div>
      )}
      {actions}
      <details className="message__diagnostics">
        <summary>消息详情</summary>
        <dl className="diagnostics-list">
          <dt>MessageId</dt><dd>{message.messageId}</dd>
          <dt>TurnId</dt><dd>{message.turnId}</dd>
          <dt>TimestampMs</dt><dd>{message.timestampMs}</dd>
          <dt>StartedAtMs</dt><dd>{message.startedAtMs}</dd>
          <dt>EndedAtMs</dt><dd>{message.endedAtMs}</dd>
          {message.sequenceRange === undefined ? null : <><dt>SequenceRange</dt><dd>{message.sequenceRange.start}–{message.sequenceRange.end}</dd></>}
          {assistant === undefined ? null : <>
            <dt>Provider</dt><dd>{assistant.providerId}</dd>
            <dt>Model</dt><dd>{assistant.modelId}</dd>
            <dt>StopReason</dt><dd>{assistant.stopReason}{assistant.rawStopReason === undefined ? "" : ` (${assistant.rawStopReason})`}</dd>
            <dt>Usage</dt><dd>in {assistant.usage.inputTokens} · out {assistant.usage.outputTokens} · cache read {assistant.usage.cacheReadTokens} · cache write {assistant.usage.cacheWriteTokens} · total {assistant.usage.totalTokens}</dd>
            {assistant.error === undefined ? null : <><dt>Error</dt><dd>{assistant.error}</dd></>}
          </>}
        </dl>
      </details>
    </article>
  );
});
