import { memo, useCallback, useEffect, useMemo, useRef } from "react";

import type { UiBootstrap } from "../../bootstrap/model";
import { MessageActionModel } from "../../domain/messageActions";
import type { EntryId, MessageId } from "../../acp/protocol";
import type { MessageEntity, SessionTreeEntry, ToolCallEntity } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";
import { initialSessionWorkspaceState } from "../../workspace/sessionState";
import { sessionWorkspaceOf } from "../../workspace/selectors";
import { useWorkspaceStore } from "../../workspace/store";
import type { ContextSelection } from "../inspector/ContextLens";
import { Composer } from "../composer/Composer";
import { MessageView } from "./MessageView";
import { ExtensionCard } from "./ExtensionCard";
import { ToolCallCard } from "./ToolCallCard";
import { BashExecutionCard } from "./BashExecutionCard";
import { CompactionCard } from "./CompactionCard";
import { useTranscriptScroll } from "./useTranscriptScroll";

export type ConversationProps = Readonly<{
  bootstrap: UiBootstrap;
  controller: WorkspaceController;
  contextSelection?: ContextSelection;
  onInspect: (selection: ContextSelection) => void;
}>;

/** Message row subscribes to its own entity so only the streaming row re-renders. */
const MessageRow = memo(function MessageRow({ messageId, entry, cwd, running, inspecting, onInspect, controller }: Readonly<{
  messageId: MessageId;
  entry?: SessionTreeEntry;
  cwd?: string;
  running: boolean;
  inspecting: boolean;
  onInspect: (message: MessageEntity) => void;
  controller: WorkspaceController;
}>) {
  const message = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.messages.get(messageId));
  return message === undefined ? null : <MessageView
    message={message}
    {...(entry === undefined ? {} : { entry })}
    {...(cwd === undefined ? {} : { cwd })}
    running={running}
    inspecting={inspecting}
    onInspect={onInspect}
    controller={controller}
  />;
});

/** Tool row subscribes to its own tool so only the streaming tool re-renders. */
const ToolRow = memo(function ToolRow({ toolCallId, inspecting, onInspect }: Readonly<{
  toolCallId: ToolCallEntity["toolCallId"];
  inspecting: boolean;
  onInspect: (tool: ToolCallEntity) => void;
}>) {
  const tool = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.tools.get(toolCallId));
  return tool === undefined ? null : <ToolCallCard tool={tool} inspecting={inspecting} onInspect={onInspect} />;
});

const BashRow = memo(function BashRow({ messageId }: Readonly<{ messageId: MessageId }>) {
  const bash = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.bashExecutions.get(messageId));
  return bash === undefined ? null : <BashExecutionCard bash={bash} />;
});

const ExtensionRow = memo(function ExtensionRow({ id }: Readonly<{ id: string }>) {
  const extension = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.extensions.get(id));
  return extension === undefined ? null : <ExtensionCard extension={extension} />;
});

const CompactionRow = memo(function CompactionRow({ entryId }: Readonly<{ entryId: EntryId }>) {
  const compaction = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.compactions.get(entryId));
  return compaction === undefined ? null : <CompactionCard compaction={compaction} />;
});

