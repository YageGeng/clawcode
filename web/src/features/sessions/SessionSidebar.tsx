import { Check, Edit3, PanelLeftClose, Plus, Search, Trash2, X } from "lucide-react";
import { useMemo, useState } from "react";

import type { SessionId } from "../../acp/protocol";
import type { SessionSummary } from "../../domain/model";

export type SessionSidebarProps = Readonly<{
  sessions: readonly SessionSummary[];
  activeSessionId?: SessionId;
  onCollapse: () => void;
  onCreate: () => void;
  onOpen: (sessionId: SessionId) => Promise<void>;
  onRename: (sessionId: SessionId, title: string) => Promise<void>;
  onDelete: (sessionId: SessionId) => Promise<void>;
}>;

type SessionGroup = Readonly<{ label: string; sessions: readonly SessionSummary[] }>;

export function SessionSidebar(props: SessionSidebarProps) {
  const [query, setQuery] = useState("");
  const [operationError, setOperationError] = useState<string>();
  const [editingSessionId, setEditingSessionId] = useState<SessionId>();
  const [editingTitle, setEditingTitle] = useState("");
  const [deletingSessionId, setDeletingSessionId] = useState<SessionId>();
  const groups = useMemo<readonly SessionGroup[]>(() => {
    const normalized = query.trim().toLocaleLowerCase();
    const visible = props.sessions.filter((session) => normalized.length === 0 ||
      session.title.toLocaleLowerCase().includes(normalized) ||
      session.cwd.toLocaleLowerCase().includes(normalized) ||
      session.sessionId.toLocaleLowerCase().includes(normalized));
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
      <div className="session-list">
        {groups.length === 0 ? <div className="empty-list">没有匹配的会话</div> : groups.map((group) => (
          <section className="session-group" key={group.label}>
            <h3>{group.label}</h3>
            {group.sessions.map((session) => (
              <div className="session-item" data-active={session.sessionId === props.activeSessionId} key={session.sessionId}>
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
                    <button className="session-item__open" type="button" onClick={() => void run(() => props.onOpen(session.sessionId))}>
                      <span className="session-item__title">{session.title}</span>
                      <span className="session-item__cwd">{session.cwd}</span>
                    </button>
                    <span className="session-item__menu">
                      <button type="button" title="重命名" onClick={() => { setEditingSessionId(session.sessionId); setEditingTitle(session.title); setDeletingSessionId(undefined); }}><Edit3 size={13} aria-hidden="true" /></button>
                      <button type="button" title="删除会话" onClick={() => { setDeletingSessionId(session.sessionId); setEditingSessionId(undefined); }}><Trash2 size={14} aria-hidden="true" /></button>
                    </span>
                  </div>
                )}
                {deletingSessionId === session.sessionId ? <div className="session-item__confirm"><span>永久删除会话？此操作不可撤销。</span><button type="button" onClick={() => setDeletingSessionId(undefined)}>取消</button><button type="button" onClick={() => void run(async () => { await props.onDelete(session.sessionId); setDeletingSessionId(undefined); })}>确认删除</button></div> : null}
              </div>
            ))}
          </section>
        ))}
      </div>
    </aside>
  );
}
