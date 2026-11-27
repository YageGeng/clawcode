import { effect, signal, Text, View } from "nsh";
import { currentWindow, isAvailable } from "nsh/desktop";
import { t } from "nsh/i18n";

import { messageOf } from "../client/errors";
import { RemoteDialogs } from "../remote/RemoteDialogs";
import type { RemotePage } from "../remote/RemoteDialogs";
import { remoteError } from "../stores/remote";
import type { RemoteStore } from "../stores/remote";
import { Button, IconButton } from "../ui/button";
import { SessionView } from "../session/SessionView";
import { SettingsDialog } from "../settings/SettingsDialog";
import type { AppStore } from "../stores/app";
import type { HostFailure, HostPhase } from "../stores/host";
import { createPinStore } from "../stores/pins";
import type { SettingsStore } from "../stores/settings";
import { configureDesktopFonts } from "../ui/fonts";
import { Tooltip } from "../ui/menu";
import { glideNextLayout } from "../ui/motion";
import { StatusDot } from "../ui/surface";
import { Toasts, ToastStore } from "../ui/toast";
import { focusRing, type } from "../ui/tokens";
import { EmptyState } from "./EmptyState";
import { Sidebar } from "./Sidebar";
import { NavBar } from "./NavBar";
import { groupByWorkspace } from "./workspaces";

/** Minimum desktop window size, matching `nsh.json`'s `window.minSize`. */
const MIN_WINDOW_SIZE = { width: 800, height: 560 } as const;

/** Applies desktop chrome and size constraints. NSH restores the saved bounds at launch. */
function configureShellWindow(): void {
  if (!isAvailable) return;
  const win = currentWindow();
  // The system draws the title bar (KDE's own on Wayland): its buttons, menu and double click match every other window.
  win.set({
    title: "Clawcode",
    minSize: MIN_WINDOW_SIZE,
    titleBar: { style: "default" },
  });
  // Some hosts create the window before applying minSize. Clamp its first reported size per axis.
  let checkedLaunchBounds = false;
  effect(() => {
    if (checkedLaunchBounds) return;
    const state = win.state();
    if (!state) return;
    checkedLaunchBounds = true;
    const { width, height } = state.contentBounds.size;
    if (width < MIN_WINDOW_SIZE.width || height < MIN_WINDOW_SIZE.height) {
      win.setBounds({ size: {
        width: Math.max(width, MIN_WINDOW_SIZE.width),
        height: Math.max(height, MIN_WINDOW_SIZE.height),
      } });
    }
  });
}

/** The window below the system title bar: the sidebar headed by navigation, and the main area (a Session, the empty
 * state, or the host's state). */
