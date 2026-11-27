import { View } from "nsh";
import { t } from "nsh/i18n";

import type { AppStore } from "../stores/app";
import { Button, IconButton } from "../ui/button";
import { Tooltip } from "../ui/menu";

/** The navigation buttons below the system title bar: sidebar toggle, back/forward and remote devices. */
export function NavBar(props: { store: AppStore; onToggleSidebar: () => void; connected?: number; onShowDevices?: () => void }) {
  return (
    <View class="h-12 shrink-0 flex-row items-center gap-1 px-2">
      <IconButton icon="panel-left" label={t("shell.toggleSidebar")} tooltip={<Tooltip text={t("shell.toggleSidebar")} />} onPress={props.onToggleSidebar} />
      <IconButton icon="arrow-left" label={t("shell.back")} tooltip={<Tooltip text={t("shell.back")} />}
        disabled={!props.store.canGoBack()} onPress={props.store.back} />
      <IconButton icon="arrow-right" label={t("shell.forward")} tooltip={<Tooltip text={t("shell.forward")} />}
        disabled={!props.store.canGoForward()} onPress={props.store.forward} />
      <If when={props.connected !== undefined}>
        <Button size="sm" variant="ghost" icon="shield-check" label={t("remote.connections", { count: props.connected ?? 0 })}
          class="min-w-0 shrink" onPress={props.onShowDevices} />
      </If>
    </View>
  );
}
