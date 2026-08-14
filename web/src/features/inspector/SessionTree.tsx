import { GitBranch, GitFork, LocateFixed, Minimize2 } from "lucide-react";
import { useMemo, useState } from "react";

import type { EntryId } from "../../acp/protocol";
import type { SessionTree as SessionTreeModel, SessionTreeEntry } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";

export type SessionTreeProps = Readonly<{
  tree?: SessionTreeModel;
  cwd?: string;
  controller: WorkspaceController;
}>;

type PresentedEntry = Readonly<{ entry: SessionTreeEntry; depth: number }>;

export function SessionTree({ tree, cwd, controller }: SessionTreeProps) {
  const [selected, setSelected] = useState<EntryId | null>();
  const [busy, setBusy] = useState<string>();
  const [error, setError] = useState<string>();
  const entries = useMemo<readonly PresentedEntry[]>(() => {
    if (tree === undefined) return [];
    const parents = new Map(tree.entries.map((entry) => [entry.entryId, entry.parentId ?? null]));
    return tree.entries.map((entry) => {
      let depth = 0;
      let parent = entry.parentId ?? null;
      const seen = new Set<EntryId>();
      while (parent !== null && depth < tree.entries.length && !seen.has(parent)) {
        seen.add(parent);
        depth += 1;
        parent = parents.get(parent) ?? null;
      }
      return { entry, depth };
    });
  }, [tree]);

  const operate = async (name: string, operation: () => Promise<unknown>) => {
    setBusy(name);
    setError(undefined);
    try {
      await operation();
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(undefined);
    }
  };

  if (tree === undefined) return <div className="inspector-empty">选择会话后查看持久化树。</div>;
  const target = selected === undefined ? tree.leafId ?? null : selected;
  return (
    <div className="tree-view">
      <div className="tree-toolbar">
        <button type="button" disabled={busy !== undefined} title="移动到所选节点" onClick={() => void operate("navigate", () => controller.navigate(target))}><LocateFixed size={14} />Navigate</button>
        <button type="button" disabled={busy !== undefined} title="从所选节点建立活动分支" onClick={() => void operate("branch", () => controller.branch(target))}><GitBranch size={14} />Branch</button>
        <button type="button" disabled={busy !== undefined || target === null || cwd === undefined} title="Fork 为新会话" onClick={() => {
          if (target !== null && cwd !== undefined) void operate("fork", () => controller.fork(target, cwd));
        }}><GitFork size={14} />Fork</button>
        <button type="button" disabled={busy !== undefined} title="自动压缩上下文" onClick={() => void operate("compact", () => controller.compact())}><Minimize2 size={14} />Compact</button>
      </div>
      {busy === undefined ? null : <div className="operation-status">{busy} 进行中…</div>}
      {error === undefined ? null : <div className="form-error" role="alert">{error}</div>}
      <button className="tree-root" data-selected={target === null} type="button" onClick={() => setSelected(null)}>根节点</button>
      <div className="tree-entries">
        {entries.map(({ entry, depth }) => (
          <button
            className="tree-entry"
            data-active={entry.entryId === tree.leafId}
            data-selected={entry.entryId === target}
            key={entry.entryId}
            style={{ paddingLeft: `${10 + Math.min(depth, 8) * 13}px` }}
            type="button"
            onClick={() => setSelected(entry.entryId)}
          >
            <span className="tree-entry__rail" />
            <span className="tree-entry__kind">{entry.kind}</span>
            <span className="tree-entry__id">{entry.entryId}</span>
          </button>
        ))}
      </div>
      {target === null ? null : <details className="tree-preview"><summary>所选节点数据</summary><pre>{JSON.stringify(tree.entries.find((entry) => entry.entryId === target)?.payload ?? {}, null, 2)}</pre></details>}
    </div>
  );
}
