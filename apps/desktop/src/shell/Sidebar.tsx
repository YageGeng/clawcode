import { computed, effect, List, signal, View } from "nsh";
import { t } from "nsh/i18n";
import type { JSX } from "nsh/jsx-runtime";

import type { SessionId } from "../client/acp/protocol";
import type { SessionSummary } from "../client/domain/model";
import { messageOf } from "../client/errors";
import type { AppStore } from "../stores/app";
import type { PinStore } from "../stores/pins";
import { confirm } from "../ui/dialog";
import { Button } from "../ui/button";
import { ListItem } from "../ui/list";
import { HostSwitcher } from "./HostSwitcher";
import { SidebarRowView, sidebarRows } from "./SessionRows";
import { groupByWorkspace } from "./workspaces";

/** The sidebar: the computer, New session, workspaces with their Sessions, recent Sessions and Settings. */
export function Sidebar(props: {
  /** The navigation row above the host switcher, level with the session title. */
  header?: JSX.Element;
  store: AppStore; pins: PinStore; onOpenSettings: () => void; onAddHost?: () => void;
  onError: (title: string, detail: string) => void;
}) {
  const host = () => props.store.currentHost();
  // Which workspace groups are open; kept here by cwd, never in a list cell (cells are reused).
  const expanded = signal<ReadonlySet<string>>(new Set());
  // The newest workspace starts open, once its Sessions have arrived.
  const remembered = new Map<string, ReadonlySet<string>>();
  let previous: string | undefined;
  effect(() => {
    const id = host().info.id;
    const newest = groupByWorkspace(host().sessions())[0];
    if (id === previous && remembered.has(id)) return;
    previous = id;
    if (newest && !remembered.has(id)) remembered.set(id, new Set([newest.cwd]));
    expanded.set(remembered.get(id) ?? new Set());
  });
  const rows = computed(() => sidebarRows(host().sessions(), expanded(), props.pins.pinned(host().info.id)));
  const renaming = signal<string | null>(null);
  const openMenu = signal<string | null>(null);
  /** Deletes after the user confirms; an open Session gives way to a new draft first. */
  const remove = async (session: SessionSummary) => {
    const current = host();
    const yes = await confirm({ title: t("shell.deleteTitle", { title: session.title }), description: t("shell.deleteDetail"),
      action: t("shell.delete"), danger: true });
    if (!yes) return;
    const location = props.store.location();
    if (location.hostId === current.info.id && location.sessionId === session.sessionId) props.store.beginDraft();
    try {
      await current.deleteSession(session.sessionId);
      if (props.pins.pinned(current.info.id).has(session.sessionId)) props.pins.toggle(current.info.id, session.sessionId);
    } catch (error) {
      props.onError(t("shell.deleteFailed"), messageOf(error));
    }
  };
  const context = {
    selected: (sessionId: SessionId) => props.store.location().hostId === host().info.id && props.store.location().sessionId === sessionId,
    running: (sessionId: SessionId) => host().running(sessionId),
    /** Approval takes precedence when the same Session also has unanswered questions. */
    attentionLabel: (sessionId: SessionId) => {
      const current = host();
      if (current.phase().kind !== "ready") return undefined;
      if (current.permissionFor(sessionId)) return t("session.needsAttention");
      return current.awaitsAnswer(sessionId) ? t("session.awaitsAnswer") : undefined;
    },
    openSession: (sessionId: SessionId) => props.store.openSession(host().info.id, sessionId),
    renaming: (rowKey: string) => renaming() === rowKey,
    finishRename: (session: SessionSummary, title?: string) => {
      renaming.set(null);
      const next = title?.trim();
      if (!next || next === session.title) return;
      const current = host();
      current.renameSession(session.sessionId, next).catch((error: unknown) => props.onError(t("shell.renameFailed"), messageOf(error)));
    },
    menuOpen: (rowKey: string) => openMenu() === rowKey,
    setMenuOpen: (rowKey: string, open: boolean) => openMenu.set(open ? rowKey : openMenu() === rowKey ? null : openMenu()),
    actions: (session: SessionSummary, rowKey: string) => {
      const id = host().info.id;
      const pinned = props.pins.pinned(id).has(session.sessionId);
      return [
        { label: t("shell.rename"), icon: "pencil" as const, onSelect: () => { openMenu.set(null); renaming.set(rowKey); } },
        { label: t(pinned ? "shell.unpin" : "shell.pin"), icon: pinned ? ("pin-off" as const) : ("pin" as const), onSelect: () => { openMenu.set(null); props.pins.toggle(id, session.sessionId); } },
        { label: t("shell.delete"), icon: "trash-2" as const, danger: true, onSelect: () => { openMenu.set(null); void remove(session); } },
      ];
    },
    toggleWorkspace: (cwd: string) =>
      expanded.update((open) => {
        const next = new Set(open);
        if (!next.delete(cwd)) next.add(cwd);
        remembered.set(host().info.id, next);
        return next;
      }),
  };
  return (
    <View class="w-[280px] gap-1.5 bg-sidebar px-2.5 pb-2.5">
      <View class="-mx-2.5">{props.header}</View>
      <View class="items-start pb-0.5">
        <HostSwitcher store={props.store} onAddHost={props.onAddHost} />
      </View>
      <If when={host().phase().kind === "ready"}>
        <Button label={t("shell.newSession")} icon="square-pen" variant="outline" class="w-full justify-start shadow-2xs" onPress={props.store.beginDraft} />
      </If>
      <List each={rows()} keyExtractor={(row) => row.key} estimatedItemSize={32} class="flex-1">
        {(row) => <SidebarRowView row={row()} context={context} />}
      </List>
      <View class="border-t border-border pt-2">
        <ListItem label={t("shell.settings")} icon="settings" onPress={props.onOpenSettings} />
      </View>
    </View>
  );
}
