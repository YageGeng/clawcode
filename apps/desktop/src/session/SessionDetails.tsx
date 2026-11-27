import { ExternalEditors } from "clawcode-backend";
import type { OpenTarget } from "clawcode-backend";
import { cleanup, computed, effect, Image, Scroll, signal, Text, View } from "nsh";
import { t } from "nsh/i18n";
import type { JSX } from "nsh/jsx-runtime";

import type { SessionId } from "../client/acp/protocol";
import type { SessionSummary } from "../client/domain/model";
import { messageOf } from "../client/errors";
import { sessionRelations, subagentActivity } from "../client/relations";
import type { HostStore } from "../stores/host";
import { FILE_MANAGER } from "../stores/settings";
import type { SettingsStore } from "../stores/settings";
import { IconButton } from "../ui/button";
import { AgentAvatar } from "../ui/agentAvatar";
import { Icon } from "../ui/icon";
import type { IconName } from "../ui/icon";
import { ListItem } from "../ui/list";
import { Tooltip } from "../ui/menu";
import { motionClass } from "../ui/motion";
import { Separator } from "../ui/surface";
import { inlineLabel, type } from "../ui/tokens";

/** All local "Open in" targets (file manager first, then editors on `PATH`), read once at startup. */
const openTargets: readonly OpenTarget[] = ExternalEditors.targets();
/** Detected external editors, excluding the system file manager entry. */
const installedEditors: readonly OpenTarget[] = openTargets.filter((target) => target.command !== FILE_MANAGER);

/** Fallback product names when a detected editor has no desktop entry name on this system. */
const EDITOR_NAMES: Readonly<Record<string, string>> = {
  zed: "Zed",
  code: "VS Code",
  cursor: "Cursor",
  codium: "VSCodium",
  subl: "Sublime Text",
};

/**
 * Formats an `OpenTarget`: a known editor's short product name first ("VS Code" fits the 280 pt card where the desktop
 * entry's "Visual Studio Code" overflows), then its desktop entry name, then the raw command.
 */
function targetLabel(target: OpenTarget): string {
  if (target.command === FILE_MANAGER) return target.name || t("details.fileManager");
  return EDITOR_NAMES[target.command] || target.name || target.command;
}

/** Draws a target's desktop icon when available, falling back to a semantic Lucide glyph. */
function TargetIcon(props: { target: OpenTarget }) {
  const fallback: IconName = props.target.command === FILE_MANAGER ? "folder" : "external-link";
  return (
    <If when={props.target.icon} else={<Icon name={fallback} size="md" color="muted-foreground" />}>
      {(src) => <Image src={src()} fadeDuration={0} accessibilityElementsHidden style={{ width: 16, height: 16 }} />}
    </If>
  );
}

/** How long ago `modifiedAtMs` was, in the largest whole unit ("2 min", "3 h"). */
function elapsed(modifiedAtMs: string | undefined): string {
  if (modifiedAtMs === undefined) return "";
  // ponytail: computed at render, not ticking; add a minute timer if stale labels bother anyone.
  const minutes = Math.floor((Date.now() - Number(modifiedAtMs)) / 60_000);
  if (minutes < 1) return t("details.justNow");
  if (minutes < 60) return t("details.minutes", { n: minutes });
  if (minutes < 24 * 60) return t("details.hours", { n: Math.floor(minutes / 60) });
  return t("details.days", { n: Math.floor(minutes / (24 * 60)) });
}

/** One static row: an icon, a label, and trailing controls aligned on the card's right action spine (`pr-1`). */
function Row(props: { icon: IconName; label: string; children?: JSX.Element }) {
  return (
    <View class="min-h-8 flex-row items-center gap-2 pl-2.5 pr-1">
      <Icon name={props.icon} size="md" color="muted-foreground" />
      <Text class={`${inlineLabel} ${type.ui} min-w-0 flex-1 truncate text-foreground`}>{props.label}</Text>
      {props.children}
    </View>
  );
}

/**
 * What a session runs on and what it relates to: the host with "Open in" for local folders, then the parent agent,
 * fork source and subagents. The caller places it, floating beside a wide transcript or in a header popover.
 */
