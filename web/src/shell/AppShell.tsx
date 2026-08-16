import { Boxes, Info, MessageSquareText, PanelLeftOpen, PanelRightOpen, PlugZap, Sparkles } from "lucide-react";
import { useEffect, useState } from "react";

import type { UiBootstrap } from "../bootstrap/model";
import { NewSessionDialog } from "../features/sessions/NewSessionDialog";
import { SessionSidebar } from "../features/sessions/SessionSidebar";
import { Conversation } from "../features/conversation/Conversation";
import { Inspector } from "../features/inspector/Inspector";
import { McpPanel } from "../features/mcp/McpPanel";
import { SkillsPanel } from "../features/skills/SkillsPanel";
import { useWorkspaceStore } from "../workspace/store";
import type { WorkspaceController } from "../workspace/controller";

type PrimarySection = "sessions" | "skills" | "mcp" | "about";

export type AppShellProps = Readonly<{
  bootstrap: UiBootstrap;
  controller: WorkspaceController;
}>;

export function AppShell({ bootstrap, controller }: AppShellProps) {
  const state = useWorkspaceStore();
  const [section, setSection] = useState<PrimarySection>("sessions");
  const [sessionsOpen, setSessionsOpen] = useState(() => window.innerWidth >= 820);
  const [inspectorOpen, setInspectorOpen] = useState(() => window.innerWidth >= 1100);
  const [newSessionOpen, setNewSessionOpen] = useState(false);
  const [nowMs, setNowMs] = useState(() => Date.now());

  useEffect(() => {
    const sessionsQuery = window.matchMedia("(min-width: 820px)");
    const inspectorQuery = window.matchMedia("(min-width: 1100px)");
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
    if (state.retry?.type !== "waiting") return undefined;
    const timer = window.setInterval(() => setNowMs(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [state.retry]);

  const activeSession = state.sessions.find((session) => session.sessionId === state.activeSessionId);
  const connectionLabel = state.connection.type === "ready" ? "已连接" :
    state.connection.type === "connecting" ? `正在连接 · 第 ${state.connection.attempt} 次` :
    state.connection.type === "initializing" ? "正在初始化 ACP" :
    state.connection.type === "reconnecting" ? `连接中断，正在重连 · 第 ${state.connection.attempt} 次` :
    state.connection.type === "failed" ? state.connection.message : "已断开";
  const connectionTone = state.connection.type === "ready" ? "success" : state.connection.type === "failed" ? "danger" : "warning";
  const retryRemainingMs = state.retry?.type === "waiting"
    ? Math.max(0, Number(BigInt(state.retry.scheduledAtMs) + BigInt(state.retry.delayMs) - BigInt(nowMs)))
    : 0;

  return (
    <div className="workbench" data-sessions-open={sessionsOpen} data-inspector-open={inspectorOpen}>
      <nav className="icon-rail" aria-label="主导航">
        <div className="icon-rail__brand" title={bootstrap.product.name}>{bootstrap.product.name.slice(0, 2).toUpperCase()}</div>
        <button className="icon-button" data-active={section === "sessions"} type="button" title="会话" onClick={() => { setSection("sessions"); setSessionsOpen(true); }}><MessageSquareText size={19} /></button>
        <button className="icon-button" data-active={section === "skills"} type="button" title="Skills" onClick={() => setSection("skills")}><Sparkles size={19} /></button>
        <button className="icon-button" data-active={section === "mcp"} type="button" title="MCP" onClick={() => setSection("mcp")}><PlugZap size={19} /></button>
        <span className="icon-rail__spacer" />
        <button className="icon-button" data-active={section === "about"} type="button" title="关于" onClick={() => setSection("about")}><Info size={19} /></button>
      </nav>

      <div className="side-panel session-panel">
        <SessionSidebar
          sessions={state.sessions}
          {...(state.activeSessionId === undefined ? {} : { activeSessionId: state.activeSessionId })}
          onCollapse={() => setSessionsOpen(false)}
          onCreate={() => setNewSessionOpen(true)}
          onOpen={(sessionId) => controller.openSession(sessionId)}
          onRename={(sessionId, title) => controller.renameSession(sessionId, title)}
          onDelete={(sessionId) => controller.deleteSession(sessionId)}
        />
      </div>

      <main className="main-stage">
        <div className="connection-banner" data-tone={connectionTone}>{connectionLabel}</div>
        <header className="conversation-header">
          {!sessionsOpen ? <button className="icon-button" type="button" title="展开会话栏" onClick={() => setSessionsOpen(true)}><PanelLeftOpen size={18} /></button> : null}
          <div className="conversation-header__identity">
            <h1>{activeSession?.title ?? (section === "sessions" ? "Agent 工作台" : section === "skills" ? "Skills" : section === "mcp" ? "MCP" : "关于")}</h1>
            <p>{bootstrap.activeModel.displayName}{activeSession === undefined ? "" : ` · ${activeSession.cwd}`}</p>
          </div>
          <div className="runtime-status" aria-label="Agent 运行状态">
            {state.contextUsage === undefined ? null : (
              <span className="runtime-chip" title={`输入 ${state.contextUsage.exact.inputTokens} · 输出 ${state.contextUsage.exact.outputTokens} · 缓存读取 ${state.contextUsage.exact.cacheReadTokens}`}>
                Context {state.contextUsage.used.toLocaleString()} / {state.contextUsage.size.toLocaleString()}
              </span>
            )}
            {state.retry?.type === "waiting" ? <span className="runtime-chip" data-tone="warning">Retry {state.retry.attempt}/{state.retry.maxAttempts} · {(retryRemainingMs / 1_000).toFixed(1)}s</span> : null}
            {state.retry?.type === "running" ? <span className="runtime-chip" data-tone="warning">Retry {state.retry.attempt}/{state.retry.maxAttempts}</span> : null}
            {state.retry?.type === "finished" && !state.retry.success ? <span className="runtime-chip" data-tone="danger">Retry 失败</span> : null}
            {state.compaction.type === "running" ? <span className="runtime-chip" data-tone="accent">Compact · {state.compaction.reason}</span> : null}
          </div>
          <button className="icon-button" type="button" title={inspectorOpen ? "收起详情栏" : "展开详情栏"} onClick={() => setInspectorOpen((value) => !value)}>
            {inspectorOpen ? <Boxes size={18} /> : <PanelRightOpen size={18} />}
          </button>
        </header>
        {section === "sessions" ? <Conversation bootstrap={bootstrap} controller={controller} /> : null}
        {section === "skills" ? <SkillsPanel skills={state.skills} diagnostics={state.skillDiagnostics} hasSession={state.activeSessionId !== undefined} controller={controller} /> : null}
        {section === "mcp" ? <McpPanel servers={state.mcpServers} /> : null}
        {section === "about" ? <section className="conversation-placeholder"><div className="empty-state"><h2>{bootstrap.product.name}</h2><p>基于 ACP v2 WebSocket 的本机 Agent 工作台。当前模型：{bootstrap.activeModel.displayName}。</p></div></section> : null}
      </main>

      <div className="side-panel inspector-panel"><Inspector controller={controller} /></div>

      {newSessionOpen ? (
        <NewSessionDialog
          defaultCwd={bootstrap.defaultCwd}
          onCancel={() => setNewSessionOpen(false)}
          onCreate={async (cwd) => {
            await controller.newSession(cwd);
            setNewSessionOpen(false);
            setSection("sessions");
          }}
        />
      ) : null}
    </div>
  );
}
