import { CheckCircle2, Circle, ClipboardList, LoaderCircle } from "lucide-react";

import type { SessionId } from "../../acp/protocol";
import type { PlanTodo } from "../../domain/model";
import { sessionWorkspace } from "../../workspace/state";
import { useWorkspaceStore } from "../../workspace/store";
import { MarkdownContent } from "../conversation/MarkdownContent";

export type PlanPanelProps = Readonly<{ sessionId: SessionId }>;

/** Picks the icon for a todo status; statuses this client does not know read as pending. */
function TodoIcon({ status }: Readonly<{ status: string }>) {
  if (status === "completed") return <CheckCircle2 size={14} aria-hidden="true" />;
  if (status === "in_progress") return <LoaderCircle className="spin" size={14} aria-hidden="true" />;
  return <Circle size={14} aria-hidden="true" />;
}

/** Shows the agent's todo list and the plan the user approved, above the composer. */
export function PlanPanel({ sessionId }: PlanPanelProps) {
  const plans = useWorkspaceStore((state) => sessionWorkspace(state, sessionId).plans);
  const todos: readonly PlanTodo[] = plans.todos ?? [];
  if (todos.length === 0 && plans.approvedPlan === undefined) return null;
  const done = todos.filter((todo) => todo.status === "completed").length;

  return (
    <details className="plan-panel" open>
      <summary>
        <ClipboardList size={14} aria-hidden="true" />
        <span>计划</span>
        {todos.length === 0 ? null : <span className="plan-panel__progress">{done}/{todos.length}</span>}
      </summary>
      {todos.length === 0 ? null : (
        <ol className="plan-panel__todos">
          {todos.map((todo, index) => (
            <li key={index} data-status={todo.status === "completed" || todo.status === "in_progress" ? todo.status : "pending"}>
              <TodoIcon status={todo.status} />
              <span>{todo.content}</span>
            </li>
          ))}
        </ol>
      )}
      {plans.approvedPlan === undefined ? null : (
        <details className="plan-panel__approved">
          <summary>已批准的计划</summary>
          <MarkdownContent text={plans.approvedPlan} />
        </details>
      )}
    </details>
  );
}