export function SessionDetails(props: {
  host: HostStore; sessionId: SessionId; cwd?: string; deviceName?: string; settings: SettingsStore;
  onOpenSession: (sessionId: SessionId) => void;
}) {
  const ready = computed(() => props.host.phase().kind === "ready");
  const local = computed(() => props.host.info.kind === "local");
  const queried = signal<ReadonlySet<SessionId>>(new Set());
  const openError = signal<string | undefined>(undefined);
  const pickerOpen = signal(false);
  let mounted = true;
  let queryVersion = 0;
  cleanup(() => { mounted = false; });

  // Membership, not object identity: title updates must not re-query every subagent.
  const childIds = computed(
    () => props.host.sessions().filter((session) => session.owner?.sessionId === props.sessionId).map((session) => session.sessionId),
    { equals: (left, right) => left.length === right.length && left.every((id, index) => id === right[index]) },
  );
  const parentRunning = computed(() => props.host.running(props.sessionId));
  // Subagents are rarely open here, so ask the host which still work when the set changes or the parent starts or stops.
  effect(() => {
    const ids = childIds();
    parentRunning();
    if (!ready() || ids.length === 0) return;
    const version = ++queryVersion;
    const host = props.host;
    void Promise.all(ids.map((id) => host.queryRunning(id).catch(() => false))).then((running) => {
      if (mounted && version === queryVersion) queried.set(new Set(ids.filter((_, index) => running[index])));
    });
  });
  const relations = computed(() => sessionRelations(props.host.sessions(), props.sessionId,
    // An offline host's last answers may be stale, so nothing is shown as running.
    ready() ? subagentActivity(props.host, queried()) : () => false));

  // A saved editor that is no longer installed falls back to the first installed editor (or the file manager).
  const activeTarget = computed(() => {
    const saved = props.settings.settings().openIn;
    return installedEditors.find((target) => target.command === saved) ?? installedEditors[0] ?? openTargets[0];
  });
  /** Opens the local workspace in `target`; a launch failure stays visible in the card. */
  const openWorkspace = (target: string) => {
    const root = props.cwd;
    if (!root) return;
    try {
      ExternalEditors.open(root, target);
      openError.set(undefined);
    } catch (error) {
      console.warn(`open in ${target} failed: ${messageOf(error)}`);
      openError.set(t("details.openFailed", { detail: messageOf(error) }));
    }
  };
  /** Toggles the inline editor chooser inside the card so it never stacks a menu above a header popover. */
  const togglePicker = () => pickerOpen.update((open) => !open);

  /** One related Session; it reads the summary through `session` so title and nickname changes show at once. */
  const RelatedRow = (row: { session: () => SessionSummary; icon?: IconName; running?: boolean; description?: string }) => (
    <ListItem label={row.session().owner?.nickname ?? row.session().title} description={row.description}
      accessibilityLabel={row.running ? t("details.runningRow", { name: row.session().owner?.nickname ?? row.session().title }) : undefined}
      leading={row.session().owner ? <AgentAvatar name={row.session().owner!.nickname} running={row.running} />
        : <Icon name={row.icon ?? "circle-check"} size="md" color="muted-foreground" />}
      trailing={<Text class={`shrink-0 tabular-nums ${inlineLabel} ${type.meta} text-muted-foreground`}>{elapsed(row.session().modifiedAtMs)}</Text>}
      onPress={() => props.onOpenSession(row.session().sessionId)} />
  );

  return (
    <View class="gap-1.5">
      <View class="gap-0.5">
        <Row icon="laptop" label={local() ? props.deviceName ?? t("shell.thisComputer") : props.host.info.name}>
          <If when={local() && props.cwd}>
            <IconButton icon="folder" label={t("details.openFileManager")} size="sm" tooltip={<Tooltip text={t("details.openFileManager")} />}
              onPress={() => openWorkspace(FILE_MANAGER)} />
          </If>
        </Row>
        <If when={local() && props.cwd ? activeTarget() : undefined}>
          {(current) => (
            <View class="gap-1">
              <View class="flex-row items-center gap-0.5 pr-1">
                <View class="min-w-0 flex-1">
                  <ListItem label={t("details.openIn", { target: targetLabel(current()) })} leading={<TargetIcon target={current()} />}
                    onPress={() => openWorkspace(current().command)} />
                </View>
                <IconButton
                  icon="chevron-down"
                  label={t("details.chooseEditor")}
                  size="sm"
                  selected={pickerOpen()}
                  class={`${pickerOpen() ? "rotate-180" : ""} ${motionClass.turn}`}
                  tooltip={<Tooltip text={t("details.chooseEditor")} />}
                  onPress={togglePicker}
                />
              </View>
              <If when={pickerOpen()}>
                <View class="mx-1 gap-0.5 rounded-(--radius) border border-border/60 bg-muted/40 p-1">
                  <Each of={openTargets} key={(target) => target.command}>
                    {(target) => (
                      <ListItem
                        label={targetLabel(target())}
                        leading={<TargetIcon target={target()} />}
                        trailing={
                          <If when={current().command === target().command}>
                            <Icon name="check" size="sm" color="muted-foreground" />
                          </If>
                        }
                        onPress={() => {
                          props.settings.update({ openIn: target().command });
                          openWorkspace(target().command);
                          pickerOpen.set(false);
                        }}
                      />
                    )}
                  </Each>
                </View>
              </If>
            </View>
          )}
        </If>
        <If when={openError()}>{(text) => <Text role="alert" class={`px-2.5 py-1 ${type.meta} text-destructive`}>{text()}</Text>}</If>
      </View>

      <If when={relations().owner || relations().parent || relations().running.length || relations().previous.length}>
        <Separator inset />
        <View class="gap-0.5">
          <View class="flex-row items-center px-2.5 pt-1 pb-0.5">
            <Text role="header" class={`${inlineLabel} ${type.meta} min-w-0 flex-1 truncate font-medium text-muted-foreground`}>
              {relations().running.length ? t("details.relatedRunning", { count: relations().running.length }) : t("details.related")}
            </Text>
          </View>
          <Scroll class="max-h-64" contentClass="gap-0.5">
            <If when={relations().owner}>
              {(owner) => <RelatedRow session={owner} icon="corner-left-up" description={t("details.parentAgent")} />}
            </If>
            <If when={relations().parent}>
              {(parent) => <RelatedRow session={parent} icon="corner-left-up" description={t("details.forkedFrom")} />}
            </If>
            <Each of={relations().running} key={(session) => session.sessionId}>
              {(session) => <RelatedRow session={session} running />}
            </Each>
            <Each of={relations().previous} key={(session) => session.sessionId}>
              {(session) => <RelatedRow session={session} />}
            </Each>
          </Scroll>
        </View>
      </If>
    </View>
  );
}
