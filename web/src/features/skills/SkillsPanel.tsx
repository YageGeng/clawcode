import { Sparkles } from "lucide-react";

import type { SkillDiagnostic, SkillInfo } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";
import { SkillCard } from "./SkillCard";
import { SkillDiagnostics } from "./SkillDiagnostics";

export type SkillsPanelProps = Readonly<{
  skills: readonly SkillInfo[];
  diagnostics: readonly SkillDiagnostic[];
  hasSession: boolean;
  controller: WorkspaceController;
}>;

/** Presents the immutable Session Skill snapshot and its load diagnostics. */
export function SkillsPanel({ skills, diagnostics, hasSession, controller }: SkillsPanelProps) {
  return (
    <section className="capability-page">
      <header className="capability-page__intro">
        <Sparkles size={22} />
        <div>
          <h2>Skills</h2>
          <p>发现结果只读展示。调用时由 Agent 重新加载 Skill 正文，再与当前请求一起发送。</p>
        </div>
      </header>
      <SkillDiagnostics diagnostics={diagnostics} />
      <div className="capability-grid">
        {skills.length === 0 ? (
          <div className="capability-empty">没有发现可用 Skill。</div>
        ) : skills.map((skill) => (
          <SkillCard
            controller={controller}
            hasSession={hasSession}
            key={skill.name}
            skill={skill}
          />
        ))}
      </div>
    </section>
  );
}
