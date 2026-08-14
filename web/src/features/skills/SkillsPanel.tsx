import { Play, Sparkles } from "lucide-react";
import { useState } from "react";

import type { SkillInfo } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";

export type SkillsPanelProps = Readonly<{
  skills: readonly SkillInfo[];
  hasSession: boolean;
  controller: WorkspaceController;
}>;

export function SkillsPanel({ skills, hasSession, controller }: SkillsPanelProps) {
  const [inputs, setInputs] = useState<Readonly<Record<string, string>>>({});
  const [busy, setBusy] = useState<string>();
  const [error, setError] = useState<string>();
  return (
    <section className="capability-page">
      <header className="capability-page__intro"><Sparkles size={22} /><div><h2>Skills</h2><p>发现结果只读展示。调用时由 Agent 加载 Skill 正文，再与当前请求一起发送。</p></div></header>
      {error === undefined ? null : <div className="form-error" role="alert">{error}</div>}
      <div className="capability-grid">
        {skills.length === 0 ? <div className="capability-empty">没有发现可用 Skill。</div> : skills.map((skill) => (
          <article className="capability-card" key={skill.name}>
            <h3>{skill.name}</h3><p>{skill.description}</p><code>{skill.path}</code>
            <textarea aria-label={`${skill.name} 请求`} placeholder="给这个 Skill 的请求（可选）" value={inputs[skill.name] ?? ""} onChange={(event) => setInputs({ ...inputs, [skill.name]: event.target.value })} />
            <button className="primary-button" type="button" disabled={!hasSession || busy !== undefined} onClick={() => {
              setBusy(skill.name); setError(undefined);
              void controller.invokeSkill(skill.name, inputs[skill.name] ?? "").catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason))).finally(() => setBusy(undefined));
            }}><Play size={13} />{busy === skill.name ? "调用中…" : "调用并发送"}</button>
          </article>
        ))}
      </div>
    </section>
  );
}
