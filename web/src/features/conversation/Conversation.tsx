import type { UiBootstrap } from "../../bootstrap/model";
import { MessageActionModel } from "../../domain/messageActions";
import type { WorkspaceController } from "../../workspace/controller";
import { initialSessionWorkspaceState } from "../../workspace/sessionState";
import { useWorkspaceStore } from "../../workspace/store";
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
}>;

export function Conversation({ bootstrap, controller }: ConversationProps) {
  const activeSessionId = useWorkspaceStore((state) => state.activeSessionId);
  const workspace = useWorkspaceStore((state) => state.activeSessionId === undefined
    ? initialSessionWorkspaceState
    : state.sessionWorkspaces.get(state.activeSessionId) ?? initialSessionWorkspaceState);
  const cwd = useWorkspaceStore((state) => state.sessions
    .find((session) => session.sessionId === state.activeSessionId)?.cwd);
  const activeDraft = useWorkspaceStore((state) => activeSessionId === undefined
    ? undefined
    : state.drafts.get(activeSessionId));
  const composerRevision = useWorkspaceStore((state) => state.composerRevision);
  // Streaming entities replace their collection identity on every update, so
  // the scroll controller can follow new content without owning workspace state.
  const { transcriptRef, onScroll } = useTranscriptScroll({
    sessionId: activeSessionId,
    messages: workspace.messages,
    tools: workspace.tools,
    bashExecutions: workspace.bashExecutions,
    extensions: workspace.extensions,
    compactions: workspace.compactions,
    compactionStatus: workspace.compaction,
    transcriptLength: workspace.transcript.length
  });

  if (activeSessionId === undefined) {
    return <section className="conversation-placeholder"><div className="empty-state"><h2>开始一个 Agent 会话</h2><p>从左侧恢复会话，或创建一个使用本机工作目录的新会话。</p></div></section>;
  }
  return (
    <section className="conversation-workspace">
      <div
        className="transcript"
        aria-live="polite"
        ref={transcriptRef}
        onScroll={onScroll}
      >
        {workspace.transcript.length === 0 ? <div className="empty-state transcript__empty"><h2>准备好了</h2><p>发送文本、图片、Markdown 或资源链接开始这一会话。</p></div> : null}
        {workspace.transcript.map((entry) => {
          if (entry.type === "message") {
            const message = workspace.messages.get(entry.messageId);
            const treeEntry = MessageActionModel.entry(workspace.tree, entry.messageId);
            return message === undefined ? null : (
              <MessageView
                key={`message:${entry.messageId}`}
                message={message}
                {...(treeEntry === undefined ? {} : { entry: treeEntry })}
                {...(cwd === undefined ? {} : { cwd })}
                running={workspace.running}
                controller={controller}
              />
            );
          }
          if (entry.type === "extension") {
            const extension = workspace.extensions.get(entry.extensionEventId);
            return extension === undefined ? null : <ExtensionCard key={`extension:${entry.extensionEventId}`} extension={extension} />;
          }
          if (entry.type === "bash") {
            const bash = workspace.bashExecutions.get(entry.messageId);
            return bash === undefined ? null : <BashExecutionCard key={`bash:${entry.messageId}`} bash={bash} />;
          }
          if (entry.type === "compaction") {
            const compaction = workspace.compactions.get(entry.entryId);
            return compaction === undefined ? null : <CompactionCard key={`compaction:${entry.entryId}`} compaction={compaction} />;
          }
          const tool = workspace.tools.get(entry.toolCallId);
          return tool === undefined ? null : <ToolCallCard key={`tool:${entry.toolCallId}`} tool={tool} />;
        })}
        {workspace.compaction.type === "running" ? <div className="compaction-activity" role="status"><span className="compaction-activity__pulse" />正在压缩上下文…</div> : null}
        {workspace.compaction.type === "failed" ? <div className="compaction-activity" data-tone="danger" role="alert">上下文压缩失败：{workspace.compaction.message}</div> : null}
        {workspace.compaction.type === "cancelled" ? <div className="compaction-activity" role="status">已取消上下文压缩</div> : null}
      </div>
      <Composer
        key={`${activeSessionId}:${composerRevision}`}
        productSlug={bootstrap.product.slug}
        sessionId={activeSessionId}
        running={workspace.running}
        outcomeUnknown={workspace.outcomeUnknown}
        pending={workspace.pending}
        availableCommands={workspace.availableCommands}
        {...(activeDraft === undefined ? {} : { initialDraft: activeDraft })}
        controller={controller}
      />
    </section>
  );
}