export function Conversation({ bootstrap, controller, contextSelection, onInspect }: ConversationProps) {
  const activeSessionId = useWorkspaceStore((state) => state.activeSessionId);
  // Subscribe only to the fields this view renders so ongoing streaming deltas
  // do not re-render the whole transcript; each row subscribes to its own entity.
  const transcript = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.transcript ?? initialSessionWorkspaceState.transcript);
  const tree = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.tree);
  const running = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.running ?? false);
  const compaction = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.compaction ?? initialSessionWorkspaceState.compaction);
  const pending = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.pending ?? initialSessionWorkspaceState.pending);
  const availableCommands = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.availableCommands ?? initialSessionWorkspaceState.availableCommands);
  const outcomeUnknown = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.outcomeUnknown ?? false);
  const cwd = useWorkspaceStore((state) => state.sessions
    .find((session) => session.sessionId === state.activeSessionId)?.cwd);
  const messageTreeIndex = useMemo(() => MessageActionModel.index(tree), [tree]);

  /** Resolves a Tool's nearest same-Turn assistant message from the live Tree. */
  const resolveToolOwner = useCallback((tool: ToolCallEntity): SessionTreeEntry | undefined => {
    if (tool.meta === undefined) return undefined;
    const store = useWorkspaceStore.getState();
    const current = store.activeSessionId === undefined ? undefined : store.sessionWorkspaces.get(store.activeSessionId);
    const messages = current?.messages;
    const treeAtClick = current?.tree;
    if (messages === undefined || treeAtClick === undefined) return undefined;
    const owner = [...messages.values()].reverse()
      .find((message) => message.role === "assistant" && message.turnId === tool.meta?.turnId);
    return owner === undefined ? undefined : MessageActionModel.entry(treeAtClick, owner.messageId);
  }, []);

  // Stable per-Session inspect callbacks keep memoized transcript rows from
  // re-rendering while unrelated messages stream; each row calls with itself.
  const handleInspectMessage = useCallback((message: MessageEntity) => {
    const treeEntry = messageTreeIndex.get(message.messageId);
    onInspect({
      type: "message",
      messageId: message.messageId,
      ...(treeEntry === undefined ? {} : { treeEntryId: treeEntry.entryId })
    });
  }, [messageTreeIndex, onInspect]);

  const handleInspectTool = useCallback((tool: ToolCallEntity) => {
    const owner = resolveToolOwner(tool);
    onInspect({
      type: "tool",
      toolCallId: tool.toolCallId,
      ...(owner === undefined ? {} : { treeEntryId: owner.entryId })
    });
  }, [onInspect, resolveToolOwner]);

  const activeDraft = useWorkspaceStore((state) => activeSessionId === undefined
    ? undefined
    : state.drafts.get(activeSessionId));
  const composerRevision = useWorkspaceStore((state) => state.composerRevision);
  // The scroll controller observes the DOM, so it needs no projection subscription.
  const { transcriptRef, onScroll } = useTranscriptScroll(activeSessionId);
  const liveRegionRef = useRef<HTMLDivElement>(null);
  const runningRef = useRef(running);
  useEffect(() => {
    const live = liveRegionRef.current;
    const wasRunning = runningRef.current;
    runningRef.current = running;
    // Announce a settled running turn only, never per streaming frame.
    if (live === null || !wasRunning || running) return;
    live.textContent = "Agent 已回复完成，可以发送新消息。";
    window.setTimeout(() => { live.textContent = ""; }, 1_000);
  }, [running]);

  if (activeSessionId === undefined) {
    return <section className="conversation-placeholder"><div className="empty-state"><h2>开始一个 Agent 会话</h2><p>从左侧恢复会话，或创建一个使用本机工作目录的新会话。</p></div></section>;
  }
  return (
    <section className="conversation-workspace">
      <div
        className="transcript"
        aria-label="会话消息"
        ref={transcriptRef}
        onScroll={onScroll}
      >
        {transcript.length === 0 ? <div className="empty-state transcript__empty"><h2>准备好了</h2><p>发送文本、图片、Markdown 或资源链接开始这一会话。</p></div> : null}
        {transcript.map((entry) => {
          if (entry.type === "recovery") {
            return <div className="recovery-divider" key={`recovery:${entry.notice.recoveryId}`} role="status">
              <span>已恢复 {entry.notice.eventCount} 条事件 · seq {entry.notice.sequenceRange.start}–{entry.notice.sequenceRange.end}</span>
            </div>;
          }
          if (entry.type === "message") {
            const treeEntry = messageTreeIndex.get(entry.messageId);
            return <MessageRow
              key={`message:${entry.messageId}`}
              messageId={entry.messageId}
              {...(treeEntry === undefined ? {} : { entry: treeEntry })}
              {...(cwd === undefined ? {} : { cwd })}
              running={running}
              inspecting={contextSelection?.type === "message" && contextSelection.messageId === entry.messageId}
              onInspect={handleInspectMessage}
              controller={controller}
            />;
          }
          if (entry.type === "extension") return <ExtensionRow key={`extension:${entry.extensionEventId}`} id={entry.extensionEventId} />;
          if (entry.type === "bash") return <BashRow key={`bash:${entry.messageId}`} messageId={entry.messageId} />;
          if (entry.type === "compaction") return <CompactionRow key={`compaction:${entry.entryId}`} entryId={entry.entryId} />;
          return <ToolRow
            key={`tool:${entry.toolCallId}`}
            toolCallId={entry.toolCallId}
            inspecting={contextSelection?.type === "tool" && contextSelection.toolCallId === entry.toolCallId}
            onInspect={handleInspectTool}
          />;
        })}
        {compaction.type === "running" ? <div className="compaction-activity" role="status"><span className="compaction-activity__pulse" />正在压缩上下文…</div> : null}
        {compaction.type === "failed" ? <div className="compaction-activity" data-tone="danger" role="alert">上下文压缩失败：{compaction.message}</div> : null}
        {compaction.type === "cancelled" ? <div className="compaction-activity" role="status">已取消上下文压缩</div> : null}
        <div className="visually-hidden" ref={liveRegionRef} role="status" />
      </div>
      <Composer
        key={`${activeSessionId}:${composerRevision}`}
        productSlug={bootstrap.product.slug}
        sessionId={activeSessionId}
        running={running}
        outcomeUnknown={outcomeUnknown}
        pending={pending}
        availableCommands={availableCommands}
        {...(activeDraft === undefined ? {} : { initialDraft: activeDraft })}
        controller={controller}
      />
    </section>
  );
}
