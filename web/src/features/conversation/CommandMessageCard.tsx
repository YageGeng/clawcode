import type { ReactNode } from "react";

import type { MessageEntity, SlashCommandMessageMeta, SlashCommandSource } from "../../domain/model";
import { MarkdownContent } from "./MarkdownContent";

const SOURCE_LABELS: Readonly<Record<SlashCommandSource, string>> = {
  builtin: "Builtin",
  extension: "Extension",
  skill: "Skill",
  prompt_template: "Template"
};

export type CommandMessageCardProps = Readonly<{
  message: MessageEntity;
  actions?: ReactNode;
}>;

const STATUS_LABELS: Readonly<Record<NonNullable<SlashCommandMessageMeta["status"]>, string>> = {
  succeeded: "成功",
  failed: "失败",
  cancelled: "已取消"
};

/** Renders one decoded Slash Command message without inspecting raw ACP data. */
export function CommandMessageCard({ message, actions }: CommandMessageCardProps) {
  const command = message.slashCommand;
  if (command === undefined) return null;
  return (
    <article className="command-message" data-kind={command.messageKind} data-status={command.status}>
      <header className="command-message__header">
        <code>/{command.name}</code>
        <span className="command-source-badge" data-source={command.source}>{SOURCE_LABELS[command.source]}</span>
        <span className="command-message__kind">{command.messageKind === "output" ? "结果" : command.messageKind === "expansion" ? "展开调用" : "调用"}</span>
        {command.status === undefined ? null : <span className="command-message__status">{STATUS_LABELS[command.status]}</span>}
      </header>
      {message.text.length === 0 ? null : <div className="command-message__body"><MarkdownContent text={message.text} /></div>}
      {actions}
      <details className="message__diagnostics">
        <summary>消息详情</summary>
        <dl className="diagnostics-list">
          <dt>MessageId</dt><dd>{message.messageId}</dd>
          <dt>TurnId</dt><dd>{message.turnId}</dd>
          <dt>TimestampMs</dt><dd>{message.timestampMs}</dd>
          <dt>StartedAtMs</dt><dd>{message.startedAtMs}</dd>
          <dt>EndedAtMs</dt><dd>{message.endedAtMs}</dd>
        </dl>
      </details>
    </article>
  );
}
