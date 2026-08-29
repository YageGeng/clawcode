import { Activity, GitFork, SquareTerminal, Wrench, X } from "lucide-react";
import { useState } from "react";

import type { EntryId } from "../../acp/protocol";
import type { WorkspaceController } from "../../workspace/controller";
import { MAX_RETAINED_SESSION_EVENTS } from "../../workspace/sessionState";
import { sessionWorkspaceOf } from "../../workspace/selectors";
import { useWorkspaceStore } from "../../workspace/store";
import { ToolCallCard } from "../conversation/ToolCallCard";
import { EventLog } from "./EventLog";
import { SessionTree } from "./SessionTree";
import { TerminalPanel } from "./TerminalPanel";

type InspectorTab = "tree" | "events" | "tools" | "terminals";

export type InspectorProps = Readonly<{
  controller: WorkspaceController;
  treeEntryId?: EntryId;
  onClose: () => void;
}>;

/** Tree panel reads only the durable tree, so streaming deltas skip it. */
function TreePanel({ controller, treeEntryId }: Readonly<{ controller: WorkspaceController; treeEntryId?: EntryId }>) {
  const tree = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.tree);
  const cwd = useWorkspaceStore((state) => state.sessions
    .find((session) => session.sessionId === state.activeSessionId)?.cwd);
  return <SessionTree {...(tree === undefined ? {} : { tree })} {...(cwd === undefined ? {} : { cwd })} {...(treeEntryId === undefined ? {} : { focusEntryId: treeEntryId })} controller={controller} />;
}

/** Events panel only joins the event stream and diagnostics on demand. */
function EventsPanel() {
  const events = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.events);
  const diagnostics = useWorkspaceStore((state) => state.diagnostics);
  const sessionDiagnostics = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.diagnostics);
  const capped = events !== undefined && events.length >= MAX_RETAINED_SESSION_EVENTS;
  return <>
    {capped ? <div className="inspector-note">当前展示最近 {MAX_RETAINED_SESSION_EVENTS} 条事件。</div> : null}
    <EventLog events={events ?? []} diagnostics={[...(diagnostics ?? []), ...(sessionDiagnostics ?? [])]} />
  </>;
}

/** Tools panel subscribes only to the tool collection. */
function ToolsPanel() {
  const tools = useWorkspaceStore((state) => sessionWorkspaceOf(state)?.tools);
  if (tools === undefined || tools.size === 0) {
    return <div className="inspector-empty">当前会话没有工具调用。</div>;
  }
  return <div className="inspector-tools">{[...tools.values()].map((tool) => <ToolCallCard key={tool.toolCallId} tool={tool} />)}</div>;
}

export function Inspector({ controller, treeEntryId, onClose }: InspectorProps) {
  const [tab, setTab] = useState<InspectorTab>("tree");
  const tabs: readonly InspectorTab[] = ["tree", "events", "tools", "terminals"];
  return (
    <aside className="inspector" aria-label="运行详情">
      <header className="inspector__overlay-header">
        <span>Inspector</span>
        <button className="icon-button" type="button" aria-label="关闭详情栏" title="关闭详情栏" onClick={onClose}><X size={16} aria-hidden="true" /></button>
      </header>
      <div className="inspector-tabs" role="tablist">
        {tabs.map((item) => {
          const icon = item === "tree" ? <GitFork size={14} aria-hidden="true" /> : item === "events" ? <Activity size={14} aria-hidden="true" /> : item === "tools" ? <Wrench size={14} aria-hidden="true" /> : <SquareTerminal size={14} aria-hidden="true" />;
          const label = item === "tree" ? "Tree" : item === "events" ? "Events" : item === "tools" ? "Tools" : "Terminals";
          return <button
            aria-controls={`inspector-panel-${item}`}
            aria-selected={tab === item}
            data-active={tab === item}
            id={`inspector-tab-${item}`}
            key={item}
            role="tab"
            tabIndex={tab === item ? 0 : -1}
            type="button"
            onClick={() => setTab(item)}
            onKeyDown={(event) => {
              const current = tabs.indexOf(item);
              const next = event.key === "ArrowRight" ? (current + 1) % tabs.length
                : event.key === "ArrowLeft" ? (current - 1 + tabs.length) % tabs.length
                  : event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 : undefined;
              if (next === undefined) return;
              event.preventDefault();
              const nextTab = tabs[next];
              if (nextTab !== undefined) {
                setTab(nextTab);
                document.getElementById(`inspector-tab-${nextTab}`)?.focus();
              }
            }}
          >{icon}{label}</button>;
        })}
      </div>
      <div aria-labelledby={`inspector-tab-${tab}`} className="inspector-content" id={`inspector-panel-${tab}`} role="tabpanel">
        {tab === "tree" ? <TreePanel controller={controller} {...(treeEntryId === undefined ? {} : { treeEntryId })} /> : null}
        {tab === "events" ? <EventsPanel /> : null}
        {tab === "tools" ? <ToolsPanel /> : null}
        {tab === "terminals" ? <TerminalPanel controller={controller} /> : null}
      </div>
    </aside>
  );
}
