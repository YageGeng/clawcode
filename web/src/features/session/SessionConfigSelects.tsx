import { useState } from "react";

import type { ConfigSelect, SessionId } from "../../acp/protocol";
import type { WorkspaceController } from "../../workspace/controller";

export type SessionConfigSelectsProps = Readonly<{
  sessionId: SessionId;
  options: readonly ConfigSelect[];
  controller: WorkspaceController;
}>;

/** Compact header selects for a Session's model and thinking level. */
export function SessionConfigSelects({ sessionId, options, controller }: SessionConfigSelectsProps) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  /** Sends one change; selects stay disabled until the agent answers. */
  const change = async (configId: string, value: string) => {
    setBusy(true);
    setError(undefined);
    try {
      await controller.setConfigOption(sessionId, configId, value);
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  };

  return (
    <span className="session-config">
      {options.map((option) => (
        <select
          key={option.configId}
          aria-label={option.name}
          title={option.options.find((item) => item.value === option.currentValue)?.description ?? option.name}
          disabled={busy}
          value={option.currentValue}
          onChange={(event) => void change(option.configId, event.target.value)}
        >
          {option.options.map((item) => <option key={item.value} value={item.value}>{item.name}</option>)}
        </select>
      ))}
      {error === undefined ? null : <span className="session-config__error" role="alert" title={error}>切换失败</span>}
    </span>
  );
}
