import { useState } from "react";

export type NewSessionDialogProps = Readonly<{
  defaultCwd: string;
  onCancel: () => void;
  onCreate: (cwd: string) => Promise<void>;
}>;

export function NewSessionDialog({ defaultCwd, onCancel, onCreate }: NewSessionDialogProps) {
  const [cwd, setCwd] = useState(defaultCwd);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string>();

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
      <section className="dialog" role="dialog" aria-modal="true" aria-labelledby="new-session-title">
        <h2 id="new-session-title">新建会话</h2>
        <p>Agent 将以这个本机目录作为会话工作区。</p>
        <label className="field-label" htmlFor="session-cwd">工作目录</label>
        <input
          id="session-cwd"
          className="field-input"
          autoFocus
          value={cwd}
          onChange={(event) => setCwd(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !submitting) void submit();
            if (event.key === "Escape" && !submitting) onCancel();
          }}
        />
        {error === undefined ? null : <div className="form-error" role="alert">{error}</div>}
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
