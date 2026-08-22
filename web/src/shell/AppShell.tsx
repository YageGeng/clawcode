import { Boxes, Info, MessageSquareText, PanelLeftOpen, PanelRightOpen, PlugZap, Sparkles } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";

import type { EntryId, SessionId } from "../acp/protocol";
import type { UiBootstrap } from "../bootstrap/model";
import { NewSessionDialog } from "../features/sessions/NewSessionDialog";
import { SessionSidebar } from "../features/sessions/SessionSidebar";
import { Conversation } from "../features/conversation/Conversation";
import { ContextLens, type ContextSelection } from "../features/inspector/ContextLens";
import { Inspector } from "../features/inspector/Inspector";
import { McpPanel } from "../features/mcp/McpPanel";
import { McpElicitationDialog } from "../features/mcp/McpElicitationDialog";
import { SkillsPanel } from "../features/skills/SkillsPanel";
import { initialSessionWorkspaceState } from "../workspace/sessionState";
import { sessionWorkspaceOf } from "../workspace/selectors";
import { useWorkspaceStore } from "../workspace/store";
import type { WorkspaceController } from "../workspace/controller";

type PrimarySection = "sessions" | "skills" | "mcp" | "about";
type ScopedContextSelection = Readonly<{
  sessionId: SessionId;
  selection: ContextSelection;
}>;
type ScopedTreeFocus = Readonly<{
  sessionId: SessionId;
  revision: number;
  entryId?: EntryId;
}>;

export type AppShellProps = Readonly<{
  bootstrap: UiBootstrap;
  controller: WorkspaceController;
}>;

