import { signal, Text, View } from "nsh";
import { Menu as NativeMenu } from "nsh/desktop";
import { t } from "nsh/i18n";

import type { SessionId } from "../client/acp/protocol";
import type { SessionSummary } from "../client/domain/model";
import { Spinner } from "../ui/feedback";
import { IconButton } from "../ui/button";
import { Icon } from "../ui/icon";
import type { IconName } from "../ui/icon";
import { Input } from "../ui/input";
import { ListItem } from "../ui/list";
import { Menu, MenuItem, MenuSeparator } from "../ui/menu";
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
export function sidebarRows(
  listed: readonly SessionSummary[], expanded: ReadonlySet<string>, pinned: ReadonlySet<SessionId> = new Set(),
): readonly SidebarRow[] {
  // Subagents belong to their owner's details card, not to the conversation list.
  const sessions = listed.filter((session) => session.owner === undefined);
  const rows: SidebarRow[] = [];
  const pins = sessions.filter((session) => pinned.has(session.sessionId));
  if (pins.length) {
    rows.push({ kind: "heading", key: "heading:pinned", label: t("shell.pinned") });
    for (const session of pins) rows.push({ kind: "session", key: `pinned:${session.sessionId}`, session, indented: false });
  }
  rows.push({ kind: "heading", key: "heading:workspaces", label: t("shell.workspaces") });
  for (const group of groupByWorkspace(sessions)) {
    const open = expanded.has(group.cwd);
    rows.push({ kind: "workspace", key: `workspace:${group.cwd}`, cwd: group.cwd, name: group.name, expanded: open });
    if (open) for (const session of group.sessions) rows.push({ kind: "session", key: `workspace-session:${session.sessionId}`, session, indented: true });
  }
  rows.push({ kind: "heading", key: "heading:recent", label: t("shell.recent") });
  // A pinned Session already leads the list; Recent shows the others.
  for (const session of recentSessions(sessions.filter((session) => !pinned.has(session.sessionId)), RECENT_COUNT)) rows.push({ kind: "session", key: `recent:${session.sessionId}`, session, indented: false });
  return rows;
}

/** What a sidebar row needs to know about the app around it. */
export type RowContext = Readonly<{
  selected: (sessionId: SessionId) => boolean;
  running: (sessionId: SessionId) => boolean;
  attentionLabel: (sessionId: SessionId) => string | undefined;
  openSession: (sessionId: SessionId) => void;
  toggleWorkspace: (cwd: string) => void;
  /** Whether this row (by key: a Session can be listed twice) is editing its title in place. */
  renaming: (rowKey: string) => boolean;
  /** Ends an in-place edit: saves `title`, or cancels without one. */
  finishRename: (session: SessionSummary, title?: string) => void;
  /** Whether this row's overflow menu is open, keeping its trigger visible and highlighted. */
  menuOpen: (rowKey: string) => boolean;
  /** Updates which row's overflow menu is currently open. */
  setMenuOpen: (rowKey: string, open: boolean) => void;
  /** The commands of a Session row's hover and context menus, in order. */
  actions: (session: SessionSummary, rowKey: string) => readonly SessionAction[];
}>;

/** One command of a Session row's menus. */
export type SessionAction = Readonly<{ label: string; icon?: IconName; danger?: boolean; onSelect: () => void }>;

/** Edits a Session's title in place: Enter saves, Escape or leaving the field cancels. */
function RenameField(props: { session: SessionSummary; context: RowContext }) {
  const draft = signal(props.session.title);
  let done = false;
  /** Finishes once, whichever of Enter, Escape or blur comes first. */
  const finish = (title?: string) => {
    if (done) return;
    done = true;
    props.context.finishRename(props.session, title);
  };
  return (
    <View onKeyDown={(event) => { if (event.key === "Escape") finish(); }} class="px-0.5 py-1">
      <Input value={draft()} onChange={(value) => draft.set(value)} accessibilityLabel={t("shell.renameField")} size="md"
        autoFocus selectTextOnFocus onSubmit={() => finish(draft())} onBlur={() => finish()} />
    </View>
  );
}

/** Numbers each mounted row cell, so per-row UI state can name the cell that owns it. */
let cellCount = 0;

/** Draws one sidebar row; reads `row` in bindings because list cells are reused for other rows. */
export function SidebarRowView(props: { row: SidebarRow; context: RowContext }) {
  // The list keeps recycled cells mounted with the row they last drew, so a row key alone can match a stale cell
  // too: its menu or rename field would open detached from the row. Scope that state to this cell as well.
  const cell = ++cellCount;
  const stateKey = () => `${cell}\n${props.row.key}`;
  const session = () => (props.row.kind === "session" ? props.row.session : undefined);
  const workspace = () => (props.row.kind === "workspace" ? props.row : undefined);
  return (
    <View>
      <If when={props.row.kind === "heading" ? props.row.label : undefined}>
        {(label) => <Text role="header" class="px-2.5 pt-4 pb-1 text-xs leading-4 font-semibold tracking-wide text-muted-foreground">{label()}</Text>}
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
          // A 2pt gap keeps a hovered row's wash from merging with the selected pill beside it.
          <View class={`py-px ${props.row.kind === "session" && props.row.indented ? "pl-6" : ""}`}
            onContextMenu={(event) => void NativeMenu.popup(
              props.context.actions(item(), stateKey()).map((action) => ({ label: action.label, onSelect: action.onSelect })), event.pagePosition)}>
            <If when={props.context.renaming(stateKey())} else={
            <ListItem
              label={item().title}
              selected={props.context.selected(item().sessionId)}
              actionsVisible={props.context.menuOpen(stateKey())}
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
              actions={
                <Menu align="end" open={props.context.menuOpen(stateKey())}
                  onOpenChange={(open) => props.context.setMenuOpen(stateKey(), open)}
                  trigger={<IconButton icon="ellipsis" size="sm" selected={props.context.menuOpen(stateKey())}
                    label={t("shell.sessionActions", { title: item().title })} />}>
                  <Each of={props.context.actions(item(), stateKey()).filter((action) => !action.danger)} key={(action) => action.label}>
                    {(action) => <MenuItem label={action().label} icon={action().icon} onSelect={action().onSelect} />}
                  </Each>
                  <If when={props.context.actions(item(), stateKey()).some((action) => action.danger)}>
                    <MenuSeparator />
                    <Each of={props.context.actions(item(), stateKey()).filter((action) => action.danger)} key={(action) => action.label}>
                      {(action) => <MenuItem label={action().label} icon={action().icon} danger onSelect={action().onSelect} />}
                    </Each>
                  </If>
                </Menu>
              }
            />
            }>
              <RenameField session={item()} context={props.context} />
            </If>
          </View>
        )}
      </If>
    </View>
  );
}
