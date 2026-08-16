import { useEffect, useRef } from "react";

import type { UiBootstrap } from "../../bootstrap/model";
import type { WorkspaceController } from "../../workspace/controller";
import { useWorkspaceStore } from "../../workspace/store";
import { Composer } from "../composer/Composer";
import { MessageView } from "./MessageView";
import { ExtensionCard } from "./ExtensionCard";
import { ToolCallCard } from "./ToolCallCard";
import { BashExecutionCard } from "./BashExecutionCard";

export type ConversationProps = Readonly<{
  bootstrap: UiBootstrap;
  controller: WorkspaceController;
}>;

export function Conversation({ bootstrap, controller }: ConversationProps) {
  const state = useWorkspaceStore();
  const transcript = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const container = transcript.current;
    if (container !== null) container.scrollTop = container.scrollHeight;
  }, [state.bashExecutions, state.extensions, state.messages, state.tools, state.transcript.length]);

  if (state.activeSessionId === undefined) {
    return <section className="conversation-placeholder"><div className="empty-state"><h2>开始一个 Agent 会话</h2><p>从左侧恢复会话，或创建一个使用本机工作目录的新会话。</p></div></section>;
  }
  return (
    <section className="conversation-workspace">
      <div className="transcript" aria-live="polite" ref={transcript}>
        {state.transcript.length === 0 ? <div className="empty-state transcript__empty"><h2>准备好了</h2><p>发送文本、Markdown 或资源链接开始这一会话。</p></div> : null}
        {state.transcript.map((entry) => {
          if (entry.type === "message") {
            const message = state.messages.get(entry.messageId);
            return message === undefined ? null : <MessageView key={`message:${entry.messageId}`} message={message} />;
          }
          if (entry.type === "extension") {
            const extension = state.extensions.get(entry.extensionEventId);
            return extension === undefined ? null : <ExtensionCard key={`extension:${entry.extensionEventId}`} extension={extension} />;
          }
          if (entry.type === "bash") {
            const bash = state.bashExecutions.get(entry.messageId);
            return bash === undefined ? null : <BashExecutionCard key={`bash:${entry.messageId}`} bash={bash} />;
          }
          const tool = state.tools.get(entry.toolCallId);
          return tool === undefined ? null : <ToolCallCard key={`tool:${entry.toolCallId}`} tool={tool} />;
        })}
      </div>
      <Composer
        key={state.activeSessionId}
        productSlug={bootstrap.product.slug}
        sessionId={state.activeSessionId}
        running={state.running}
        outcomeUnknown={state.outcomeUnknown}
        pending={state.pending}
        availableCommands={state.availableCommands}
        controller={controller}
      />
    </section>
  );
}
