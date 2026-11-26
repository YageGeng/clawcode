import { effect, signal, Text, View } from "nsh";
import { t } from "nsh/i18n";

import { Composer } from "../composer/Composer";
import { ContextStrip } from "../composer/ContextStrip";
import { SessionView } from "../session/SessionView";
import { SettingsDialog } from "../settings/SettingsDialog";
import type { AppStore } from "../stores/app";
import type { HostFailure, HostPhase } from "../stores/host";
import type { SettingsStore } from "../stores/settings";
import { glideNextLayout } from "../ui/motion";
import { Toasts, ToastStore } from "../ui/toast";
import { type } from "../ui/tokens";
import { EmptyState } from "./EmptyState";
import { Sidebar } from "./Sidebar";
import { TitleBar } from "./TitleBar";
import { groupByWorkspace } from "./workspaces";

/** The window: title bar, sidebar and the main area (a Session, the empty state, or the host's state). */
export function Shell(props: { store: AppStore; settings: SettingsStore }) {
  const sidebarOpen = signal(true);
  const settingsOpen = signal(false);
  // The working directory a new Session starts in; the newest workspace until the user picks one.
  const chosenCwd = signal<string | null>(null);
  const cwd = () => chosenCwd() ?? groupByWorkspace(props.store.currentHost().sessions())[0]?.cwd ?? null;
  const host = () => props.store.currentHost();
  const openSessionId = () => props.store.location().sessionId;
  /** Why the host is not ready in `kind`, in the interface language; `undefined` in any other phase. */
  const failureText = (kind: "failed" | "offline") => {
    const phase = host().phase();
    if (phase.kind !== kind) return undefined;
    const failure: HostFailure = (phase as Extract<HostPhase, { failure: HostFailure }>).failure;
    return t(`host.failure.${failure.cause}`, { detail: failure.detail });
  };

  // The host's notices (a prompt that was not sent) show as toasts; each leaves the host once shown.
  const toasts = new ToastStore();
  effect(() => {
    for (const notice of host().notices()) {
      toasts.show({ title: notice.title, description: notice.description, tone: "danger" });
      host().dismissNotice(notice.id);
    }
  });

  /** Shows or hides the sidebar, gliding the layout unless the user asked for less motion. */
  const toggleSidebar = () => {
    glideNextLayout();
    sidebarOpen.update((open) => !open);
  };

  return (
    <View class="flex-1 bg-sidebar">
      <TitleBar store={props.store} onToggleSidebar={toggleSidebar} />
      <View class="flex-1 flex-row">
        <If when={sidebarOpen()}>
          <Sidebar store={props.store} onOpenSettings={() => settingsOpen.set(true)} />
        </If>
        <View class={`flex-1 bg-background ${sidebarOpen() ? "rounded-tl-(--radius-lg) border-l border-t border-border" : "border-t border-border"}`}>
          <If when={host().phase().kind === "starting"}>
            <View class="flex-1 items-center justify-center">
              <Text class={`${type.ui} text-muted-foreground`}>{t("shell.starting")}</Text>
            </View>
          </If>
          <If when={failureText("failed")}>
            {(reason) => (
              <View class="flex-1 items-center justify-center px-6">
                <Text class={`${type.ui} text-destructive text-center`}>{reason()}</Text>
              </View>
            )}
          </If>
          {/* Lost after it was ready: say why above the Sessions, which stay on screen (a resend fails into a notice). */}
          <If when={failureText("offline")}>
            {(reason) => (
              <View role="alert" class="border-b border-border px-6 py-2">
                <Text class={`${type.ui} text-destructive`}>{reason()}</Text>
              </View>
            )}
          </If>
          <If when={host().phase().kind === "ready" || host().phase().kind === "offline"}>
            <If
              when={openSessionId()}
              else={
                <EmptyState
                  host={host()}
                  cwd={cwd()}
                  onCwdChange={(next) => chosenCwd.set(next)}
                  composer={
                    <Composer
                      host={host()}
                      sessionId={null}
                      disabled={cwd() === null}
                      context={<ContextStrip store={props.store} />}
                      onSend={(text) => props.store.startSession(host().info.id, cwd() ?? "", text)}
                    />
                  }
                />
              }
            >
              {/* Keyed by host and Session: `If` keeps its branch while it stays truthy, so without the key one view
                  (its draft, scroll anchor and jump state) would carry over into the next Session. */}
              <Each of={[openSessionId()!]} key={(id) => `${host().info.id}\n${id}`}>
                {(sessionId) => <SessionView host={host()} sessionId={sessionId()} />}
              </Each>
            </If>
          </If>
        </View>
      </View>
      <SettingsDialog open={settingsOpen()} settings={props.settings} onClose={() => settingsOpen.set(false)} />
      <Toasts store={toasts} />
    </View>
  );
}
