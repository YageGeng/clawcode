import { ArchiveRestore, ChevronRight } from "lucide-react";

import type { CompactionEntity } from "../../domain/model";
import { MarkdownContent } from "./MarkdownContent";

export type CompactionCardProps = Readonly<{
  compaction: CompactionEntity;
}>;

/** Renders one persisted context checkpoint as a collapsed, replay-safe transcript card. */
export function CompactionCard({ compaction }: CompactionCardProps) {
  const tokens = BigInt(compaction.tokensBefore).toLocaleString();
  const elapsed = BigInt(compaction.endedAtMs) - BigInt(compaction.startedAtMs);
  const reason = compaction.reason === "manual" ? "手动" : compaction.reason === "threshold" ? "达到上下文阈值" : "上下文溢出恢复";

  return (
    <details className="compaction-card">
      <summary>
        <span className="compaction-card__icon"><ArchiveRestore size={16} /></span>
        <span className="compaction-card__heading">
          <strong>上下文已压缩</strong>
          <small>从 {tokens} tokens 压缩 · 点击展开</small>
        </span>
        <ChevronRight className="compaction-card__chevron" size={16} />
      </summary>
      <div className="compaction-card__details">
        <div className="compaction-card__meta">{reason} · {elapsed.toLocaleString()} ms · Turn {compaction.turnId}</div>
        <div className="compaction-card__summary"><MarkdownContent text={compaction.summary} /></div>
      </div>
    </details>
  );
}
