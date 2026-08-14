import { Activity, GitFork, Wrench } from "lucide-react";
import { useState } from "react";

import type { WorkspaceController } from "../../workspace/controller";
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
  const state = useWorkspaceStore();
  const cwd = state.sessions.find((session) => session.sessionId === state.activeSessionId)?.cwd;
  return (
    <aside className="inspector" aria-label="运行详情">
      <div className="inspector-tabs" role="tablist">
        <button data-active={tab === "tree"} type="button" role="tab" onClick={() => setTab("tree")}><GitFork size={14} />Tree</button>
        <button data-active={tab === "events"} type="button" role="tab" onClick={() => setTab("events")}><Activity size={14} />Events</button>
        <button data-active={tab === "tools"} type="button" role="tab" onClick={() => setTab("tools")}><Wrench size={14} />Tools</button>
      </div>
      <div className="inspector-content">
        {tab === "tree" ? <SessionTree {...(state.tree === undefined ? {} : { tree: state.tree })} {...(cwd === undefined ? {} : { cwd })} controller={controller} /> : null}
        {tab === "events" ? <EventLog events={state.events} diagnostics={state.diagnostics} /> : null}
        {tab === "tools" ? <div className="inspector-tools">{state.tools.size === 0 ? <div className="inspector-empty">当前会话没有工具调用。</div> : [...state.tools.values()].map((tool) => <ToolCallCard key={tool.toolCallId} tool={tool} />)}</div> : null}
      </div>
    </aside>
  );
}
