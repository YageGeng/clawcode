import { ArrowUpToLine, ChevronDown, ListOrdered, LoaderCircle, Trash2, X } from "lucide-react";
import { useId, useState } from "react";

import type { PendingMessages, QueuedMessage } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";

type PendingQueueProps = Readonly<{
  pending: PendingMessages;
  controller: WorkspaceController;
  onError: (reason: unknown) => void;
  onHint: (message: string) => void;
}>;

type QueueAction = { kind: "clear" } | { kind: "promote" | "remove"; queueId: string };

const QueuedMessageView = {
  /** Preserves the original command and attachment summary when inspecting queued input. */
  text(item: QueuedMessage): string {
    const content = item.message.content;
    if (typeof content !== "object" || content === null || Array.isArray(content)) return item.queueId;
    const contentRecord = content as Record<string, unknown>;
    const expansion = typeof contentRecord.expansion === "object" && contentRecord.expansion !== null && !Array.isArray(contentRecord.expansion)
      ? contentRecord.expansion as Record<string, unknown>
      : undefined;
    const invocation = typeof expansion?.invocation === "object" && expansion.invocation !== null && !Array.isArray(expansion.invocation)
      ? expansion.invocation as Record<string, unknown>
      : undefined;
    if (typeof invocation?.original === "string") return invocation.original;
    const blocks = contentRecord.blocks;
    if (!Array.isArray(blocks)) return item.queueId;
    const text = blocks.map((block) => {
      if (typeof block !== "object" || block === null || Array.isArray(block)) return "";
      const text = (block as Record<string, unknown>).text;
      return typeof text === "string" ? text : "";
    }).join("");
    const imageCount = blocks.filter((block) => typeof block === "object" && block !== null && !Array.isArray(block) && (block as Record<string, unknown>).type === "image").length;
    return [text, imageCount === 0 ? "" : `${imageCount} 张图片`].filter((part) => part.length > 0).join(" · ") || item.queueId;
  }
} as const;

/** Keeps queued input inspectable without letting long messages displace the composer. */
export function PendingQueue({ pending, controller, onError, onHint }: PendingQueueProps) {
  const listId = useId();
  const [expanded, setExpanded] = useState(true);
  const [confirmClear, setConfirmClear] = useState(false);
  const [busy, setBusy] = useState(false);
  const queued = [...pending.steering, ...pending.followUp];

  /** Waits for authoritative queue updates and prevents overlapping destructive actions. */
  const updateQueue = async (action: QueueAction) => {
    if (busy) return;
    setBusy(true);
    try {
      if (action.kind === "clear") {
        await controller.clearPending();
        setConfirmClear(false);
      } else if (action.kind === "remove") {
        await controller.removePending(action.queueId);
      } else {
        const promoted = await controller.promotePending(action.queueId);
        onHint(promoted ? "已改为本轮补充，将在下一次模型请求前处理" : "这条消息已开始处理，无法更改发送时机");
      }
    } catch (reason: unknown) {
      onError(reason);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="pending-queue" aria-label="待发送消息" aria-busy={busy}>
      <div className="pending-queue__heading">
        <button className="pending-queue__toggle" type="button" aria-expanded={expanded} aria-controls={listId} onClick={() => setExpanded(!expanded)}>
          <ListOrdered size={15} aria-hidden="true" />
          <span>待发送</span>
          <span className="pending-queue__count" role="status" aria-label={`${queued.length} 条待发送消息`}>{queued.length}</span>
          <span className="pending-queue__description">{pending.steering.length > 0 ? `${pending.steering.length} 条本轮补充 · ${pending.followUp.length} 条稍后发送` : "当前任务完成后依次发送"}</span>
          <ChevronDown className="pending-queue__chevron" size={14} aria-hidden="true" />
        </button>
        <button className="pending-queue__clear" type="button" disabled={busy} aria-expanded={confirmClear} onClick={() => setConfirmClear(!confirmClear)}>
          {busy ? <LoaderCircle className="pending-queue__spinner" size={14} aria-hidden="true" /> : <Trash2 size={14} aria-hidden="true" />}
          <span>{busy ? "处理中" : "清空"}</span>
        </button>
      </div>
      {confirmClear ? (
        <div className="pending-queue__confirm" role="group" aria-label="确认清空待发送消息">
          <span>清空全部 {queued.length} 条待发送消息？</span>
          <button type="button" disabled={busy} onClick={() => setConfirmClear(false)}>保留</button>
          <button className="pending-queue__confirm-clear" type="button" disabled={busy} onClick={() => void updateQueue({ kind: "clear" })}>确认清空</button>
        </div>
      ) : null}
      <div id={listId} className="pending-queue__list" hidden={!expanded}>
        {queued.map((item, index) => {
          const text = QueuedMessageView.text(item);
          return (
            <div className="pending-item" key={item.queueId}>
              <span className="pending-item__position" aria-hidden="true">{index + 1}</span>
              <span className="pending-item__kind" data-kind={item.kind} title={item.kind === "steering" ? "在下一次模型请求前补充给当前任务" : "当前任务完成后发送"}>{item.kind === "steering" ? "本轮" : "稍后"}</span>
              <details className="pending-item__details">
                <summary aria-label={`第 ${index + 1} 条消息详情`}>
                  <span className="pending-item__preview">{text}</span>
                  <ChevronDown size={13} aria-hidden="true" />
                </summary>
                <div className="pending-item__content">{text}</div>
              </details>
              <div className="pending-item__actions">
                {item.kind === "followUp" ? <button className="pending-item__promote" type="button" disabled={busy} title="改为本轮补充，在下一次模型请求前处理" aria-label={`将第 ${index + 1} 条消息改为本轮补充`} onClick={() => void updateQueue({ kind: "promote", queueId: item.queueId })}><ArrowUpToLine size={14} aria-hidden="true" /><span>本轮补充</span></button> : null}
                <button className="pending-item__remove" type="button" disabled={busy} title="移除消息" aria-label={`移除第 ${index + 1} 条待发送消息`} onClick={() => void updateQueue({ kind: "remove", queueId: item.queueId })}><X size={15} aria-hidden="true" /></button>
              </div>
            </div>
          );
        })}
      </div>
    </section>
  );
}
