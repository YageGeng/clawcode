import { effect, signal, Text, View } from "nsh";
import { t } from "nsh/i18n";

import { messageOf } from "../client/errors";
import { RemoteDialogs } from "../remote/RemoteDialogs";
import type { RemotePage } from "../remote/RemoteDialogs";
import { remoteError } from "../stores/remote";
import type { RemoteStore } from "../stores/remote";
import { Button } from "../ui/button";
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
export function Shell(props: { store: AppStore; settings: SettingsStore; remote?: RemoteStore }) {
  const sidebarOpen = signal(true);
  const settingsOpen = signal(false);
  // The working directory a new Session starts in; the newest workspace until the user picks one.
  const chosenCwd = signal<ReadonlyMap<string, string>>(new Map());
  const host = () => props.store.currentHost();
  const cwd = () => chosenCwd().get(host().info.id) ?? groupByWorkspace(host().sessions())[0]?.cwd ?? null;
  const remotePage = signal<RemotePage>(null);
  /** Moves between local device settings, invitations and adding a computer. */
  const navigateRemote = (next: RemotePage) => {
    props.remote?.clearError();
    remotePage.set(next);
  };
  const addHost = props.remote ? () => navigateRemote("pair") : undefined;
  const openSessionId = () => props.store.location().sessionId;
  /** Why the host is not ready in `kind`, in the interface language; `undefined` in any other phase. */
  const failureText = (kind: "failed" | "offline") => {
    const phase = host().phase();
    if (phase.kind !== kind) return undefined;
    const failure: HostFailure = (phase as Extract<HostPhase, { failure: HostFailure }>).failure;
    if (host().info.kind === "remote") return t("remote.offline", { name: host().info.name, detail: remoteError(failure.detail) });
    return t(`host.failure.${failure.cause}`, { detail: failure.detail });
  };

  /** Redials a failed or disconnected host using its saved identity and route hints. */
  const retry = () => props.remote?.connect(host().info);
  // The host's notices (a prompt that was not sent) show as toasts; each leaves the host once shown.
  const toasts = new ToastStore();
  /** Keep a failed first prompt editable and explain workspace errors reported by the selected host. */
  const startSession = async (text: string) => {
    try {
      await props.store.startSession(host().info.id, cwd() ?? "", text);
    } catch (error: unknown) {
      toasts.show({ title: t("notice.newSessionFailed"), description: messageOf(error), tone: "danger" });
      throw error;
    }
  };
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
      <TitleBar store={props.store} onToggleSidebar={toggleSidebar}
        connected={props.remote?.snapshot?.enabled ? props.remote.snapshot.online.length : undefined}
        onShowDevices={() => {
          if (host().info.kind !== "local") props.store.selectHost("local");
          navigateRemote("sharing");
        }} />
      <View class="flex-1 flex-row">
        <If when={sidebarOpen()}>
          <Sidebar store={props.store} onAddHost={addHost} onOpenSettings={() => settingsOpen.set(true)} />
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
                <If when={host().info.kind === "remote"}>
                  <Button label={t("remote.retry")} onPress={retry} />
                </If>
              </View>
            )}
          </If>
          {/* Lost after it was ready: say why above the Sessions, which stay on screen (a resend fails into a notice). */}
          <If when={failureText("offline")}>
            {(reason) => (
              <View role="alert" class="border-b border-border px-6 py-2">
                <Text class={`${type.ui} text-destructive`}>{reason()}</Text>
                <If when={host().info.kind === "remote"}>
                  <Button class="self-start" label={t("remote.retry")} onPress={retry} />
                </If>
              </View>
            )}
          </If>
          <If when={host().phase().kind === "reconnecting"}>
            <Text class={`${type.ui} text-muted-foreground px-6 py-2`}>{t("remote.status.reconnecting")}</Text>
          </If>
          <If when={["ready", "offline", "reconnecting"].includes(host().phase().kind)}>
            <If
              when={openSessionId()}
              else={
                <EmptyState
                  host={host()}
                  cwd={cwd()}
                  onCwdChange={(next) => chosenCwd.update((previous) => new Map(previous).set(host().info.id, next))}
                  composer={
                    <Composer
                      host={host()}
                      sessionId={null}
                      disabled={cwd() === null || host().phase().kind !== "ready"}
                      context={<ContextStrip store={props.store} onAddHost={addHost} />}
                      onSend={startSession}
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
      <SettingsDialog open={settingsOpen() && !remotePage() && !props.remote?.snapshot?.pending.length}
        settings={props.settings} onClose={() => settingsOpen.set(false)}
        onRemoteSettings={props.remote && host().info.kind === "local" ? () => navigateRemote("sharing") : undefined} />
      <If when={props.remote}>
        {(remote) => <RemoteDialogs remote={remote()} page={remotePage()}
          localReady={props.store.hosts().some((item) => item.info.kind === "local" && item.phase().kind === "ready")}
          onNavigate={navigateRemote} onPaired={(id) => { navigateRemote(null); props.store.selectHost(id); }} />}
      </If>
      <Toasts store={toasts} />
    </View>
  );
}
