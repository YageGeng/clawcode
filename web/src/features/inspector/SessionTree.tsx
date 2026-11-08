import { Crosshair, GitBranch, GitFork, LocateFixed, Minimize2 } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";

import type { EntryId } from "../../acp/protocol";
import type { SessionTree as SessionTreeModel, SessionTreeEntry } from "../../domain/model";
import type { NavigationSummaryOptions, WorkspaceController } from "../../workspace/controller";

export type SessionTreeProps = Readonly<{
  tree?: SessionTreeModel;
  cwd?: string;
  focusEntryId?: EntryId;
  /** Nodes on the active branch with a transcript row to locate, so only they offer a locate button. */
  revealableRows?: ReadonlyMap<EntryId, string>;
  onRevealRow?: (rowKey: string) => void;
  controller: WorkspaceController;
}>;

type PresentedEntry = Readonly<{ entry: SessionTreeEntry; depth: number }>;

/** Tree operations that leave a branch behind and can summarize it. */
type NavigationKind = "navigate" | "branch";

/** Deepest indent the Tree draws; deeper nodes share it, so depth stops counting there. */
const MAX_INDENT_DEPTH = 8;

export function SessionTree({ tree, cwd, focusEntryId, revealableRows, onRevealRow, controller }: SessionTreeProps) {
  const [selected, setSelected] = useState<EntryId | null | undefined>(focusEntryId);
  const [busy, setBusy] = useState<string>();
  const [error, setError] = useState<string>();
  // Navigation waiting for the user's branch-summary choice, as pi asks.
  const [choosing, setChoosing] = useState<NavigationKind>();
  const [instructions, setInstructions] = useState<string>();
  const focusedEntryRef = useRef<HTMLButtonElement>(null);
  const revealedRef = useRef(false);
  const entries = useMemo<readonly PresentedEntry[]>(() => {
    if (tree === undefined) return [];
    const parents = new Map(tree.entries.map((entry) => [entry.entryId, entry.parentId ?? null]));
    return tree.entries.map((entry) => {
      // The cap bounds the walk, which also stops any parent cycle.
      let depth = 0;
      let parent = entry.parentId ?? null;
      while (parent !== null && depth < MAX_INDENT_DEPTH) {
        depth += 1;
        parent = parents.get(parent) ?? null;
      }
      return { entry, depth };
    });
  }, [tree]);

  useEffect(() => {
    // Inspector remounts for each locate request. Reveal the node once, when it
    // first renders, so per-Turn Tree refreshes leave the user's scrolling alone.
    if (focusEntryId === undefined || revealedRef.current || focusedEntryRef.current === null) return;
    revealedRef.current = true;
    focusedEntryRef.current.scrollIntoView({
      behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth",
      block: "center"
    });
  }, [focusEntryId, tree]);

  const operate = async (name: string, operation: () => unknown) => {
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
  const navigateWith = (kind: NavigationKind, options?: NavigationSummaryOptions) => {
    setChoosing(undefined);
    setInstructions(undefined);
    void operate(kind, () => kind === "navigate" ? controller.navigate(target, options) : controller.branch(target, options));
  };
  // Moving forward to a descendant of the leaf, or to the root, abandons
  // nothing, so only a move off the current branch offers a summary.
  const abandonsBranch = (entryId: EntryId) => {
    const parents = new Map(tree.entries.map((entry) => [entry.entryId, entry.parentId ?? null]));
    const leaf = tree.leafId ?? null;
    if (leaf === null) return false;
    // The visited set bounds the walk, which also stops any parent cycle.
    const visited = new Set<EntryId>();
    for (let current: EntryId | null = entryId; current !== null && !visited.has(current); current = parents.get(current) ?? null) {
      if (current === leaf) return false;
      visited.add(current);
    }
    return true;
  };
  const startNavigation = (kind: NavigationKind) => {
    if (target !== null && abandonsBranch(target)) setChoosing(kind);
    else navigateWith(kind);
  };
  return (
    <div className="tree-view">
      <div className="tree-toolbar">
        <button type="button" disabled={busy !== undefined} title="移动到所选节点" onClick={() => startNavigation("navigate")}><LocateFixed size={14} />Navigate</button>
        <button type="button" disabled={busy !== undefined} title="从所选节点建立活动分支" onClick={() => startNavigation("branch")}><GitBranch size={14} />Branch</button>
        <button type="button" disabled={busy !== undefined || target === null || cwd === undefined} title="Fork 为新会话" onClick={() => {
          if (target !== null && cwd !== undefined) void operate("fork", () => controller.fork(target, cwd));
        }}><GitFork size={14} />Fork</button>
        <button type="button" disabled={busy !== undefined} title="自动压缩上下文" onClick={() => void operate("compact", () => controller.submit({ text: "/compact", resources: [], images: [] }))}><Minimize2 size={14} />Compact</button>
      </div>
      {choosing === undefined ? null : (
        <div className="tree-summary-choice" role="group" aria-label="分支摘要">
          <span>离开当前分支前是否生成摘要？</span>
          <button type="button" onClick={() => navigateWith(choosing, { summarize: false })}>不生成摘要</button>
          <button type="button" onClick={() => navigateWith(choosing, { summarize: true })}>生成摘要</button>
          {instructions === undefined
            ? <button type="button" onClick={() => setInstructions("")}>带指令生成摘要</button>
            : (
              <>
                <textarea aria-label="摘要指令" value={instructions} onChange={(event) => setInstructions(event.target.value)} />
                <button type="button" disabled={instructions.trim() === ""} onClick={() => navigateWith(choosing, { summarize: true, customInstructions: instructions.trim() })}>确认</button>
              </>
            )}
          <button type="button" onClick={() => { setChoosing(undefined); setInstructions(undefined); }}>取消</button>
        </div>
      )}
      {busy === undefined ? null : <div className="operation-status">{busy} 进行中…</div>}
      {error === undefined ? null : <div className="form-error" role="alert">{error}</div>}
      {focusEntryId === undefined ? null : <span aria-hidden="true" className="tree-locate-spacer" data-position="before" />}
      <button className="tree-root" data-selected={target === null} type="button" onClick={() => setSelected(null)}>根节点</button>
      <div className="tree-entries">
        {entries.map(({ entry, depth }) => {
          const rowKey = revealableRows?.get(entry.entryId);
          return (
            <div className="tree-row" key={entry.entryId}>
              <button
                aria-pressed={entry.entryId === target}
                className="tree-entry"
                data-active={entry.entryId === tree.leafId}
                data-located={entry.entryId === focusEntryId}
                data-revealable={rowKey !== undefined}
                data-selected={entry.entryId === target}
                {...(entry.entryId === focusEntryId ? { ref: focusedEntryRef } : {})}
                style={{ paddingLeft: `${10 + depth * 13}px` }}
                type="button"
                onClick={() => setSelected(entry.entryId)}
              >
                <span className="tree-entry__rail" />
                <span className="tree-entry__kind">{entry.kind}</span>
                <span className="tree-entry__id">{entry.entryId}</span>
              </button>
              {rowKey === undefined ? null : <button aria-label={`在聊天记录中定位 ${entry.entryId}`} className="tree-entry__locate" title="在聊天记录中定位" type="button" onClick={() => onRevealRow?.(rowKey)}><Crosshair size={13} aria-hidden="true" /></button>}
            </div>
          );
        })}
      </div>
      {target === null ? null : <details className="tree-preview"><summary>所选节点数据</summary><pre>{JSON.stringify(tree.entries.find((entry) => entry.entryId === target)?.payload ?? {}, null, 2)}</pre></details>}
      {focusEntryId === undefined ? null : <span aria-hidden="true" className="tree-locate-spacer" />}
    </div>
  );
}
