import { CornerDownRight, SkipForward } from "lucide-react";

import type { SessionId } from "../../acp/protocol";
import type { ExtensionEntity, QuestionSettlement, UserQuestions } from "../../domain/model";
import { Questions, TranscriptRowKeys } from "../../domain/model";
import { questionSettlements, sessionWorkspaceOf } from "../../workspace/selectors";
import { useWorkspaceStore } from "../../workspace/store";
import { MarkdownContent } from "./MarkdownContent";

/** Lists each question a call asked with whether it was answered or skipped. */
export function AskedQuestions({ sessionId, asked }: Readonly<{ sessionId: SessionId; asked: UserQuestions }>) {
  const extensions = useWorkspaceStore((state) => sessionWorkspaceOf(state, sessionId)?.extensions);
  const settlements = extensions === undefined ? undefined : questionSettlements(extensions);
  return (
    <section className="asked-questions" aria-label="问题">
      <h4>问题</h4>
      <ol>
        {asked.questions.map((question, index) => {
          const settlement = settlements?.get(Questions.id({ requestId: asked.requestId, index }));
          // Multi-select answers are Markdown lists; the one-line status joins their items.
          const status = settlement === undefined ? "待回答" : settlement.answer === undefined ? "已跳过" : `已回答：${settlement.answer.replace(/^- /gm, "").split("\n").join("、")}`;
          return (
            <li key={index} data-settled={settlement !== undefined}>
              <MarkdownContent text={question.title} />
              <span className="asked-questions__status">{status}</span>
            </li>
          );
        })}
      </ol>
    </section>
  );
}

/** Renders one answer or skip record as the user's own message. */
export function QuestionReplyCard({ extension }: Readonly<{ extension: ExtensionEntity }>) {
  const settlements = Questions.settlements(extension.details);
  const answered = settlements.filter((settlement): settlement is QuestionSettlement & { answer: string } => settlement.answer !== undefined);
  if (answered.length === 0) {
    return (
      <p className="question-reply question-reply--skipped" data-transcript-key={TranscriptRowKeys.extension(extension.id)}>
        <SkipForward size={12} aria-hidden="true" />已跳过 {settlements.length} 个问题
      </p>
    );
  }
  return (
    <article className="question-reply" data-transcript-key={TranscriptRowKeys.extension(extension.id)}>
      <div className="question-reply__label">你的回答</div>
      {answered.map((settlement) => (
        <div className="question-reply__item" key={Questions.id(settlement.key)}>
          {/* Records without a stored title show the answer alone, not an empty line. */}
          {settlement.question.length === 0 ? null : <p className="question-reply__question" title={settlement.question}>
            <CornerDownRight size={12} aria-hidden="true" />
            <span>{settlement.question}</span>
          </p>}
          <div className="question-reply__answer"><MarkdownContent text={settlement.answer} /></div>
        </div>
      ))}
    </article>
  );
}
