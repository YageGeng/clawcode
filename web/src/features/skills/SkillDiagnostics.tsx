import { AlertTriangle } from "lucide-react";

import type { SkillDiagnostic } from "../../domain/model";

export type SkillDiagnosticsProps = Readonly<{
  diagnostics: readonly SkillDiagnostic[];
}>;

/** Renders recoverable Skill discovery failures without hiding valid Skills. */
export function SkillDiagnostics({ diagnostics }: SkillDiagnosticsProps) {
  if (diagnostics.length === 0) return null;

  return (
    <section className="skill-diagnostics" aria-label="Skill diagnostics">
      <header><AlertTriangle size={16} /><strong>发现诊断</strong><span>{diagnostics.length}</span></header>
      <div className="skill-diagnostics__list">
        {/* Multiple collisions may share a code and path, so preserve row identity with its position. */}
        {diagnostics.map((diagnostic, index) => (
          <article data-severity={diagnostic.severity} key={`${diagnostic.code}-${diagnostic.path ?? "unknown"}-${index}`}>
            <div><code>{diagnostic.code}</code><span>{diagnostic.severity}</span></div>
            <p>{diagnostic.message}</p>
            {diagnostic.path === undefined ? null : <small title={diagnostic.path}>{diagnostic.path}</small>}
            {diagnostic.collision === undefined ? null : (
              <dl>
                <div><dt>Winner</dt><dd>{diagnostic.collision.winnerPath}</dd></div>
                <div><dt>Ignored</dt><dd>{diagnostic.collision.loserPath}</dd></div>
              </dl>
            )}
          </article>
        ))}
      </div>
    </section>
  );
}