export function Shell(props: { store: AppStore; settings: SettingsStore; remote?: RemoteStore }) {
  configureDesktopFonts();
  configureShellWindow();
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
  // Only these phases mount the open Session's view; any other phase shows a status in its place.
  const sessionShown = () => openSessionId() !== null && ["ready", "offline", "reconnecting"].includes(host().phase().kind);
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
  // Pins live on this device: the host has no notion of them.
  const pins = createPinStore();
  void pins.load();
  effect(() => {
    for (const notice of host().notices()) {
      toasts.show({ title: notice.title, description: notice.description, tone: "danger" });
      host().dismissNotice(notice.id);
    }
  });

  /**
   * The host connection as the Session header shows it: a tinted pulsing pill while reconnecting; a focusable danger
   * pill (carrying the reason in its own tooltip, separate from the retry button) once offline. Nothing while connected.
   */
  const connection = () => {
    if (host().phase().kind === "reconnecting") {
      return (
        <View role="alert" class="flex-row items-center rounded-full bg-warning/10 px-2 py-0.5">
          <StatusDot tone="warning" live label={t("remote.status.reconnecting")} />
        </View>
      );
    }
    const reason = failureText("offline");
    if (reason === undefined) return undefined;
    return (
      <View role="alert" class="flex-row items-center gap-1">
        <View
          focusable
          accessibilityLabel={t("remote.status.offline")}
          accessibilityHint={reason}
          tooltip={<Tooltip text={reason} />}
          class={`flex-row items-center rounded-full bg-destructive/10 px-2 py-0.5 ${focusRing}`}
        >
          <StatusDot tone="danger" label={t("remote.status.offline")} />
        </View>
        <If when={host().info.kind === "remote"}>
          <IconButton icon="refresh-cw" size="sm" label={t("remote.retry")} tooltip={<Tooltip text={t("remote.retry")} />} onPress={retry} />
        </If>
      </View>
    );
  };
  /** The navigation row: it heads the sidebar, or the main area while the sidebar is hidden. */
  const nav = () => (
    <NavBar store={props.store} onToggleSidebar={toggleSidebar}
      connected={props.remote?.snapshot?.enabled ? props.remote.snapshot.online.length : undefined}
      onShowDevices={() => {
        if (host().info.kind !== "local") props.store.selectHost("local");
        navigateRemote("sharing");
      }} />
  );
  /** Shows or hides the sidebar, gliding the layout unless the user asked for less motion. */
  const toggleSidebar = () => {
    glideNextLayout();
    sidebarOpen.update((open) => !open);
  };

  return (
    <View class="flex-1 bg-sidebar">
      <View class="min-h-0 flex-1 flex-row">
        <If when={sidebarOpen()}>
          <Sidebar header={nav()} store={props.store} pins={pins} onAddHost={addHost} onOpenSettings={() => settingsOpen.set(true)}
            onError={(title, description) => toasts.show({ title, description, tone: "danger" })} />
        </If>
        {/* Below the system title bar the sidebar and the main area meet flush; one vertical line divides them. */}
        <View class={`min-w-0 flex-1 overflow-hidden bg-background ${sidebarOpen() ? "border-l border-border" : ""}`}>
          {/* Without the sidebar, the navigation buttons lead the main area's first row: the Session header's when one
              is shown, otherwise their own row above whatever status replaces it. */}
          <If when={!sidebarOpen() && !sessionShown()}>{nav()}</If>
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
          {/* Lost after it was ready: say why above the Sessions, which stay on screen (a resend fails into a notice).
              An open Session shows the same state compactly in its header instead. */}
          <If when={sessionShown() ? undefined : failureText("offline")}>
            {(reason) => (
              <View role="alert" class="border-b border-border px-6 py-2">
                <Text class={`${type.ui} text-destructive`}>{reason()}</Text>
                <If when={host().info.kind === "remote"}>
                  <Button class="self-start" label={t("remote.retry")} onPress={retry} />
                </If>
              </View>
            )}
          </If>
          <If when={!sessionShown() && host().phase().kind === "reconnecting"}>
            <Text class={`${type.ui} text-muted-foreground px-6 py-2`}>{t("remote.status.reconnecting")}</Text>
          </If>
          <If when={["ready", "offline", "reconnecting"].includes(host().phase().kind)}>
            {/* The navigation entry owns each blank draft. A host retry preserves it and New session replaces it. */}
            <If
              when={openSessionId()}
              else={
                <Each of={[props.store.location()]}>
                  {() => <EmptyState store={props.store} cwd={cwd()} onAddHost={addHost}
                    onCwdChange={(next) => chosenCwd.update((previous) => new Map(previous).set(host().info.id, next))}
                    onStartError={(error) => toasts.show({ title: t("notice.newSessionFailed"), description: messageOf(error), tone: "danger" })} />}
                </Each>
              }
            >
              {/* Keyed by host and Session: `If` keeps its branch while it stays truthy, so without the key one view
                  (its draft, scroll anchor and jump state) would carry over into the next Session. */}
              <Each of={[openSessionId()!]} key={(id) => `${host().info.id}\n${id}`}>
                {(sessionId) => (
                  <SessionView
                    host={host()}
                    sessionId={sessionId()}
                    settings={props.settings}
                    deviceName={props.remote?.snapshot?.name}
                    onOpenSession={(id) => props.store.openSession(host().info.id, id)}
                    leading={sidebarOpen() ? undefined : nav()}
                    status={connection()}
                    modalOpen={settingsOpen() || remotePage() !== null || !!props.remote?.snapshot?.pending.length}
                  />
                )}
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
