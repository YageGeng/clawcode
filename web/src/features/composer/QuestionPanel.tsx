import { ChevronLeft, ChevronRight, MessageCircleQuestion } from "lucide-react";
import { useMemo, useState } from "react";

import type { SessionId } from "../../acp/protocol";
import type { OpenQuestion } from "../../domain/model";
import { Questions } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";
import { openQuestions } from "../../workspace/selectors";
import { sessionWorkspace } from "../../workspace/state";
import { useWorkspaceStore } from "../../workspace/store";
import { MarkdownContent } from "../conversation/MarkdownContent";

export type QuestionPanelProps = Readonly<{ sessionId: SessionId; controller: WorkspaceController }>;

/** The chosen suggestion's position, or the free-text answer. */
type Choice = number | "other";

/**
 * Shows the questions the model asked without waiting, one at a time, above
 * the composer. Arriving questions never take focus.
 */
export function QuestionPanel({ sessionId, controller }: QuestionPanelProps) {
  // Subscribed field by field: the session object changes on every streamed token.
  const transcript = useWorkspaceStore((state) => sessionWorkspace(state, sessionId).transcript);
  const tools = useWorkspaceStore((state) => sessionWorkspace(state, sessionId).tools);
  const extensions = useWorkspaceStore((state) => sessionWorkspace(state, sessionId).extensions);
  const pending = useWorkspaceStore((state) => sessionWorkspace(state, sessionId).pending);
  const settlingQuestions = useWorkspaceStore((state) => sessionWorkspace(state, sessionId).settlingQuestions);
  const open = useMemo(
    () => openQuestions({ transcript, tools, extensions, pending, settlingQuestions }),
    [transcript, tools, extensions, pending, settlingQuestions]
  );
  const [position, setPosition] = useState(0);
  const index = Math.min(position, open.length - 1);
  const current = open[index];
  if (current === undefined) return null;

  return (
    <section className="question-panel" aria-label="待回答的问题">
      <header className="question-panel__header">
        <MessageCircleQuestion size={14} aria-hidden="true" />
        <span className="question-panel__title">问题</span>
        <span className="question-panel__progress">第 {index + 1} 题 / 共 {open.length} 题</span>
        <button type="button" aria-label="上一题" disabled={index === 0} onClick={() => setPosition(index - 1)}><ChevronLeft size={14} aria-hidden="true" /></button>
        <button type="button" aria-label="下一题" disabled={index === open.length - 1} onClick={() => setPosition(index + 1)}><ChevronRight size={14} aria-hidden="true" /></button>
      </header>
      {/* Keyed by question, so each question starts from its own saved draft. */}
      <QuestionEditor key={Questions.id(current.key)} sessionId={sessionId} controller={controller} open={current} />
    </section>
  );
}

/**
 * Answers one question: the first suggestion is preselected but sent only on
 * 发送; 其他 takes a free-text answer that survives reloads in session storage.
 * Multi-select questions use checkboxes and send the picks as a Markdown list.
 */
function QuestionEditor({ sessionId, controller, open }: Readonly<{ sessionId: SessionId; controller: WorkspaceController; open: OpenQuestion }>) {
  const options = open.question.options ?? [];
  const multi = open.question.multiSelect === true;
  const draftKey = Questions.draftKey(controller.bootstrap.product.slug, sessionId, open.key);
  const [text, setText] = useState(() => sessionStorage.getItem(draftKey) ?? "");
  const [picked, setPicked] = useState<ReadonlySet<Choice>>(() => new Set([text.length > 0 || options.length === 0 ? "other" : 0]));
  const [error, setError] = useState<string>();
  // Picks in display order, free text last; an empty 其他 contributes nothing.
  const picks = [...options.filter((_, position) => picked.has(position)), ...(picked.has("other") && text.trim().length > 0 ? [text] : [])];
  const answer = multi ? picks.map((pick) => `- ${pick}`).join("\n") : picks[0] ?? "";
  const name = `question-${Questions.id(open.key)}`;

  // A radio replaces the pick; a checkbox toggles it.
  const toggle = (choice: Choice) => setPicked((current) => {
    if (!multi) return new Set([choice]);
    const next = new Set(current);
    if (!next.delete(choice)) next.add(choice);
    return next;
  });

  const edit = (value: string) => {
    setText(value);
    sessionStorage.setItem(draftKey, value);
  };
  const send = () => {
    try {
      controller.answerQuestion(sessionId, open.key, answer);
      sessionStorage.removeItem(draftKey);
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };
  const skip = () => {
    // The editor unmounts once the skip is in flight; the controller reports failures.
    void controller.dismissQuestions(sessionId, [open.key]).catch(() => undefined);
  };

  return <>
    <div className="question-panel__question"><MarkdownContent text={open.question.title} /></div>
    {multi ? <p className="question-panel__hint">可多选</p> : null}
    <div className="question-panel__options" role={multi ? "group" : "radiogroup"} aria-label="回答">
      {options.map((option, position) => (
        <label key={position}>
          <input type={multi ? "checkbox" : "radio"} name={name} checked={picked.has(position)} onChange={() => toggle(position)} />
          <span>{option}</span>
        </label>
      ))}
      <label>
        <input type={multi ? "checkbox" : "radio"} name={name} checked={picked.has("other")} onChange={() => toggle("other")} />
        <span>其他</span>
      </label>
    </div>
    {picked.has("other") ? <textarea className="question-panel__other" aria-label="其他回答" rows={2} value={text} onChange={(event) => edit(event.target.value)} /> : null}
    {error === undefined ? null : <p className="question-panel__error" role="alert">{error}</p>}
    <div className="question-panel__actions">
      <button type="button" onClick={skip}>跳过</button>
      <button type="button" className="question-panel__send" disabled={answer.trim().length === 0} onClick={send}>发送</button>
    </div>
  </>;
}
