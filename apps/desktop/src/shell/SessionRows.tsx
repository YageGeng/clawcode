import { Text, View } from "nsh";
import { t } from "nsh/i18n";

import type { SessionId } from "../client/acp/protocol";
import type { SessionSummary } from "../client/domain/model";
import { Spinner } from "../ui/feedback";
import { Icon } from "../ui/icon";
import { ListItem } from "../ui/list";
import { StatusDot } from "../ui/surface";
import { groupByWorkspace, recentSessions } from "./workspaces";
import { motionClass } from "../ui/motion";

/** How many Sessions "Recent" lists. */
const RECENT_COUNT = 10;

/** One row of the sidebar's list: a section heading, a workspace group, or a Session. */
export type SidebarRow =
  | Readonly<{ kind: "heading"; key: string; label: string }>
  | Readonly<{ kind: "workspace"; key: string; cwd: string; name: string; expanded: boolean }>
  | Readonly<{ kind: "session"; key: string; session: SessionSummary; indented: boolean }>;

/** The sidebar's rows: workspaces (each expanded group followed by its Sessions), then the recent Sessions. */
export function sidebarRows(sessions: readonly SessionSummary[], expanded: ReadonlySet<string>): readonly SidebarRow[] {
  const rows: SidebarRow[] = [{ kind: "heading", key: "heading:workspaces", label: t("shell.workspaces") }];
  for (const group of groupByWorkspace(sessions)) {
    const open = expanded.has(group.cwd);
    rows.push({ kind: "workspace", key: `workspace:${group.cwd}`, cwd: group.cwd, name: group.name, expanded: open });
    if (open) for (const session of group.sessions) rows.push({ kind: "session", key: `workspace-session:${session.sessionId}`, session, indented: true });
  }
  rows.push({ kind: "heading", key: "heading:recent", label: t("shell.recent") });
  for (const session of recentSessions(sessions, RECENT_COUNT)) rows.push({ kind: "session", key: `recent:${session.sessionId}`, session, indented: false });
  return rows;
}

/** What a sidebar row needs to know about the app around it. */
export type RowContext = Readonly<{
  selected: (sessionId: SessionId) => boolean;
  running: (sessionId: SessionId) => boolean;
  attentionLabel: (sessionId: SessionId) => string | undefined;
  openSession: (sessionId: SessionId) => void;
  toggleWorkspace: (cwd: string) => void;
}>;

/** Draws one sidebar row; reads `row` in bindings because list cells are reused for other rows. */
export function SidebarRowView(props: { row: SidebarRow; context: RowContext }) {
  const session = () => (props.row.kind === "session" ? props.row.session : undefined);
  const workspace = () => (props.row.kind === "workspace" ? props.row : undefined);
  return (
    <View>
      <If when={props.row.kind === "heading" ? props.row.label : undefined}>
        {(label) => <Text class="px-2.5 pt-4 pb-1 text-xs leading-4 font-semibold tracking-wide text-muted-foreground">{label()}</Text>}
      </If>
      <If when={workspace()}>
        {(group) => (
          <ListItem
            label={group().name}
            icon="folder"
            onPress={() => props.context.toggleWorkspace(group().cwd)}
            trailing={
              <View class={`${group().expanded ? "rotate-90" : ""} ${motionClass.turn}`}>
                <Icon name="chevron-right" size="sm" color="muted-foreground" />
              </View>
            }
          />
        )}
      </If>
      <If when={session()}>
        {(item) => (
          <View class={props.row.kind === "session" && props.row.indented ? "pl-6" : ""}>
            <ListItem
              label={item().title}
              selected={props.context.selected(item().sessionId)}
              onPress={() => props.context.openSession(item().sessionId)}
              trailing={
                // Waiting on the user wins over running (a request always comes mid-turn). `If` builds a marker
                // only when it appears, so a streaming Session does not restart its spinner on every update.
                <If
                  when={props.context.attentionLabel(item().sessionId)}
                  else={
                    <If when={props.context.running(item().sessionId)}>
                      <Spinner label={t("session.running")} size="sm" />
                    </If>
                  }
                >
                  {(label) => <StatusDot tone="info" label={label()} hideLabel />}
                </If>
              }
            />
          </View>
        )}
      </If>
    </View>
  );
}
