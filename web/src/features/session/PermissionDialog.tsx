import { ClipboardList, ShieldQuestion } from "lucide-react";

import type { PermissionPrompt } from "../../domain/model";
import { useModalFocus } from "../../shell/useModalFocus";
import type { WorkspaceController } from "../../workspace/controller";
import { MarkdownContent } from "../conversation/MarkdownContent";

export type PermissionDialogProps = Readonly<{
  prompt: PermissionPrompt;
  controller: WorkspaceController;
}>;

/** Labels for the plan-approval options the agent offers. */
const PLAN_LABELS: Readonly<Record<string, string>> = {
  keep_planning: "继续规划",
  approve: "批准并执行"
};

/**
 * Asks the user to answer one permission request; Escape dismisses it as cancelled.
 *
 * A plan approval shows the plan itself so the user decides on what will run.
 */
export function PermissionDialog({ prompt, controller }: PermissionDialogProps) {
  const dismiss = () => controller.answerPermission(prompt.key, undefined);
  const dialogRef = useModalFocus(
    dismiss,
    false,
    // Focus the refusing choice, never the one that lets work proceed.
    (modal) => modal.querySelector<HTMLButtonElement>("button[data-safe-focus]")
  );
  const isPlan = prompt.plan !== undefined;
  // Refusals come first, so the safe choice is also the first one read.
  const options = [...prompt.options].sort((left, right) => Number(!left.kind.startsWith("reject")) - Number(!right.kind.startsWith("reject")));
  const firstSafe = options.find((option) => option.kind.startsWith("reject"))?.optionId;

  return (
    <div className="dialog-backdrop" role="presentation">
      <section ref={dialogRef} aria-labelledby="permission-title" aria-modal="true" className="dialog permission-dialog" role="dialog" tabIndex={-1}>
        <header>
          {isPlan ? <ClipboardList size={18} aria-hidden="true" /> : <ShieldQuestion size={18} aria-hidden="true" />}
          <div>
            <h2 id="permission-title">{isPlan ? "执行这个计划？" : prompt.title}</h2>
          </div>
        </header>
        {prompt.description === undefined || isPlan ? null : <p>{prompt.description}</p>}
        {prompt.plan === undefined ? null : <div className="permission-plan"><MarkdownContent text={prompt.plan} /></div>}
        <footer>
          {options.map((option) => (
            <button
              key={option.optionId}
              className={option.kind.startsWith("reject") ? "secondary-button" : "primary-button"}
              data-safe-focus={option.optionId === firstSafe ? true : undefined}
              type="button"
              onClick={() => controller.answerPermission(prompt.key, option.optionId)}
            >
              {isPlan ? PLAN_LABELS[option.optionId] ?? option.name : option.name}
            </button>
          ))}
        </footer>
      </section>
    </div>
  );
}
