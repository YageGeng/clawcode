import { ChevronDown, CircleStop, LoaderCircle, SquareTerminal } from "lucide-react";
import { useEffect, useId, useState } from "react";

import type { SessionId } from "../../acp/protocol";
import type { ExecProcessInfo, WorkspaceController } from "../../workspace/controller";

type BackgroundTerminalsProps = Readonly<{
  sessionId: SessionId;
  count: number;
  controller: WorkspaceController;
  onError: (reason: unknown) => void;
}>;

/** Shows how many commands still run in the background and lets the user stop them. */
export function BackgroundTerminals({ sessionId, count, controller, onError }: BackgroundTerminalsProps) {
  const listId = useId();
  const [processes, setProcesses] = useState<readonly ExecProcessInfo[]>();
  const [busy, setBusy] = useState(false);
  const expanded = processes !== undefined;

  // Commands start and stop while the list is open; reload it when the count moves.
  useEffect(() => {
    if (!expanded) return;
    let current = true;
    controller.listExecProcesses(sessionId).then((listed) => {
      if (current) setProcesses(listed);
    }, onError);
    return () => {
      current = false;
    };
    // Opening already fetches through the toggle, and `onError` is a fresh closure
    // per render; only a count change while open needs a reload.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [count, sessionId, controller]);

  /** Runs one action, then reloads the authoritative list from the backend. */
  const act = async (action: () => Promise<unknown>) => {
    if (busy) return;
    setBusy(true);
    try {
      await action();
      setProcesses(await controller.listExecProcesses(sessionId));
    } catch (reason: unknown) {
      onError(reason);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="background-terminals" aria-label="后台终端" aria-busy={busy}>
      <div className="background-terminals__heading">
        <button
          className="background-terminals__toggle"
          type="button"
          aria-expanded={expanded}
          aria-controls={listId}
          onClick={() => (expanded ? setProcesses(undefined) : void act(async () => {}))}
        >
          <SquareTerminal size={15} aria-hidden="true" />
          <span className="background-terminals__count" role="status">{count} 个后台终端</span>
          <ChevronDown className="background-terminals__chevron" size={14} aria-hidden="true" />
        </button>
        <button className="background-terminals__stop-all" type="button" disabled={busy} onClick={() => void act(() => controller.cleanExecProcesses(sessionId))}>
          {busy ? <LoaderCircle className="background-terminals__spinner" size={14} aria-hidden="true" /> : <CircleStop size={14} aria-hidden="true" />}
          <span>全部停止</span>
        </button>
      </div>
      {processes === undefined ? null : (
        <ul className="background-terminals__list" id={listId}>
          {processes.map((process) => (
            <li className="background-terminals__item" key={process.processId}>
              <code>{process.processId}</code>
              <span className="background-terminals__command" title={process.cwd}>{process.command}</span>
              <button type="button" disabled={busy} onClick={() => void act(() => controller.terminateExecProcess(sessionId, process.processId))}>终止</button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
