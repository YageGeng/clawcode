import { ExternalLink, MessageSquareWarning } from "lucide-react";
import { useMemo, useState } from "react";

import type { ElicitationPrompt } from "../../domain/model";
import { useModalFocus } from "../../shell/useModalFocus";
import type { WorkspaceController } from "../../workspace/controller";
import { useWorkspaceStore } from "../../workspace/store";
import { ElicitationForm } from "./elicitationForm";
import type { ElicitationField, ElicitationValue } from "./elicitationForm";

export type ElicitationDialogProps = Readonly<{
  prompt: ElicitationPrompt;
  controller: WorkspaceController;
}>;

/** Renders one input control for a parsed form field. */
function FieldInput({ field, value, onChange, invalid }: Readonly<{
  field: ElicitationField;
  value: ElicitationValue | undefined;
  onChange: (value: ElicitationValue) => void;
  invalid: boolean;
}>) {
  const id = `elicitation-field-${field.name}`;
  switch (field.kind) {
    case "boolean":
      return <input id={id} type="checkbox" checked={value === true} onChange={(event) => onChange(event.target.checked)} />;
    case "select":
      return <select id={id} aria-invalid={invalid} value={typeof value === "string" ? value : ""} onChange={(event) => onChange(event.target.value)}>
        <option value="">请选择</option>
        {field.options.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
      </select>;
    case "multi": {
      const selected = typeof value === "object" ? value : [];
      return <div className="elicitation-choices" role="group" aria-labelledby={`${id}-label`}>
        {field.options.map((option) => <label key={option.value}>
          <input type="checkbox" checked={selected.includes(option.value)} onChange={(event) => onChange(event.target.checked ? [...selected, option.value] : selected.filter((item) => item !== option.value))} />
          {option.label}
        </label>)}
      </div>;
    }
    case "number":
      return <input id={id} aria-invalid={invalid} type="number" step={field.integer ? 1 : "any"} min={field.minimum} max={field.maximum} value={typeof value === "string" ? value : ""} onChange={(event) => onChange(event.target.value)} />;
    default:
      return <input id={id} aria-invalid={invalid} type={field.format === "email" ? "email" : field.format === "uri" ? "url" : field.format === "date" ? "date" : "text"} value={typeof value === "string" ? value : ""} onChange={(event) => onChange(event.target.value)} />;
  }
}

/** Asks the user for one MCP elicitation; Escape and the safe default cancel it. */
export function ElicitationDialog({ prompt, controller }: ElicitationDialogProps) {
  const form = useMemo(() => prompt.mode.type === "form" ? ElicitationForm.parse(prompt.mode.requestedSchema) : undefined, [prompt]);
  const [values, setValues] = useState(() => form?.initialValues() ?? {});
  const [errors, setErrors] = useState<Readonly<Record<string, string>>>({});
  const session = useWorkspaceStore((state) => state.sessions.find((item) => item.sessionId === prompt.sessionId));
  const dialogRef = useModalFocus(
    () => controller.answerElicitation(prompt.key, { action: "cancel" }),
    false,
    // Never land on the button that opens an external page.
    (modal) => modal.querySelector<HTMLButtonElement>("button[data-safe-focus]")
  );

  /** Validates the form, then answers accept; URL mode opens the page first. */
  const accept = () => {
    if (prompt.mode.type === "url") {
      window.open(prompt.mode.url, "_blank", "noopener,noreferrer");
      controller.answerElicitation(prompt.key, { action: "accept" });
      return;
    }
    const checked = form?.validate(values);
    if (checked?.content === undefined) {
      setErrors(checked?.errors ?? {});
      return;
    }
    controller.answerElicitation(prompt.key, { action: "accept", content: checked.content });
  };

  return (
    <div className="dialog-backdrop" role="presentation">
      <section ref={dialogRef} aria-describedby="elicitation-message" aria-labelledby="elicitation-title" aria-modal="true" className="dialog mcp-elicitation" role="dialog" tabIndex={-1}>
        <header>
          <MessageSquareWarning size={18} aria-hidden="true" />
          <div>
            <h2 id="elicitation-title">{prompt.serverId === undefined ? "MCP 请求输入" : `${prompt.serverId} 请求输入`}</h2>
            <p>会话 {session?.title ?? prompt.sessionId}</p>
          </div>
        </header>
        <p id="elicitation-message">{prompt.message}</p>
        {prompt.mode.type === "url" ? (
          <p className="elicitation-url"><ExternalLink size={13} aria-hidden="true" /> 将打开 <strong>{new URL(prompt.mode.url).host}</strong></p>
        ) : (
          <form className="elicitation-form" onSubmit={(event) => { event.preventDefault(); accept(); }}>
            {form?.fields.map((field) => (
              <div className="elicitation-field" key={field.name}>
                <label id={`elicitation-field-${field.name}-label`} htmlFor={`elicitation-field-${field.name}`}>{field.label}{field.required ? " *" : ""}</label>
                {field.description === undefined ? null : <small>{field.description}</small>}
                <FieldInput field={field} value={values[field.name]} invalid={errors[field.name] !== undefined} onChange={(value) => setValues((current) => ({ ...current, [field.name]: value }))} />
                {errors[field.name] === undefined ? null : <span className="mcp-card__error" role="alert">{errors[field.name]}</span>}
              </div>
            ))}
          </form>
        )}
        <footer>
          <button className="secondary-button" data-safe-focus type="button" onClick={() => controller.answerElicitation(prompt.key, { action: "cancel" })}>取消</button>
          <button className="danger-button" type="button" onClick={() => controller.answerElicitation(prompt.key, { action: "decline" })}>拒绝</button>
          <button className="primary-button" type="button" onClick={accept}>{prompt.mode.type === "url" ? "打开并继续" : "提交"}</button>
        </footer>
      </section>
    </div>
  );
}
