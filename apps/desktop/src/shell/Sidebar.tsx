import { computed, effect, List, signal, View } from "nsh";
import { t } from "nsh/i18n";

import type { SessionId } from "../client/acp/protocol";
import type { AppStore } from "../stores/app";
import { ListItem } from "../ui/list";
import { HostSwitcher } from "./HostSwitcher";
import { SidebarRowView, sidebarRows } from "./SessionRows";
import { groupByWorkspace } from "./workspaces";

/** The sidebar: the computer, New session, workspaces with their Sessions, recent Sessions and Settings. */
export function Sidebar(props: { store: AppStore; onOpenSettings: () => void }) {
  const host = () => props.store.currentHost();
  // Which workspace groups are open; kept here by cwd, never in a list cell (cells are reused).
  const expanded = signal<ReadonlySet<string>>(new Set());
  // The newest workspace starts open, once its Sessions have arrived.
  let seeded = false;
  effect(() => {
    const newest = seeded ? undefined : groupByWorkspace(host().sessions())[0];
    if (newest === undefined) return;
    seeded = true;
    expanded.set(new Set([newest.cwd]));
  });
  const rows = computed(() => sidebarRows(host().sessions(), expanded()));
  const context = {
    selected: (sessionId: SessionId) => props.store.location().hostId === host().info.id && props.store.location().sessionId === sessionId,
    running: (sessionId: SessionId) => host().running(sessionId),
    needsAttention: (sessionId: SessionId) => host().permissionFor(sessionId) !== undefined,
    openSession: (sessionId: SessionId) => props.store.openSession(host().info.id, sessionId),
    toggleWorkspace: (cwd: string) =>
      expanded.update((open) => {
        const next = new Set(open);
        if (!next.delete(cwd)) next.add(cwd);
        return next;
      }),
  };
  return (
    <View class="w-[280px] gap-1 bg-sidebar px-2 pb-2">
      <View class="items-start px-0.5 pb-1">
        <HostSwitcher store={props.store} />
      </View>
      <If when={host().phase().kind === "ready"}>
        <ListItem label={t("shell.newSession")} icon="square-pen" onPress={() => props.store.selectHost(host().info.id)} />
      </If>
      <List each={rows()} keyExtractor={(row) => row.key} estimatedItemSize={32} class="flex-1">
        {(row) => <SidebarRowView row={row()} context={context} />}
      </List>
      <ListItem label={t("shell.settings")} icon="settings" onPress={props.onOpenSettings} />
    </View>
  );
}
