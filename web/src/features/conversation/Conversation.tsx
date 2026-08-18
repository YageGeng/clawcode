import type { UiBootstrap } from "../../bootstrap/model";
import { MessageActionModel } from "../../domain/messageActions";
import type { WorkspaceController } from "../../workspace/controller";
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
  const state = useWorkspaceStore();
  const cwd = state.sessions.find((session) => session.sessionId === state.activeSessionId)?.cwd;
  // Streaming entities replace their collection identity on every update, so
  // the scroll controller can follow new content without owning workspace state.
  const { transcriptRef, onScroll } = useTranscriptScroll({
    sessionId: state.activeSessionId,
    messages: state.messages,
    tools: state.tools,
    bashExecutions: state.bashExecutions,
    extensions: state.extensions,
    compactions: state.compactions,
    compactionStatus: state.compaction,
    transcriptLength: state.transcript.length
  });

  if (state.activeSessionId === undefined) {
    return <section className="conversation-placeholder"><div className="empty-state"><h2>开始一个 Agent 会话</h2><p>从左侧恢复会话，或创建一个使用本机工作目录的新会话。</p></div></section>;
  }
  const activeDraft = state.drafts.get(state.activeSessionId);
  return (
    <section className="conversation-workspace">
      <div
        className="transcript"
        aria-live="polite"
        ref={transcriptRef}
        onScroll={onScroll}
      >
        {state.transcript.length === 0 ? <div className="empty-state transcript__empty"><h2>准备好了</h2><p>发送文本、图片、Markdown 或资源链接开始这一会话。</p></div> : null}
        {state.transcript.map((entry) => {
          if (entry.type === "message") {
            const message = state.messages.get(entry.messageId);
            const treeEntry = MessageActionModel.entry(state.tree, entry.messageId);
            return message === undefined ? null : (
              <MessageView
                key={`message:${entry.messageId}`}
                message={message}
                {...(treeEntry === undefined ? {} : { entry: treeEntry })}
                {...(cwd === undefined ? {} : { cwd })}
                running={state.running}
                controller={controller}
              />
            );
          }
          if (entry.type === "extension") {
            const extension = state.extensions.get(entry.extensionEventId);
            return extension === undefined ? null : <ExtensionCard key={`extension:${entry.extensionEventId}`} extension={extension} />;
          }
          if (entry.type === "bash") {
            const bash = state.bashExecutions.get(entry.messageId);
            return bash === undefined ? null : <BashExecutionCard key={`bash:${entry.messageId}`} bash={bash} />;
          }
          if (entry.type === "compaction") {
            const compaction = state.compactions.get(entry.entryId);
            return compaction === undefined ? null : <CompactionCard key={`compaction:${entry.entryId}`} compaction={compaction} />;
          }
          const tool = state.tools.get(entry.toolCallId);
          return tool === undefined ? null : <ToolCallCard key={`tool:${entry.toolCallId}`} tool={tool} />;
        })}
        {state.compaction.type === "running" ? <div className="compaction-activity" role="status"><span className="compaction-activity__pulse" />正在压缩上下文…</div> : null}
        {state.compaction.type === "failed" ? <div className="compaction-activity" data-tone="danger" role="alert">上下文压缩失败：{state.compaction.message}</div> : null}
        {state.compaction.type === "cancelled" ? <div className="compaction-activity" role="status">已取消上下文压缩</div> : null}
      </div>
      <Composer
        key={`${state.activeSessionId}:${state.composerRevision}`}
        productSlug={bootstrap.product.slug}
        sessionId={state.activeSessionId}
        running={state.running}
        outcomeUnknown={state.outcomeUnknown}
        pending={state.pending}
        availableCommands={state.availableCommands}
        {...(activeDraft === undefined ? {} : { initialDraft: activeDraft })}
        controller={controller}
      />
    </section>
  );
}
