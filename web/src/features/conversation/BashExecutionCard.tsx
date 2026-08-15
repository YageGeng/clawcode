import { Ban, Check, ChevronDown, Terminal } from "lucide-react";

import type { BashExecutionEntity } from "../../domain/model";

export type BashExecutionCardProps = Readonly<{
  bash: BashExecutionEntity;
}>;

/** Renders one persisted server-side Bash execution with timing diagnostics. */
export function BashExecutionCard({ bash }: BashExecutionCardProps) {
  const blocked = bash.disposition.type === "blocked";
  const failed = bash.cancelled || (bash.exitCode !== undefined && bash.exitCode !== 0);
  const elapsed = BigInt(bash.endedAtMs) - BigInt(bash.startedAtMs);
  const status = blocked ? "已阻止" : bash.cancelled ? "已取消" : failed ? `退出 ${bash.exitCode ?? "signal"}` : "完成";
  const icon = blocked ? <Ban size={14} /> : failed ? <Terminal size={14} /> : <Check size={14} />;
  return (
    <details className="tool-card bash-card" data-status={blocked ? "blocked" : failed ? "failed" : "completed"} open>
      <summary>
        <span className="tool-card__icon">{icon}</span>
        <span className="tool-card__title">$ {bash.command}</span>
        <span className="tool-card__status">{status} · {elapsed} ms</span>
        <ChevronDown className="tool-card__chevron" size={14} aria-hidden="true" />
      </summary>
      <div className="tool-card__details">
        <section><h4>输出</h4><pre>{bash.output.length === 0 ? "(no output)" : bash.output}</pre></section>
        <dl className="diagnostics-list">
          <dt>MessageId</dt><dd>{bash.messageId}</dd>
          <dt>TurnId</dt><dd>{bash.turnId}</dd>
          <dt>TimestampMs</dt><dd>{bash.timestampMs}</dd>
          <dt>StartedAtMs</dt><dd>{bash.startedAtMs}</dd>
          <dt>EndedAtMs</dt><dd>{bash.endedAtMs}</dd>
          <dt>Context</dt><dd>{bash.excludeFromContext ? "excluded" : "included"}</dd>
          {bash.disposition.type === "blocked" ? <><dt>Reason</dt><dd>{bash.disposition.reason}</dd></> : null}
          {bash.fullOutputPath === undefined ? null : <><dt>Full output</dt><dd>{bash.fullOutputPath}</dd></>}
        </dl>
      </div>
    </details>
  );
}
