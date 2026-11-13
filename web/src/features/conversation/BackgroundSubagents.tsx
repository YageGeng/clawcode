import { Bot, CircleAlert, Square } from "lucide-react";
import { useMemo, useState } from "react";
import type { SessionId } from "../../acp/protocol";
import { AgentMails } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";
import { useWorkspaceStore } from "../../workspace/store";

/**
 * Shows persistent background children as a slim pill strip. Their running
 * state is polled by the shell, which stays mounted when no Session is open.
 */
export function BackgroundSubagents({ sessionId, controller }: Readonly<{ sessionId: SessionId; controller: WorkspaceController }>) {
  const sessions = useWorkspaceStore((state) => state.sessions);
  const running = useWorkspaceStore((state) => state.runningSessionIds);
  // The ownership edge is persisted in the header, so nicknames are known before the title fact is listed.
  const children = useMemo(() => sessions.flatMap((session) => session.ownership?.sessionId === sessionId
    ? [{ sessionId: session.sessionId, path: AgentMails.path(session.ownership), nickname: session.ownership.nickname }]
    : []), [sessions, sessionId]);
  const [error, setError] = useState<string>();
  if (children.length === 0) return null;
  return <section className="background-subagents" aria-label="子 agent">
    <span className="background-subagents__label"><Bot size={13} aria-hidden="true" />子 agent<b>{children.length}</b></span>
    <ul className="background-subagents__list">
      {children.map((child) => {
        const active = running.has(child.sessionId);
        return <li key={child.sessionId} className="subagent-pill" data-running={active}>
          <button className="subagent-pill__open" type="button" title={child.path} onClick={() => {
            void controller.openSession(child.sessionId).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)));
          }}>
            <span className="subagent-pill__dot" aria-hidden="true" />
            <span className="subagent-pill__name">{child.nickname}</span>
            {/* Idle pills stay quiet visually; the state remains announced to assistive technology. */}
            <span className={active ? "subagent-pill__state" : "visually-hidden"}>{active ? "运行中" : "空闲"}</span>
          </button>
          {active ? <button className="subagent-pill__stop" type="button" aria-label={`停止 ${child.nickname}`} title="停止" onClick={() => controller.cancel(child.sessionId)}>
            <Square size={9} fill="currentColor" aria-hidden="true" />
          </button> : null}
        </li>;
      })}
    </ul>
    {error === undefined ? null : <span className="background-subagents__error" role="alert" title={error}><CircleAlert size={12} aria-hidden="true" />打开失败</span>}
  </section>;
}