export function AppShell({ bootstrap, controller }: AppShellProps) {
  const connection = useWorkspaceStore((state) => state.connection);
  const sessions = useWorkspaceStore((state) => state.sessions);
  const activeSessionId = useWorkspaceStore((state) => state.activeSessionId);
  // Subscribe to the handful of Session fields this shell renders rather than
  // the entire projection, so streaming deltas do not re-render its frame.
  const contextUsage = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.contextUsage);
  const retry = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.retry);
  const skills = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.skills ?? initialSessionWorkspaceState.skills);
  const skillDiagnostics = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.skillDiagnostics ?? initialSessionWorkspaceState.skillDiagnostics);
  const mcpSnapshot = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.mcpSnapshot ?? initialSessionWorkspaceState.mcpSnapshot);
  const mcpElicitations = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.mcpElicitations ?? initialSessionWorkspaceState.mcpElicitations);
  const runningSessionIds = useWorkspaceStore((state) => state.runningSessionIds);
  const [section, setSection] = useState<PrimarySection>("sessions");
  const [sessionsOpen, setSessionsOpen] = useState(() => window.innerWidth >= 768);
  const [inspectorOpen, setInspectorOpen] = useState(() => window.innerWidth >= 1280);
  const [scopedContextSelection, setScopedContextSelection] = useState<ScopedContextSelection>();
  const [scopedTreeFocus, setScopedTreeFocus] = useState<ScopedTreeFocus>();
  const [newSessionOpen, setNewSessionOpen] = useState(false);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const sessionTriggerRef = useRef<HTMLButtonElement>(null);
  const inspectorTriggerRef = useRef<HTMLButtonElement>(null);
  const contextTriggerRef = useRef<HTMLElement | null>(null);
  const contextSelection = scopedContextSelection !== undefined && scopedContextSelection.sessionId === activeSessionId
    ? scopedContextSelection.selection
    : undefined;
  const treeFocus = scopedTreeFocus !== undefined && scopedTreeFocus.sessionId === activeSessionId
    ? scopedTreeFocus
    : undefined;

  /** Closes the non-modal Context Lens and restores its transcript trigger. */
  const closeContextLens = useCallback(() => {
    setScopedContextSelection(undefined);
    window.requestAnimationFrame(() => contextTriggerRef.current?.focus());
  }, []);
  useEffect(() => {
    // Keep panel defaults aligned with the CSS breakpoints without resetting
    // the selected Session or primary section while the viewport changes.
    const sessionsQuery = window.matchMedia("(min-width: 768px)");
    const inspectorQuery = window.matchMedia("(min-width: 1280px)");
    const updateSessions = (event: MediaQueryListEvent) => setSessionsOpen(event.matches);
    const updateInspector = (event: MediaQueryListEvent) => setInspectorOpen(event.matches);
    sessionsQuery.addEventListener("change", updateSessions);
    inspectorQuery.addEventListener("change", updateInspector);
    return () => {
      sessionsQuery.removeEventListener("change", updateSessions);
      inspectorQuery.removeEventListener("change", updateInspector);
    };
  }, []);

  useEffect(() => {
    const closeTopPanel = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (contextSelection !== undefined) {
        closeContextLens();
        return;
      }
      if (window.innerWidth >= 1280) return;
      if (inspectorOpen) {
        setInspectorOpen(false);
        inspectorTriggerRef.current?.focus();
      } else if (sessionsOpen && window.innerWidth < 1024) {
        setSessionsOpen(false);
        sessionTriggerRef.current?.focus();
      }
    };
    window.addEventListener("keydown", closeTopPanel);
    return () => window.removeEventListener("keydown", closeTopPanel);
  }, [closeContextLens, contextSelection, inspectorOpen, sessionsOpen]);

  useEffect(() => {
    if (retry?.type !== "waiting") return undefined;
    const timer = window.setInterval(() => setNowMs(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [retry]);

  const activeSession = sessions.find((session) => session.sessionId === activeSessionId);
  const selectedMessage = useWorkspaceStore((state) => {
    if (activeSessionId === undefined || contextSelection?.type !== "message") return undefined;
    return state.sessionWorkspaces.get(activeSessionId)?.messages.get(contextSelection.messageId);
  });
  const selectedTool = useWorkspaceStore((state) => {
    if (activeSessionId === undefined || contextSelection?.type !== "tool") return undefined;
    return state.sessionWorkspaces.get(activeSessionId)?.tools.get(contextSelection.toolCallId);
  });
  const pendingElicitation = activeSessionId === undefined
    ? undefined
    : mcpElicitations.values().next().value;
  const connectionLabel = connection.type === "ready" ? "已连接" :
    connection.type === "connecting" ? `正在连接 · 第 ${connection.attempt} 次` :
    connection.type === "initializing" ? "正在初始化 ACP" :
    connection.type === "reconnecting" ? `连接中断，正在重连 · 第 ${connection.attempt} 次` :
    connection.type === "failed" ? connection.message : "已断开";
  const connectionTone = connection.type === "ready" ? "success" : connection.type === "failed" ? "danger" : "warning";
  const retryRemainingMs = retry?.type === "waiting"
    ? Math.max(0, Number(BigInt(retry.scheduledAtMs) + BigInt(retry.delayMs) - BigInt(nowMs)))
    : 0;
  const handleInspect = useCallback((selection: ContextSelection) => {
    if (activeSessionId === undefined) return;
    // Preserve the originating control so closing the non-modal Lens returns
    // keyboard users to the exact transcript action.
    if (document.activeElement instanceof HTMLElement) contextTriggerRef.current = document.activeElement;
    setScopedContextSelection({ sessionId: activeSessionId, selection });
    setScopedTreeFocus((previous) => ({
      sessionId: activeSessionId,
      revision: (previous?.revision ?? 0) + 1,
      ...(selection.treeEntryId === undefined ? {} : { entryId: selection.treeEntryId })
    }));
    setInspectorOpen(true);
    if (window.innerWidth < 1024) setSessionsOpen(false);
  }, [activeSessionId]);

  return (
    <div className="workbench" data-connection={connection.type} data-sessions-open={sessionsOpen} data-inspector-open={inspectorOpen}>
      <nav className="icon-rail" aria-label="主导航">
        <div className="icon-rail__brand" title={bootstrap.product.name}>{bootstrap.product.name.slice(0, 2).toUpperCase()}<span className="visually-hidden">{connectionLabel}</span></div>
        <button ref={sessionTriggerRef} aria-label="会话" className="icon-button" data-active={section === "sessions"} type="button" title="会话" onClick={() => {
          setSection("sessions");
          setSessionsOpen(true);
          if (window.innerWidth < 1024) setInspectorOpen(false);
        }}><MessageSquareText size={19} aria-hidden="true" /></button>
        <button aria-label="Skills" className="icon-button" data-active={section === "skills"} type="button" title="Skills" onClick={() => {
          setScopedContextSelection(undefined);
          setSection("skills");
        }}><Sparkles size={19} aria-hidden="true" /></button>
        <button aria-label="MCP" className="icon-button" data-active={section === "mcp"} type="button" title="MCP" onClick={() => {
          setScopedContextSelection(undefined);
          setSection("mcp");
        }}><PlugZap size={19} aria-hidden="true" /></button>
        <span className="icon-rail__spacer" />
        <button aria-label="关于" className="icon-button" data-active={section === "about"} type="button" title="关于" onClick={() => {
          setScopedContextSelection(undefined);
          setSection("about");
        }}><Info size={19} aria-hidden="true" /></button>
      </nav>

      <div className="side-panel session-panel">
        <SessionSidebar
          sessions={sessions}
          runningSessionIds={runningSessionIds}
          {...(activeSessionId === undefined ? {} : { activeSessionId })}
          onCollapse={() => {
            setSessionsOpen(false);
            sessionTriggerRef.current?.focus();
          }}
          onCreate={() => setNewSessionOpen(true)}
          onOpen={async (sessionId) => {
            setScopedContextSelection(undefined);
            setScopedTreeFocus(undefined);
            await controller.openSession(sessionId);
            if (window.innerWidth < 768) setSessionsOpen(false);
          }}
          onCancel={(sessionId) => controller.cancel(sessionId)}
          onRename={(sessionId, title) => controller.renameSession(sessionId, title)}
          onDelete={(sessionId) => controller.deleteSession(sessionId)}
        />
      </div>

      <main className="main-stage">
        {connection.type === "ready" ? null : <div className="connection-banner" data-tone={connectionTone} role={connection.type === "failed" ? "alert" : "status"}>{connectionLabel}</div>}
        <header className="conversation-header">
          {!sessionsOpen ? <button className="icon-button" type="button" aria-label="展开会话栏" title="展开会话栏" onClick={() => {
            setSessionsOpen(true);
            if (window.innerWidth < 1024) setInspectorOpen(false);
          }}><PanelLeftOpen size={18} aria-hidden="true" /></button> : null}
          <div className="conversation-header__identity">
            <h1>{activeSession?.title ?? (section === "sessions" ? "Agent 工作台" : section === "skills" ? "Skills" : section === "mcp" ? "MCP" : "关于")}</h1>
            <p>{bootstrap.activeModel.displayName}{activeSession === undefined ? "" : ` · ${activeSession.cwd}`}</p>
          </div>
          <div className="runtime-status" aria-label="Agent 运行状态">
            <span className="runtime-chip" data-tone={connectionTone}><span className="runtime-chip__dot" />{connectionLabel}</span>
            {contextUsage === undefined ? null : (
              <span className="runtime-chip" title={`输入 ${contextUsage.exact.inputTokens} · 输出 ${contextUsage.exact.outputTokens} · 缓存读取 ${contextUsage.exact.cacheReadTokens}`}>
                Context {contextUsage.used.toLocaleString()} / {contextUsage.size.toLocaleString()}
              </span>
            )}
            {retry?.type === "waiting" ? <span className="runtime-chip" data-tone="warning">Retry {retry.attempt}/{retry.maxAttempts} · {(retryRemainingMs / 1_000).toFixed(1)}s</span> : null}
            {retry?.type === "running" ? <span className="runtime-chip" data-tone="warning">Retry {retry.attempt}/{retry.maxAttempts}</span> : null}
            {retry?.type === "finished" && !retry.success ? <span className="runtime-chip" data-tone="danger">Retry 失败</span> : null}
          </div>
          <button ref={inspectorTriggerRef} aria-expanded={inspectorOpen} aria-label={inspectorOpen ? "收起详情栏" : "展开详情栏"} className="icon-button" type="button" title={inspectorOpen ? "收起详情栏" : "展开详情栏"} onClick={() => {
            setInspectorOpen((value) => !value);
            if (!inspectorOpen && window.innerWidth < 1024) setSessionsOpen(false);
          }}>
            {inspectorOpen ? <Boxes size={18} aria-hidden="true" /> : <PanelRightOpen size={18} aria-hidden="true" />}
          </button>
        </header>
        {section === "sessions" ? <Conversation
          bootstrap={bootstrap}
          controller={controller}
          {...(contextSelection === undefined ? {} : { contextSelection })}
          onInspect={handleInspect}
        /> : null}
        {section === "skills" ? <SkillsPanel skills={skills} diagnostics={skillDiagnostics} hasSession={activeSessionId !== undefined} controller={controller} /> : null}
        {section === "mcp" ? <McpPanel snapshot={mcpSnapshot} controller={controller} /> : null}
        {section === "about" ? <section className="conversation-placeholder"><div className="empty-state"><h2>{bootstrap.product.name}</h2><p>基于 ACP v2 WebSocket 的本机 Agent 工作台。当前模型：{bootstrap.activeModel.displayName}。</p></div></section> : null}
        {section === "sessions" && contextSelection !== undefined ? <ContextLens
          selection={contextSelection}
          {...(selectedMessage === undefined ? {} : { message: selectedMessage })}
          {...(selectedTool === undefined ? {} : { tool: selectedTool })}
          onClose={closeContextLens}
        /> : null}
      </main>

      <div className="side-panel inspector-panel"><Inspector
        key={`${activeSessionId ?? "none"}:${treeFocus?.revision ?? 0}`}
        controller={controller}
        {...(treeFocus?.entryId === undefined ? {} : { treeEntryId: treeFocus.entryId })}
        onClose={() => {
          setInspectorOpen(false);
          inspectorTriggerRef.current?.focus();
        }}
      /></div>

      {(sessionsOpen || inspectorOpen) && contextSelection === undefined ? <button className="panel-backdrop" type="button" aria-label="关闭辅助面板" onClick={() => {
        if (inspectorOpen) {
          setInspectorOpen(false);
          inspectorTriggerRef.current?.focus();
        } else {
          setSessionsOpen(false);
          sessionTriggerRef.current?.focus();
        }
      }} /> : null}

      {newSessionOpen ? (
        <NewSessionDialog
          defaultCwd={bootstrap.defaultCwd}
          onCancel={() => setNewSessionOpen(false)}
          onCreate={async (cwd) => {
            setScopedContextSelection(undefined);
            setScopedTreeFocus(undefined);
            await controller.newSession(cwd);
            setNewSessionOpen(false);
            setSection("sessions");
            if (window.innerWidth < 768) setSessionsOpen(false);
          }}
        />
      ) : null}
      {pendingElicitation === undefined ? null : (
        <McpElicitationDialog controller={controller} request={pendingElicitation} />
      )}
    </div>
  );
}
