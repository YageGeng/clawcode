import { useMemo } from "react";

import type { TerminalEntity } from "../../workspace/terminals";
import { displayText } from "../../workspace/terminals";

export type TerminalViewProps = Readonly<{ terminal: TerminalEntity }>;

/** Shows one terminal's command, plain output, and exit; never links or answers queries. */
export function TerminalView({ terminal }: TerminalViewProps) {
  const text = useMemo(() => displayText(terminal.output), [terminal.output]);
  const exit = terminal.exitStatus;
  return (
    <div className="terminal-view">
      {terminal.command === undefined ? null : <div className="terminal-view__command">$ {terminal.command}</div>}
      <pre className="terminal-view__output">{text.length === 0 ? "(no output)" : text}</pre>
      {exit === undefined ? null : (
        <div className="terminal-view__exit">
          {exit.signal !== undefined ? `信号 ${exit.signal}` : exit.exitCode !== undefined ? `退出码 ${exit.exitCode}` : "已结束"}
          {terminal.cwd === undefined ? null : <span title={terminal.cwd}> · {terminal.cwd}</span>}
        </div>
      )}
    </div>
  );
}
