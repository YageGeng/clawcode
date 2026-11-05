import { KeyRound } from "lucide-react";
import { useState } from "react";

import type { ElicitationPrompt } from "../../domain/model";
import { useModalFocus } from "../../shell/useModalFocus";
import type { WorkspaceController } from "../../workspace/controller";

export type SignInDialogProps = Readonly<{
  prompt: ElicitationPrompt;
  login: NonNullable<ElicitationPrompt["login"]>;
  controller: WorkspaceController;
}>;

const COPIED_NOTICE_MS = 1_500;

/**
 * Shows a sign-in's device code until the agent reports the outcome.
 *
 * Opening the page answers nothing: the user still needs the code once it opens,
 * so only the agent closing this request or the user cancelling dismisses it.
 */
export function SignInDialog({ prompt, login, controller }: SignInDialogProps) {
  const [copied, setCopied] = useState(false);
  const dialogRef = useModalFocus(
    () => controller.answerElicitation(prompt.key, { action: "cancel" }),
    false,
    // Never land on the button that opens an external page.
    (modal) => modal.querySelector<HTMLButtonElement>("button[data-safe-focus]")
  );

  /** Copies the code; if the browser refuses, the code stays selectable by hand. */
  const copy = () => {
    void navigator.clipboard.writeText(login.userCode).then(() => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), COPIED_NOTICE_MS);
    }, () => undefined);
  };

  /** Opens the verification page without dismissing the code. */
  const openPage = () => {
    if (prompt.mode.type === "url") window.open(prompt.mode.url, "_blank", "noopener,noreferrer");
  };

  return (
    <div className="dialog-backdrop" role="presentation">
      <section ref={dialogRef} aria-describedby="sign-in-instruction" aria-labelledby="sign-in-title" aria-modal="true" className="dialog elicitation-dialog" role="dialog" tabIndex={-1}>
        <header>
          <KeyRound size={18} aria-hidden="true" />
          <div>
            <h2 id="sign-in-title">登录 {login.name}</h2>
          </div>
        </header>
        <p id="sign-in-instruction">打开验证页面，输入下面的代码：</p>
        <div className="sign-in-code">
          <code>{login.userCode}</code>
          <button className="secondary-button" type="button" onClick={copy}>{copied ? "已复制" : "复制"}</button>
        </div>
        <p className="sign-in-status" aria-live="polite">等待授权…</p>
        <footer>
          <button className="secondary-button" data-safe-focus type="button" onClick={() => controller.answerElicitation(prompt.key, { action: "cancel" })}>取消</button>
          <button className="primary-button" type="button" onClick={openPage}>打开验证页面</button>
        </footer>
      </section>
    </div>
  );
}
