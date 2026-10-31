import { Gauge } from "lucide-react";

import type { RunId } from "../../acp/protocol";
import type { ModelUsage } from "../../domain/model";
import { ModelUsages } from "../../domain/model";

export type AgentUsageProps = Readonly<{
  runId: RunId;
  usage: ModelUsage;
}>;

/** Renders token accounting for one complete Agent Run. */
export function AgentUsage({ runId, usage }: AgentUsageProps) {
  const metrics = [
    ["总计", usage.totalTokens],
    ["输入", usage.inputTokens],
    ["输出", usage.outputTokens],
    ["缓存读取", usage.cacheReadTokens],
    ["缓存写入", usage.cacheWriteTokens],
    ...(usage.reasoningTokens === undefined ? [] : [["推理", usage.reasoningTokens]])
  ] as const;

  return (
    <div
      className="agent-usage"
      role="group"
      aria-label={`Agent Run ${runId} Token 用量`}
    >
      <div className="agent-usage__heading">
        <Gauge size={14} strokeWidth={1.8} aria-hidden="true" />
        <span>Agent Usage</span>
      </div>
      <dl className="agent-usage__metrics">
        {metrics.map(([label, value]) => (
          <div className="agent-usage__metric" key={label}>
            <dt>{label}</dt>
            <dd>{ModelUsages.formatTokens(value)}</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}
