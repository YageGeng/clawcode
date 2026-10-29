import { useEffect, useMemo, useState } from "react";
import type { SessionId } from "../../acp/protocol";
import type { WorkspaceController } from "../../workspace/controller";
import { useWorkspaceStore } from "../../workspace/store";

/** Shows persistent background children and refreshes their lightweight runtime state. */
export function BackgroundSubagents({ sessionId, controller }: Readonly<{ sessionId: SessionId; controller: WorkspaceController }>) {
  const sessions = useWorkspaceStore((state) => state.sessions);
  const running = useWorkspaceStore((state) => state.runningSessionIds);
  const children = useMemo(() => sessions.filter((session) => session.ownership?.kind === "background" && session.ownership.sessionId === sessionId), [sessions, sessionId]);
  const ids = useMemo(() => children.map((child) => child.sessionId), [children]);
  const [error, setError] = useState<string>();
  useEffect(() => {
    if (ids.length === 0) return;
    let stopped = false;
    let timer: number | undefined;
    /** Schedules only after completion so slow requests never overlap polling rounds. */
    const refresh = async () => {
      try {
        await controller.refreshSessionActivity(ids);
        if (!stopped) setError(undefined);
      }
      catch (reason: unknown) { if (!stopped) setError(reason instanceof Error ? reason.message : String(reason)); }
      if (!stopped) timer = window.setTimeout(() => { void refresh(); }, 2_000);
    };
    void refresh();
    return () => { stopped = true; window.clearTimeout(timer); };
  }, [controller, ids]);
  if (children.length === 0) return null;
  return <section className="background-subagents" aria-label="后台子会话">
    <strong>后台 Subagents</strong>
    {children.map((child) => <div key={child.sessionId}>
      <span>{child.title}</span><span>{running.has(child.sessionId) ? "运行中" : "空闲"}</span>
      <button type="button" onClick={() => { void controller.openSession(child.sessionId).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason))); }}>打开</button>
      <button type="button" disabled={!running.has(child.sessionId)} onClick={() => controller.cancel(child.sessionId)}>停止</button>
    </div>)}
    {error === undefined ? null : <p role="alert">{error}</p>}
  </section>;
}
