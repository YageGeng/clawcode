import { Activity, GitFork, Wrench } from "lucide-react";
import { useState } from "react";

import type { WorkspaceController } from "../../workspace/controller";
import { initialSessionWorkspaceState } from "../../workspace/sessionState";
import { useWorkspaceStore } from "../../workspace/store";
import { ToolCallCard } from "../conversation/ToolCallCard";
import { EventLog } from "./EventLog";
import { SessionTree } from "./SessionTree";

type InspectorTab = "tree" | "events" | "tools";

export type InspectorProps = Readonly<{
  controller: WorkspaceController;
}>;

export function Inspector({ controller }: InspectorProps) {
  const [tab, setTab] = useState<InspectorTab>("tree");
  const workspace = useWorkspaceStore((state) => state.activeSessionId === undefined
    ? initialSessionWorkspaceState
    : state.sessionWorkspaces.get(state.activeSessionId) ?? initialSessionWorkspaceState);
  const cwd = useWorkspaceStore((state) => state.sessions
    .find((session) => session.sessionId === state.activeSessionId)?.cwd);
  const diagnostics = useWorkspaceStore((state) => state.diagnostics);
  return (
    <aside className="inspector" aria-label="运行详情">
      <div className="inspector-tabs" role="tablist">
        <button data-active={tab === "tree"} type="button" role="tab" onClick={() => setTab("tree")}><GitFork size={14} />Tree</button>
        <button data-active={tab === "events"} type="button" role="tab" onClick={() => setTab("events")}><Activity size={14} />Events</button>
        <button data-active={tab === "tools"} type="button" role="tab" onClick={() => setTab("tools")}><Wrench size={14} />Tools</button>
      </div>
      <div className="inspector-content">
        {tab === "tree" ? <SessionTree {...(workspace.tree === undefined ? {} : { tree: workspace.tree })} {...(cwd === undefined ? {} : { cwd })} controller={controller} /> : null}
        {tab === "events" ? <EventLog events={workspace.events} diagnostics={[...diagnostics, ...workspace.diagnostics]} /> : null}
        {tab === "tools" ? <div className="inspector-tools">{workspace.tools.size === 0 ? <div className="inspector-empty">当前会话没有工具调用。</div> : [...workspace.tools.values()].map((tool) => <ToolCallCard key={tool.toolCallId} tool={tool} />)}</div> : null}
      </div>
    </aside>
  );
}
