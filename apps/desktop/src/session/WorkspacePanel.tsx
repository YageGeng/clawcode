import { cleanup, computed, Scroll, Text, View } from "nsh";
import { t } from "nsh/i18n";
import type { JSX } from "nsh/jsx-runtime";

import type { FileChange } from "../client/domain/model";
import type { TurnView } from "../client/turns";
import type { ChangeSelection, HostStore, SessionPanel } from "../stores/host";
import { IconButton } from "../ui/button";
import { Empty } from "../ui/feedback";
import { FileIcon } from "../ui/fileIcon";
import { Icon } from "../ui/icon";
import { ListItem } from "../ui/list";
import { Tooltip } from "../ui/menu";
import { Badge, LineCounts } from "../ui/surface";
import { Tabs } from "../ui/tabs";
import { type } from "../ui/tokens";
import { ChangePanel } from "./ChangePanel";
import { FilesPanel } from "./FilesPanel";

/** Mounts the tab on first visit and keeps its native scroll position and selection across tab switches. */
function InspectorTab(props: { active: boolean; children: JSX.Element }) {
  const visited = computed<boolean>((previous) => previous === true || props.active);
  return (
    <If when={visited()}>
      <View class={props.active ? "min-h-0 flex-1" : "hidden"}>{props.children}</View>
    </If>
  );
}

/** Lists files grouped by turn, newest first, while the reader is choosing a recorded diff. */
function ChangedFilesList(props: { turns: readonly TurnView[]; onSelect: (change: ChangeSelection) => void }) {
  const changedTurns = computed(() => {
    const turns = props.turns;
    const groups = [];
    for (let i = turns.length - 1; i >= 0; i--) {
      const turn = turns[i]!;
      if (turn.changes.length) groups.push({ turn, turnNumber: i + 1 });
    }
    return groups;
  });
  return (
    <If when={changedTurns().length > 0} else={<Empty class="flex-1" icon="file-text" title={t("files.noChanges")} />}>
      <Scroll class="flex-1" contentClass="gap-3 p-2">
        <Each of={changedTurns()} key={(group) => group.turn.key}>
          {(group) => (
            <View class="gap-0.5">
              <Text class={`px-2.5 py-1 ${type.meta} font-semibold text-muted-foreground`}>{t("changes.turn", { number: group().turnNumber })}</Text>
              <Each of={group().turn.changes} key={(change) => change.path}>
                {(change) => (
                  <ListItem
                    leading={<FileIcon path={change().path} size="md" />}
                    label={change().path} mono accessibilityLabel={t("changes.openDiff", { path: change().path })}
                    trailing={
                      <View class="flex-row items-center gap-1.5">
                        <LineCounts added={change().added} removed={change().removed} />
                        <Icon name="chevron-right" size="sm" color="muted-foreground" />
                      </View>
                    }
                    onPress={() => props.onSelect({ turnKey: group().turn.key, path: change().path, editId: change().edits[0].id })}
                  />
                )}
              </Each>
            </View>
          )}
        </Each>
      </Scroll>
    </If>
  );
}

/** A session's inspector. Visited tabs stay mounted so browsing files preserves the diff and its scroll position. */
export function WorkspacePanel(props: {
  host: HostStore; cwd?: string; panel: SessionPanel; compact: boolean; modalOpen?: boolean;
  /** Host connection state forwarded here in compact mode, while the session header is hidden. */
  status?: JSX.Element;
  file?: FileChange; turnNumber: number; turns: readonly TurnView[];
  onChange: (panel: SessionPanel) => void; onClose: () => void;
}) {
  // A JSX slot is a getter that builds a new element on every read, `If`'s accessor included; read it once.
  const status = computed(() => props.status);
  // In compact mode the session header disappears. Move keyboard focus to its visible return action.
  let focusBack: ReturnType<typeof setTimeout> | undefined;
  cleanup(() => clearTimeout(focusBack));
  const changedFileCount = computed(() => {
    const paths = new Set<string>();
    for (const turn of props.turns) {
      for (const change of turn.changes) paths.add(change.path);
    }
    return paths.size;
  });
  const tabs = computed(() => [
    { value: "files", label: t("files.title") },
    { value: "changes", label: t("changes.title"), badge: changedFileCount() },
  ]);
  const root = computed(() => props.host.phase().kind === "ready" ? props.cwd : undefined);
  // Remount the browser when a reconnect replaces the reader, even if the workspace path is identical.
  const scopes = computed(() => {
    const path = root();
    return path ? [{ reader: props.host.files, root: path }] : [];
  });
  return (
    <View class="min-h-0 flex-1 bg-background">
      <View class="h-12 flex-row items-center gap-1.5 border-b border-border px-3">
        <If when={props.compact}>
          <IconButton icon="arrow-left" label={t("files.close")}
            buttonRef={(node) => { focusBack = setTimeout(() => { if (props.compact && !props.modalOpen) node.focus(); }, 0); }}
            tooltip={<Tooltip text={t("files.close")} />} onPress={props.onClose} />
        </If>
        <Tabs variant="bar" items={tabs()} value={props.panel.tab} accessibilityLabel={t("files.panel")}
          onChange={(value) => props.onChange({ ...props.panel, tab: value === "files" ? "files" : "changes" })} />
        <View class="min-w-0 flex-1 items-start pl-1">
          <If when={props.panel.tab === "changes" && props.file}>
            <Badge label={t("changes.turn", { number: props.turnNumber })} />
          </If>
        </View>
        <If when={status()}><View class="shrink-0 flex-row items-center">{status()}</View></If>
        <If when={!props.compact}>
          <IconButton icon="x" label={t("files.close")} tooltip={<Tooltip text={t("files.close")} />} onPress={props.onClose} />
        </If>
      </View>
      <InspectorTab active={props.panel.tab === "files"}>
        <Each of={scopes()}>
          {(workspace) => <FilesPanel reader={workspace().reader} root={workspace().root} />}
        </Each>
        <If when={!scopes().length}><Empty class="flex-1" icon="folder" title={t("files.unavailable")} /></If>
      </InspectorTab>
      <InspectorTab active={props.panel.tab === "changes"}>
        <If when={props.file} else={
          <ChangedFilesList turns={props.turns} onSelect={(change) => props.onChange({ tab: "changes", change })} />
        }>
          {(change) => <ChangePanel change={change()} editId={props.panel.change?.editId}
            onBack={() => props.onChange({ tab: "changes" })}
            onEdit={(editId) => {
              const saved = props.panel.change;
              if (saved) props.onChange({ ...props.panel, change: { ...saved, editId } });
            }} />}
        </If>
      </InspectorTab>
    </View>
  );
}
