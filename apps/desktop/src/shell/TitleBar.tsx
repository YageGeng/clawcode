import { TitleBar as BaseTitleBar } from "nsh/base";
import { t } from "nsh/i18n";

import type { AppStore } from "../stores/app";
import { IconButton } from "../ui/button";
import { Tooltip } from "../ui/menu";

/** The title bar's height in points; the window's hidden system bar takes the same. */
export const TITLE_BAR_HEIGHT = 44;

/** The app-drawn title bar: sidebar toggle and back/forward; the rest drags the window. */
export function TitleBar(props: { store: AppStore; onToggleSidebar: () => void }) {
  // NSH reserves space on whichever side the system places its window controls.
  return (
    <BaseTitleBar
      class="flex-row items-center gap-1 bg-sidebar px-2"
      style={{ height: TITLE_BAR_HEIGHT }}
    >
      <IconButton icon="panel-left" label={t("shell.toggleSidebar")} tooltip={<Tooltip text={t("shell.toggleSidebar")} />} onPress={props.onToggleSidebar} />
      <IconButton icon="arrow-left" label={t("shell.back")} disabled={!props.store.canGoBack()} onPress={props.store.back} />
      <IconButton icon="arrow-right" label={t("shell.forward")} disabled={!props.store.canGoForward()} onPress={props.store.forward} />
    </BaseTitleBar>
  );
}
