import { Check, Edit3, LoaderCircle, PanelLeftClose, Plus, Search, Square, Trash2, X } from "lucide-react";
import { useMemo, useState } from "react";
import { createPortal } from "react-dom";

import type { SessionId } from "../../acp/protocol";
import type { SessionSummary } from "../../domain/model";

export type SessionSidebarProps = Readonly<{
  sessions: readonly SessionSummary[];
  activeSessionId?: SessionId;
  runningSessionIds: ReadonlySet<SessionId>;
  onCollapse: () => void;
  onCreate: () => void;
  onOpen: (sessionId: SessionId) => Promise<void>;
  onCancel: (sessionId: SessionId) => void;
  onRename: (sessionId: SessionId, title: string) => Promise<void>;
  onDelete: (sessionId: SessionId) => Promise<void>;
}>;

type SessionGroup = Readonly<{ label: string; sessions: readonly SessionSummary[] }>;
type SessionPreview = Readonly<{
  sessionId: SessionId;
  title: string;
  cwd: string;
  left: number;
  top: number;
}>;

export function SessionSidebar(props: SessionSidebarProps) {
  const [query, setQuery] = useState("");
  const [operationError, setOperationError] = useState<string>();
  const [editingSessionId, setEditingSessionId] = useState<SessionId>();
  const [editingTitle, setEditingTitle] = useState("");
  const [deletingSessionId, setDeletingSessionId] = useState<SessionId>();
  const [preview, setPreview] = useState<SessionPreview>();
  const groups = useMemo<readonly SessionGroup[]>(() => {
    const normalized = query.trim().toLocaleLowerCase();
    const matched = props.sessions.filter((session) => normalized.length === 0 ||
      session.title.toLocaleLowerCase().includes(normalized) ||
      session.cwd.toLocaleLowerCase().includes(normalized) ||
      session.sessionId.toLocaleLowerCase().includes(normalized));
    const included = new Set(matched.map((session) => session.sessionId));
    const byId = new Map(props.sessions.map((session) => [session.sessionId, session]));
    for (const session of matched) {
      let owner = session.ownership?.sessionId;
      const visited = new Set<SessionId>();
      while (owner !== undefined && !visited.has(owner)) {
        visited.add(owner);
        included.add(owner);
        owner = byId.get(owner)?.ownership?.sessionId;
      }
    }
    const visible = props.sessions.filter((session) => included.has(session.sessionId) &&
      (session.ownership === undefined || !byId.has(session.ownership.sessionId)));
    const current = new Date();
    const today = new Date(current.getFullYear(), current.getMonth(), current.getDate()).getTime();
    const yesterday = today - 86_400_000;
    const buckets = new Map<string, SessionSummary[]>([["今天", []], ["昨天", []], ["更早", []]]);
    for (const session of visible) {
      const modified = session.modifiedAtMs === undefined ? 0 : Number(BigInt(session.modifiedAtMs));
      const label = modified >= today ? "今天" : modified >= yesterday ? "昨天" : "更早";
      buckets.get(label)?.push(session);
    }
    return [...buckets.entries()]
      .map(([label, sessions]) => ({
        label,
        sessions: sessions.sort((left, right) => {
          const leftTime = left.modifiedAtMs === undefined ? 0n : BigInt(left.modifiedAtMs);
          const rightTime = right.modifiedAtMs === undefined ? 0n : BigInt(right.modifiedAtMs);
          return leftTime === rightTime ? 0 : leftTime > rightTime ? -1 : 1;
        })
      }))
      .filter((group) => group.sessions.length > 0);
  }, [props.sessions, query]);

  const run = async (operation: () => Promise<void>) => {
    setOperationError(undefined);
    try {
      await operation();
    } catch (reason: unknown) {
      setOperationError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  const showPreview = (session: SessionSummary, target: HTMLElement) => {
    const bounds = target.getBoundingClientRect();
    setPreview({
      sessionId: session.sessionId,
      title: session.title,
      cwd: session.cwd,
      left: Math.max(8, Math.min(bounds.right + 8, window.innerWidth - 348)),
      top: Math.max(8, Math.min(bounds.top, window.innerHeight - 248))
    });
  };

  /** Renders the ownership tree while guarding malformed cyclic metadata. */
  const renderSession = (session: SessionSummary, ancestors = new Set<SessionId>()): React.ReactNode => {
              const running = props.runningSessionIds.has(session.sessionId);
              const children = props.sessions.filter((child) => child.ownership?.sessionId === session.sessionId && !ancestors.has(child.sessionId));
              const visited = new Set(ancestors).add(session.sessionId);
              return <div className="session-branch" key={session.sessionId}><div
                className="session-item"
                data-active={session.sessionId === props.activeSessionId}
                data-running={running}
                key={session.sessionId}
                onBlurCapture={() => setPreview(undefined)}
                onFocusCapture={(event) => showPreview(session, event.currentTarget)}
                onMouseEnter={(event) => showPreview(session, event.currentTarget)}
                onMouseLeave={() => setPreview(undefined)}
              >
                {editingSessionId === session.sessionId ? (
                  <div className="session-item__inline-form">
                    <input autoFocus aria-label="新会话标题" value={editingTitle} onChange={(event) => setEditingTitle(event.target.value)} onKeyDown={(event) => {
                      if (event.key === "Escape") setEditingSessionId(undefined);
                      if (event.key === "Enter" && editingTitle.trim().length > 0) void run(async () => {
                        await props.onRename(session.sessionId, editingTitle.trim());
                        setEditingSessionId(undefined);
                      });
                    }} />
                    <button type="button" title="保存标题" disabled={editingTitle.trim().length === 0} onClick={() => void run(async () => {
                      await props.onRename(session.sessionId, editingTitle.trim());
                      setEditingSessionId(undefined);
                    })}><Check size={13} /></button>
                    <button type="button" title="取消重命名" onClick={() => setEditingSessionId(undefined)}><X size={13} /></button>
                  </div>
                ) : (
                  <div className="session-item__heading">
                    <button
                      className="session-item__open"
                      type="button"
                      aria-describedby={preview?.sessionId === session.sessionId ? "session-preview" : undefined}
                      onClick={() => void run(() => props.onOpen(session.sessionId))}
                    >
                      <span className="session-item__title">{session.title}</span>
                      <span className="session-item__cwd">{session.cwd}</span>
                    </button>
                    {running ? <span className="session-item__status" role="status"><LoaderCircle size={11} aria-hidden="true" />运行中</span> : null}
                    <span className="session-item__menu">
                      {running ? <button type="button" title="停止运行" onClick={() => props.onCancel(session.sessionId)}><Square size={12} aria-hidden="true" /></button> : null}
                      <button type="button" title="重命名" onClick={() => { setEditingSessionId(session.sessionId); setEditingTitle(session.title); setDeletingSessionId(undefined); }}><Edit3 size={13} aria-hidden="true" /></button>
                      <button type="button" title="删除会话" onClick={() => { setDeletingSessionId(session.sessionId); setEditingSessionId(undefined); }}><Trash2 size={14} aria-hidden="true" /></button>
                    </span>
                  </div>
                )}
                {deletingSessionId === session.sessionId ? <div className="session-item__confirm"><span>{children.length > 0 ? "永久删除会话及其所有子会话？此操作不可撤销。" : "永久删除会话？此操作不可撤销。"}</span><button type="button" onClick={() => setDeletingSessionId(undefined)}>取消</button><button type="button" onClick={() => void run(async () => {
                  // A successful delete unmounts the hovered row before it can
                  // emit mouseleave, so close its body-level Portal explicitly.
                  setPreview(undefined);
                  await props.onDelete(session.sessionId);
                  setDeletingSessionId(undefined);
                })}>确认删除</button></div> : null}
              </div>{children.length === 0 ? null : <details className="session-children" open={children.some((child) => child.sessionId === props.activeSessionId) || undefined}>
                <summary>子会话 · {children.length}</summary>
                {children.map((child) => renderSession(child, visited))}
              </details>}</div>;
  };

  return (
    <aside className="session-sidebar" aria-label="会话">
      <div className="panel-heading">
        <h2>会话</h2>
        <button className="icon-button" type="button" title="收起会话栏" onClick={props.onCollapse}>
          <PanelLeftClose size={17} aria-hidden="true" />
        </button>
      </div>
      <div className="session-sidebar__actions">
        <button className="primary-button" type="button" onClick={props.onCreate}>
          <Plus size={15} aria-hidden="true" /> 新建
        </button>
      </div>
      <label className="search-field">
        <span className="visually-hidden">搜索会话</span>
        <Search size={14} aria-hidden="true" />
        <input className="search-input" placeholder="搜索标题或目录" value={query} onChange={(event) => setQuery(event.target.value)} />
      </label>
      {operationError === undefined ? null : <div className="form-error" role="alert">{operationError}</div>}
      <div className="session-list" onScroll={() => setPreview(undefined)}>
        {groups.length === 0 ? <div className="empty-list">没有匹配的会话</div> : groups.map((group) => (
          <section className="session-group" key={group.label}>
            <h3>{group.label}</h3>
            {group.sessions.map((session) => renderSession(session))}
          </section>
        ))}
      </div>
      {preview === undefined ? null : createPortal(
        <div
          className="session-preview"
          id="session-preview"
          role="tooltip"
          style={{ left: preview.left, top: preview.top }}
        >
          <span className="session-preview__label">标题</span>
          <strong>{preview.title}</strong>
          <span className="session-preview__label">目录</span>
          <span>{preview.cwd}</span>
        </div>,
        document.body
      )}
    </aside>
  );
}
