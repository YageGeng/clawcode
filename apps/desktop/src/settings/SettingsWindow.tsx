import { Scroll, Text, View } from "nsh";
import { Window } from "nsh/desktop";
import type { WindowRef } from "nsh/desktop";
import { t } from "nsh/i18n";

import { SharingPage } from "../remote/SharingPage";
import type { HostStore } from "../stores/host";
import type { RemoteStore } from "../stores/remote";
import type { SettingsStore } from "../stores/settings";
import type { IconName } from "../ui/icon";
import { ListItem } from "../ui/list";
import { type } from "../ui/tokens";
import { AboutPage } from "./AboutPage";
import { AppearancePage, GeneralPage } from "./GeneralPages";
import { ProvidersPage } from "./ProvidersPage";

/** The pages of the settings window, in navigation order. */
export const SETTINGS_PAGES = ["general", "appearance", "providers", "remote", "about"] as const;
/** One page of the settings window. */
export type SettingsPage = (typeof SETTINGS_PAGES)[number];

/** Each page's navigation icon. */
const pageIcons: Readonly<Record<SettingsPage, IconName>> = {
  general: "settings", appearance: "sun", providers: "shield-check", remote: "laptop", about: "info",
};

/**
 * Settings in a window of their own: page navigation on the left, the page on the right. It shares the app's stores,
 * so a change applies to the main window at once. Rendered only while open: a `<Window>` in the root at mount would
 * keep NSH from opening the implicit main window.
 */
export function SettingsWindow(props: {
  settings: SettingsStore;
  /** The local host, whose providers the Providers page manages. */
  localHost?: HostStore;
  remote?: RemoteStore;
  page: SettingsPage;
  onPage: (page: SettingsPage) => void;
  onClose: () => void;
  windowRef?: (window: WindowRef) => void;
  /** A sign-in to begin on the Providers page, asked for by a notice. */
  startSignIn?: string;
  onSignInStarted?: () => void;
}) {
  const pages = () => SETTINGS_PAGES.filter((page) => page !== "remote" || props.remote !== undefined);
  return (
    <Window id="settings" title={t("settings.title")} defaultSize={{ width: 860, height: 600 }} minSize={{ width: 640, height: 480 }}
      ref={(node) => props.windowRef?.(node)} onOpenChange={(event) => { if (!event.open) props.onClose(); }}>
      <View class="flex-1 flex-row bg-background">
        <View role="list" accessibilityLabel={t("settings.title")} class="w-[208px] gap-0.5 border-r border-border bg-sidebar px-2.5 py-4">
          <Each of={pages()} key={(page) => page}>
            {(page) => (
              <ListItem label={t(`settings.pages.${page()}`)} icon={pageIcons[page()]} selected={props.page === page()}
                onPress={() => props.onPage(page())} />
            )}
          </Each>
        </View>
        <Scroll class="min-w-0 flex-1" contentClass="w-full max-w-[640px] gap-5 px-8 py-6">
          <Text role="header" class={`${type.title} tracking-tight text-foreground`}>{t(`settings.pages.${props.page}`)}</Text>
          <If when={props.page === "general"}><GeneralPage settings={props.settings} /></If>
          <If when={props.page === "appearance"}><AppearancePage settings={props.settings} /></If>
          <If when={props.page === "providers"}><ProvidersPage host={props.localHost} startSignIn={props.startSignIn} onStarted={props.onSignInStarted} /></If>
          <If when={props.page === "remote" ? props.remote : undefined}>
            {(remote) => <SharingPage remote={remote()} localReady={props.localHost?.phase().kind === "ready"} />}
          </If>
          <If when={props.page === "about"}><AboutPage /></If>
        </Scroll>
      </View>
    </Window>
  );
}
