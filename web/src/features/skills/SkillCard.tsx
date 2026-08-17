import { Play } from "lucide-react";
import { useState } from "react";

import type { SkillInfo } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";

const SOURCE_LABELS = {
  configured: "Configured",
  project: "Project",
  user: "User",
  extension: "Extension"
} as const;

export type SkillCardProps = Readonly<{
  skill: SkillInfo;
  hasSession: boolean;
  controller: WorkspaceController;
}>;

/** Displays one Skill and owns its independent explicit-invocation form state. */
export function SkillCard({ skill, hasSession, controller }: SkillCardProps) {
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  return (
    <article className="capability-card skill-card">
      <div className="skill-card__heading">
        <h3>{skill.name}</h3>
        <div className="skill-card__badges">
          <span className="capability-badge" data-scope={skill.source.scope}>{SOURCE_LABELS[skill.source.scope]}</span>
          {skill.disableModelInvocation ? <span className="capability-badge" data-tone="manual">仅手动调用</span> : null}
        </div>
      </div>
      <p>{skill.description}</p>
      <dl className="skill-card__paths">
        <div><dt>Skill</dt><dd title={skill.path}>{skill.path}</dd></div>
        <div><dt>References</dt><dd title={skill.referenceDir}>{skill.referenceDir}</dd></div>
      </dl>
      <textarea
        aria-label={`${skill.name} 请求`}
        placeholder="给这个 Skill 的请求（可选）"
        value={input}
        onChange={(event) => setInput(event.target.value)}
      />
      {error === undefined ? null : <div className="form-error" role="alert">{error}</div>}
      <button
        className="primary-button"
        type="button"
        disabled={!hasSession || busy}
        onClick={() => {
          setBusy(true);
          setError(undefined);
          const arguments_ = input.trim();
          void controller.send({
            text: `/skill:${skill.name}${arguments_.length === 0 ? "" : ` ${arguments_}`}`,
            resources: []
          })
            .catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)))
            .finally(() => setBusy(false));
        }}
      >
        <Play size={13} />{busy ? "调用中…" : "调用并发送"}
      </button>
    </article>
  );
}
