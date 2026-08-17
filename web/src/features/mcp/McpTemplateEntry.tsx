import { useState } from "react";

import type { McpResourceTemplateInfo } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";

export type McpTemplateEntryProps = Readonly<{
  template: McpResourceTemplateInfo;
  controller: WorkspaceController;
}>;

/** Completes one named Resource Template argument through the remote Server. */
export function McpTemplateEntry({ template, controller }: McpTemplateEntryProps) {
  const [argumentName, setArgumentName] = useState("");
  const [argumentValue, setArgumentValue] = useState("");
  const [values, setValues] = useState<readonly string[]>([]);
  const [error, setError] = useState<string>();

  /** Requests Completion while preserving the original URI template route. */
  const complete = async () => {
    setError(undefined);
    try {
      const result = await controller.completeMcpArgument({
        target: { type: "resourceTemplate", reference: { serverId: template.serverId, uriTemplate: template.uriTemplate } },
        argumentName,
        argumentValue,
        context: {}
      });
      setValues(result.values);
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  return (
    <details>
      <summary><span>Template</span>{template.title ?? template.name}</summary>
      <p>{template.uriTemplate}</p>
      <div className="mcp-catalog__form">
        <label>Argument name<input value={argumentName} onChange={(event) => setArgumentName(event.target.value)} /></label>
        <label>Partial value<input value={argumentValue} onChange={(event) => setArgumentValue(event.target.value)} /></label>
        <button disabled={argumentName.trim().length === 0} type="button" onClick={() => void complete()}>Complete</button>
        {values.map((value) => <button className="mcp-candidate" key={value} type="button" onClick={() => setArgumentValue(value)}>{value}</button>)}
      </div>
      {error === undefined ? null : <div className="mcp-card__error">{error}</div>}
    </details>
  );
}
