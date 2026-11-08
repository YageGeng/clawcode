import { ChevronRight, GitBranch } from "lucide-react";

import type { BranchSummaryEntity } from "../../domain/model";
import { TranscriptRowKeys } from "../../domain/model";
import { MarkdownContent } from "./MarkdownContent";

export type BranchSummaryCardProps = Readonly<{
  branchSummary: BranchSummaryEntity;
}>;

/** Renders the summary of an abandoned branch as a collapsed, replay-safe transcript card. */
export function BranchSummaryCard({ branchSummary }: BranchSummaryCardProps) {
  // Reuses the compaction card styles: both are collapsed context checkpoints.
  return (
    <details className="compaction-card" data-transcript-key={TranscriptRowKeys.branchSummary(branchSummary.messageId)}>
      <summary>
        <span className="compaction-card__icon"><GitBranch size={16} /></span>
        <span className="compaction-card__heading">
          <strong>分支摘要</strong>
          <small>已离开分支的摘要 · 点击展开</small>
        </span>
        <ChevronRight className="compaction-card__chevron" size={16} />
      </summary>
      <div className="compaction-card__details">
        <div className="compaction-card__summary"><MarkdownContent text={branchSummary.summary} /></div>
      </div>
    </details>
  );
}
