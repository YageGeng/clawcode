import { useState } from "react";

import type { McpPromptInfo, McpPromptResult } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";

export type McpPromptEntryProps = Readonly<{
  prompt: McpPromptInfo;
  controller: WorkspaceController;
}>;

/** Retrieves one remote Prompt and offers server-backed argument completion. */
export function McpPromptEntry({ prompt, controller }: McpPromptEntryProps) {
  const [arguments_, setArguments] = useState<Record<string, string>>({});
  const [result, setResult] = useState<McpPromptResult>();
  const [candidates, setCandidates] = useState<Readonly<Record<string, readonly string[]>>>({});
  const [error, setError] = useState<string>();

  /** Retrieves the Prompt with the current explicit argument values. */
  const retrieve = async () => {
    setError(undefined);
    try {
      setResult(await controller.getMcpPrompt(prompt.reference.serverId, prompt.reference.remoteName, arguments_));
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  /** Requests remote Completion for one Prompt argument and retains candidate order. */
  const complete = async (argumentName: string) => {
    setError(undefined);
    try {
      const completion = await controller.completeMcpArgument({
        target: { type: "prompt", reference: prompt.reference },
        argumentName,
        argumentValue: arguments_[argumentName] ?? "",
        context: arguments_
      });
      setCandidates((current) => ({ ...current, [argumentName]: completion.values }));
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  return (
    <details>
      <summary><span>Prompt</span>{prompt.title ?? prompt.reference.remoteName}</summary>
      {prompt.description === undefined ? null : <p>{prompt.description}</p>}
      <div className="mcp-catalog__form">
        {prompt.arguments.map((argument) => (
          <label key={argument.name}>
            {argument.name}{argument.required ? " *" : ""}
            <span><input value={arguments_[argument.name] ?? ""} onChange={(event) => setArguments((current) => ({ ...current, [argument.name]: event.target.value }))} /><button type="button" onClick={() => void complete(argument.name)}>Complete</button></span>
            {(candidates[argument.name] ?? []).map((candidate) => <button className="mcp-candidate" key={candidate} type="button" onClick={() => setArguments((current) => ({ ...current, [argument.name]: candidate }))}>{candidate}</button>)}
          </label>
        ))}
        <button type="button" onClick={() => void retrieve()}>Get Prompt</button>
      </div>
      {error === undefined ? null : <div className="mcp-card__error">{error}</div>}
      {result === undefined ? null : <pre>{JSON.stringify(result, null, 2)}</pre>}
    </details>
  );
}
