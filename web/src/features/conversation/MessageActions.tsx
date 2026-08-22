import { Check, Copy, GitFork, Pencil, ScanSearch } from "lucide-react";
import { useState } from "react";

import { MessageActionModel } from "../../domain/messageActions";
import type { MessageEntity, SessionTreeEntry } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";

export type MessageActionsProps = Readonly<{
  message: MessageEntity;
  entry?: SessionTreeEntry;
  cwd?: string;
  running: boolean;
  inspecting: boolean;
  onInspect: (message: MessageEntity) => void;
  controller: WorkspaceController;
}>;

/** Renders Codex-style actions bound to one persisted transcript message. */
export function MessageActions({ message, entry, cwd, running, inspecting, onInspect, controller }: MessageActionsProps) {
  const [copied, setCopied] = useState(false);
  const [busy, setBusy] = useState<"edit" | "fork">();
  const [error, setError] = useState<string>();
  const editPlan = entry === undefined ? undefined : MessageActionModel.editPlan(entry);
  const disabled = message.streaming || running || busy !== undefined;

  if (message.role !== "user" && message.role !== "assistant") return null;
  return (
    <div className="message__action-area">
      <div className="message__actions">
        <button
          className="message-action"
          type="button"
          aria-label="在 Context Lens 中检查消息"
          aria-pressed={inspecting}
          title="在 Context Lens 中检查"
          onClick={() => onInspect(message)}
        ><ScanSearch size={15} aria-hidden="true" /></button>
        <button
          className="message-action"
          type="button"
          aria-label="复制消息"
          title={copied ? "已复制" : "复制消息"}
          disabled={message.text.length === 0}
          onClick={() => {
            void navigator.clipboard.writeText(message.text).then(() => {
              setCopied(true);
              setError(undefined);
              window.setTimeout(() => setCopied(false), 1_200);
            }).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)));
          }}
        >{copied ? <Check size={15} /> : <Copy size={15} />}</button>
        {message.role !== "user" || editPlan === undefined ? null : (
          <button
            className="message-action"
            type="button"
            aria-label="编辑并重新提问"
            title="Branch 并编辑这条消息"
            disabled={disabled}
            onClick={() => {
              setBusy("edit");
              setError(undefined);
              void controller.branchForEditing(editPlan).catch((reason: unknown) => {
                setError(reason instanceof Error ? reason.message : String(reason));
              }).finally(() => setBusy(undefined));
            }}
          ><Pencil size={15} /></button>
        )}
        {message.role !== "assistant" || entry === undefined || cwd === undefined ? null : (
          <button
            className="message-action"
            type="button"
            aria-label="分叉为新会话"
            title="Fork 为新会话"
            disabled={disabled}
            onClick={() => {
              setBusy("fork");
              setError(undefined);
              void controller.fork(entry.entryId, cwd).catch((reason: unknown) => {
                setError(reason instanceof Error ? reason.message : String(reason));
              }).finally(() => setBusy(undefined));
            }}
          ><GitFork size={15} /></button>
        )}
      </div>
      {error === undefined ? null : <div className="message-action-error" role="alert">{error}</div>}
    </div>
  );
}
