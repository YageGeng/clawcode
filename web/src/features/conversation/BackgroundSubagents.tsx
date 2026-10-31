import { Bot, CircleAlert, Square } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import type { SessionId } from "../../acp/protocol";
import type { WorkspaceController } from "../../workspace/controller";
import { useWorkspaceStore } from "../../workspace/store";

/** Shows persistent background children as a slim pill strip and refreshes their lightweight runtime state. */
export function BackgroundSubagents({ sessionId, controller }: Readonly<{ sessionId: SessionId; controller: WorkspaceController }>) {
  const sessions = useWorkspaceStore((state) => state.sessions);
  const running = useWorkspaceStore((state) => state.runningSessionIds);
  const connection = useWorkspaceStore((state) => state.connection.type);
  // The ownership name is persisted in the header, so it is available even before the title fact is listed.
  const children = useMemo(() => sessions.flatMap((session) => session.ownership?.kind === "background" && session.ownership.sessionId === sessionId
    ? [{ sessionId: session.sessionId, name: session.ownership.name }]
    : []), [sessions, sessionId]);
  const ids = useMemo(() => children.map((child) => child.sessionId), [children]);
  const [error, setError] = useState<string>();
  useEffect(() => {
    if (ids.length === 0 || connection !== "ready") return;
    let stopped = false;
    let timer: number | undefined;
    /** Schedules only after completion so slow requests never overlap polling rounds. */
    const refresh = async () => {
      let keepPolling = true;
      try {
        keepPolling = await controller.refreshSessionActivity(ids);
        if (!stopped) setError(undefined);
      }
      catch (reason: unknown) { if (!stopped) setError(reason instanceof Error ? reason.message : String(reason)); }
      if (!stopped && keepPolling) timer = window.setTimeout(() => { void refresh(); }, 2_000);
    };
    void refresh();
    return () => { stopped = true; window.clearTimeout(timer); };
  }, [connection, controller, ids]);
  if (children.length === 0) return null;
  return <section className="background-subagents" aria-label="后台子会话">
    <span className="background-subagents__label"><Bot size={13} aria-hidden="true" />后台<b>{children.length}</b></span>
    <ul className="background-subagents__list">
      {children.map((child) => {
        const active = running.has(child.sessionId);
        return <li key={child.sessionId} className="subagent-pill" data-running={active}>
          <button className="subagent-pill__open" type="button" title={`打开 ${child.name}`} onClick={() => {
            void controller.openSession(child.sessionId).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)));
          }}>
            <span className="subagent-pill__dot" aria-hidden="true" />
            <span className="subagent-pill__name">{child.name}</span>
            {/* Idle pills stay quiet visually; the state remains announced to assistive technology. */}
            <span className={active ? "subagent-pill__state" : "visually-hidden"}>{active ? "运行中" : "空闲"}</span>
          </button>
          {active ? <button className="subagent-pill__stop" type="button" aria-label={`停止 ${child.name}`} title="停止" onClick={() => controller.cancel(child.sessionId)}>
            <Square size={9} fill="currentColor" aria-hidden="true" />
          </button> : null}
        </li>;
      })}
    </ul>
    {error === undefined ? null : <span className="background-subagents__error" role="alert" title={error}><CircleAlert size={12} aria-hidden="true" />状态刷新失败</span>}
  </section>;
}
