import { SquareTerminal, Trash2, X } from "lucide-react";
import { useState } from "react";

import type { TerminalSnapshot } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";
import { sessionWorkspaceOf } from "../../workspace/selectors";
import { useWorkspaceStore } from "../../workspace/store";

export type TerminalPanelProps = Readonly<{
  controller: WorkspaceController;
}>;

const EMPTY_TERMINALS: readonly TerminalSnapshot[] = [];

/** Formats one protocol millisecond timestamp for compact host inspection. */
function terminalTime(value: string): string {
  const timestamp = Number(value);
  return Number.isSafeInteger(timestamp)
    ? new Date(timestamp).toLocaleString()
    : value;
}

/** Renders retained Session terminals with idempotent host cleanup controls. */
export function TerminalPanel({ controller }: TerminalPanelProps) {
  const sessionId = useWorkspaceStore((state) => state.activeSessionId);
  const terminals = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.terminals ?? EMPTY_TERMINALS);
  const [busyTerminalId, setBusyTerminalId] = useState<number | undefined>();
  const [cleaning, setCleaning] = useState(false);

  /** Terminates one running terminal while preventing duplicate host requests. */
  async function terminate(terminalId: number): Promise<void> {
    if (sessionId === undefined || cleaning || busyTerminalId !== undefined) return;
    setBusyTerminalId(terminalId);
    try {
      await controller.terminateTerminal(sessionId, terminalId);
    } catch (reason: unknown) {
      controller.reportSessionDiagnostic(
        sessionId,
        `终止终端 #${terminalId} 失败：${reason instanceof Error ? reason.message : String(reason)}`
      );
    } finally {
      setBusyTerminalId(undefined);
    }
  }

  /** Cleans every retained terminal while disabling conflicting controls. */
  async function clean(): Promise<void> {
    if (sessionId === undefined || cleaning || busyTerminalId !== undefined) return;
    setCleaning(true);
    try {
      await controller.cleanTerminals(sessionId);
    } catch (reason: unknown) {
      controller.reportSessionDiagnostic(
        sessionId,
        `清理后台终端失败：${reason instanceof Error ? reason.message : String(reason)}`
      );
    } finally {
      setCleaning(false);
    }
  }

  if (sessionId === undefined || terminals.length === 0) {
    return <div className="inspector-empty">当前会话没有后台终端。</div>;
  }
  return <div className="terminal-panel">
    <div className="terminal-panel__toolbar">
      <span>{terminals.length} 个终端</span>
      <button type="button" disabled={cleaning || busyTerminalId !== undefined} onClick={() => void clean()}><Trash2 size={13} />{cleaning ? "清理中…" : "全部清理"}</button>
    </div>
    {terminals.map((terminal) => <article className="terminal-panel__item" key={terminal.terminalId}>
      <div className="terminal-panel__heading">
        <SquareTerminal size={15} aria-hidden="true" />
        <strong>#{terminal.terminalId}</strong>
        <span data-status={terminal.status}>{terminal.status}</span>
        {terminal.status === "running" ? <button aria-label={`终止终端 ${terminal.terminalId}`} title="终止" type="button" disabled={cleaning || busyTerminalId !== undefined} onClick={() => void terminate(terminal.terminalId)}><X size={13} /></button> : null}
      </div>
      <code title={terminal.command}>{terminal.command}</code>
      <small>{terminal.tty ? "PTY" : "Pipe"} · {terminal.cwd}{terminal.status !== "exited" || terminal.exitCode === undefined ? "" : terminal.exitCode === null ? " · exit signal" : ` · exit ${terminal.exitCode}`} · 最近活动 {terminalTime(terminal.lastActivityAt)}</small>
    </article>)}
  </div>;
}
