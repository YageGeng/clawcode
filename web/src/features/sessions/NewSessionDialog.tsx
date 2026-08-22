import { useState } from "react";

import { useModalFocus } from "../../shell/useModalFocus";

export type NewSessionDialogProps = Readonly<{
  defaultCwd: string;
  onCancel: () => void;
  onCreate: (cwd: string) => Promise<void>;
}>;

export function NewSessionDialog({ defaultCwd, onCancel, onCreate }: NewSessionDialogProps) {
  const [cwd, setCwd] = useState(defaultCwd);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string>();
  const dialogRef = useModalFocus(onCancel, submitting);

  const submit = async () => {
    const value = cwd.trim();
    if (value.length === 0 || !value.startsWith("/")) {
      setError("请输入绝对工作目录路径。");
      return;
    }
    setSubmitting(true);
    setError(undefined);
    try {
      await onCreate(value);
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="dialog-backdrop" role="presentation" onMouseDown={(event) => {
      if (event.target === event.currentTarget && !submitting) onCancel();
    }}>
      <section ref={dialogRef} className="dialog" role="dialog" tabIndex={-1} aria-modal="true" aria-describedby="new-session-description" aria-labelledby="new-session-title">
        <h2 id="new-session-title">新建会话</h2>
        <p id="new-session-description">Agent 将以这个本机目录作为会话工作区。</p>
        <label className="field-label" htmlFor="session-cwd">工作目录</label>
        <input
          id="session-cwd"
          className="field-input"
          autoFocus
          aria-describedby={error === undefined ? "new-session-description" : "new-session-error"}
          aria-invalid={error !== undefined}
          value={cwd}
          onChange={(event) => setCwd(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !submitting) void submit();
          }}
        />
        {error === undefined ? null : <div className="form-error" id="new-session-error" role="alert">{error}</div>}
        <div className="dialog__actions">
          <button className="secondary-button" type="button" disabled={submitting} onClick={onCancel}>取消</button>
          <button className="primary-button" type="button" disabled={submitting} onClick={() => void submit()}>
            {submitting ? "创建中…" : "创建"}
          </button>
        </div>
      </section>
    </div>
  );
}
