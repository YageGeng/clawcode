import { cleanup, computed, effect, keymap, List, signal, Text, View } from "nsh";
import type { ListRef, ViewRef } from "nsh";
import { t } from "nsh/i18n";
import type { JSX } from "nsh/jsx-runtime";

import type { MessageId, SessionId, ToolCallId } from "../client/acp/protocol";
import { TurnGroups } from "../client/turns";
import { Composer } from "../composer/Composer";
import type { SettingsStore } from "../stores/settings";
import type { HostStore } from "../stores/host";
import { Appear } from "../ui/appear";
import { IconButton } from "../ui/button";
import { Icon } from "../ui/icon";
import { glideNextLayout } from "../ui/motion";
import { SplitGroup, SplitPane } from "../ui/split";
import { Badge, Separator, Surface } from "../ui/surface";
import { inlineLabel, type } from "../ui/tokens";
import { workspaceName } from "../shell/workspaces";
import { SessionDetails } from "./SessionDetails";
import { WorkspacePanel } from "./WorkspacePanel";
import type { ChangeOpener, ViewTurnChange } from "./ChangeCard";
import { Popover, Tooltip } from "../ui/menu";
import { TurnRow } from "./TurnRow";
import type { TurnSource } from "./TurnRow";

/** Within this many points of the end the list counts as at the bottom and follows new content. */
const BOTTOM_THRESHOLD = 48;
/** The details card's width plus the gutter between it and the transcript. */
const DETAILS_GUTTER = 280 + 16;
/** Session width at which the 760 pt transcript column still fits beside the details card. */
const DETAILS_FLOAT_MIN_WIDTH = 760 + DETAILS_GUTTER + 16;

/** Remembers the user's dragged Changes inspector width across Session switches. */
const inspectorWidth = signal(420);

/** One open Session: its header, its transcript as turns, and the composer. */
export function SessionView(props: {
  host: HostStore; sessionId: SessionId; settings: SettingsStore; modalOpen?: boolean; deviceName?: string;
  onOpenSession: (sessionId: SessionId) => void;
  /** Controls before the title, such as navigation while the sidebar is hidden. */
  leading?: JSX.Element;
  /** The host connection's state, shown at the right of the header when it is not plainly connected. */
  status?: JSX.Element;
}) {
  // The details card stays open beside the transcript whenever the column fits next to it; the column moves aside for it.
  const transcriptWidth = signal(0);
  // JSX slots are getters that build a new element on every read, `If`'s accessor included: read each one once.
  const leading = computed(() => props.leading);
  const status = computed(() => props.status);
  const floatDetails = computed(() => transcriptWidth() >= DETAILS_FLOAT_MIN_WIDTH);
  const detailsOpen = signal(false);
  const details = () => (
    <SessionDetails host={props.host} sessionId={props.sessionId} cwd={summary()?.cwd} deviceName={props.deviceName}
      settings={props.settings} onOpenSession={props.onOpenSession} />
  );
  const summary = () => props.host.sessions().find((session) => session.sessionId === props.sessionId);
  const session = computed(() => props.host.session(props.sessionId));
  const groups = new TurnGroups();
  const turns = computed(() => {
    const current = session();
    return current ? groups.update(current) : [];
  });
  // Parts the user unfolded, by key; kept here, not in list cells (cells are reused for other turns).
  const opened = signal<ReadonlySet<string>>(new Set());
  const atBottom = signal(true);
  let list: ListRef | undefined;

  // Start at the end of the conversation once its replay arrives; native list anchoring follows from there.
  let anchored = false;
  effect(() => {
    if (anchored || turns().length === 0) return;
    anchored = true;
    setTimeout(() => list?.scrollToEnd({ animated: false }), 0);
  });

  const source: TurnSource = {
    message: (id: MessageId) => session()?.messages.get(id),
    tool: (id: ToolCallId) => session()?.tools.get(id),
    terminal: (id) => session()?.terminals.get(id),
    isOpen: (key) => opened().has(key),
    toggle: (key) => {
      glideNextLayout();
      opened.update((set) => {
        const next = new Set(set);
        if (!next.delete(key)) next.add(key);
        return next;
      });
    },
  };

  const panel = computed(() => props.host.panels().get(props.sessionId));
  const selection = computed(() => panel()?.change);
  const turnKey = computed(() => selection()?.turnKey);
  const turnIndex = computed(() => {
    const key = turnKey();
    return key === undefined ? -1 : turns().findIndex((turn) => turn.key === key);
  });
  const file = computed(() => {
    const index = turnIndex();
    return index >= 0 ? turns()[index]?.changes.find((change) => change.path === selection()?.path) : undefined;
  });
  // Measure the space remaining after the sidebar, including when it is toggled or the window is resized.
  const compact = signal(false);
  let opener: ChangeOpener | undefined;
  let headerButton: ViewRef | undefined;
  let restoreFocus: ReturnType<typeof setTimeout> | undefined;
  cleanup(() => clearTimeout(restoreFocus));
  /** Returns focus to the originating card or to the header if the list recycled that card. */
  const closePanel = () => {
    props.host.setPanel(props.sessionId);
    const target = opener;
    opener = undefined;
    clearTimeout(restoreFocus);
    restoreFocus = setTimeout(() => {
      if (!panel() && !props.modalOpen) (target?.isMounted() ? target.node : headerButton)?.focus();
    }, 0);
  };
  /** Opens the files tab from the session header, or dismisses the current panel. */
  const togglePanel = () => {
    if (panel()) return closePanel();
    props.host.setPanel(props.sessionId, { tab: "files" });
  };
  /** Reviews a turn at its current or first file. Individual file cards also toggle an active diff closed. */
  const openChange: ViewTurnChange = (turn, trigger, requestedFile) => {
    const current = selection();
    const change = requestedFile ?? (current?.turnKey === turn.key ? file() : undefined) ?? turn.changes[0];
    if (!change) return;
    const sameFile = current?.turnKey === turn.key && current.path === change.path;
    // The last transcript button used owns focus, including when a file card closes a diff opened by Review.
    opener = trigger;
    if (sameFile && panel()?.tab === "changes") {
      if (requestedFile) closePanel();
      return;
    }
    props.host.setPanel(props.sessionId, { tab: "changes", change: sameFile ? current
      : { turnKey: turn.key, path: change.path, editId: change.edits[0].id } });
  };
  // Bind Escape whenever the inspector is open and no modal dialog sits above the Session.
  effect(() => {
    if (!props.modalOpen && panel()) return keymap.bind("escape", closePanel, { when: compact() ? "always" : "notEditing" });
  });
  return (
    <View class="flex-1" onLayout={({ bounds }) => compact.set(bounds.size.width < 760)}>
      <SplitGroup handleAccessibilityLabel={t("files.resize")} class="flex-1">
        <SplitPane minSize={360} visible={!compact() || !panel()} class="min-w-0">
          <View class="h-12 flex-row items-center justify-between gap-3 border-b border-border px-5">
            <View class="min-w-0 flex-1 flex-row items-center gap-2.5">
              {/* `-ml-5` cancels the header's `px-5`, so the buttons sit where the sidebar's own navigation row puts them. */}
              <If when={leading()}><View class="-ml-5">{leading()}</View></If>
              <View class="size-7 items-center justify-center rounded-lg bg-muted">
                <Icon name="folder" size="sm" color="muted-foreground" />
              </View>
              <Text class={`${inlineLabel} ${type.ui} font-semibold text-foreground truncate`}>{summary()?.title ?? props.sessionId}</Text>
            </View>
            <View class="shrink-0 flex-row items-center gap-2">
              {/* Show status here while the session header is visible; in compact panel mode `WorkspacePanel` hosts it so only one copy mounts. */}
              <If when={(!compact() || !panel()) ? status() : undefined}>
                <View class="flex-row items-center gap-2">
                  {status()}
                  <View class="h-3.5"><Separator vertical /></View>
                </View>
              </If>
              <If when={summary()?.cwd}>
                {(cwd) => <Badge label={workspaceName(cwd())} />}
              </If>
              <View class="flex-row items-center gap-1">
                <If when={!floatDetails()}>
                  <Popover class="w-[280px] p-1.5" offset={18} open={detailsOpen()} onOpenChange={(open) => detailsOpen.set(open)} trigger={
                    <IconButton icon="info" label={t("details.title")} selected={detailsOpen()} tooltip={<Tooltip text={t("details.title")} />} />
                  }>
                    {details()}
                  </Popover>
                </If>
                <IconButton icon="panel-right" label={t(panel() ? "files.hide" : "files.open")} selected={!!panel()}
                  buttonRef={(node) => { headerButton = node; }} tooltip={<Tooltip text={t(panel() ? "files.hide" : "files.open")} />}
                  onPress={togglePanel} />
              </View>
            </View>
          </View>
          <View class="flex-1" onLayout={({ bounds }) => transcriptWidth.set(bounds.size.width)}>
            <List
              ref={list}
              each={turns()}
              keyExtractor={(turn) => turn.key}
              estimatedItemSize={160}
              maintainVisibleContentPosition={{ autoscrollToBottomThreshold: BOTTOM_THRESHOLD }}
              onScroll={(event) => atBottom.set(event.contentSize.height - (event.offset.y + event.size.height) <= BOTTOM_THRESHOLD)}
              class="flex-1"
              contentClass={floatDetails() ? "pr-[296px]" : ""}
            >
              {(turn) => (
                <TurnRow
                  turn={turn()}
                  source={source}
                  selectedPath={panel()?.tab === "changes" && selection()?.turnKey === turn().key ? selection()?.path : undefined}
                  onViewChange={openChange}
                />
              )}
            </List>
            <If when={floatDetails()}>
              <Surface variant="panel" class="absolute right-4 top-3 w-[280px]">
                {details()}
              </Surface>
            </If>
            <If when={!atBottom()}>
              <Appear entrance="riseSm" class={`absolute bottom-4 left-0 right-0 items-center ${floatDetails() ? "pr-[296px]" : ""}`}>
                <IconButton
                  icon="arrow-down"
                  label={t("session.jumpToBottom")}
                  variant="outline"
                  round
                  class="shadow-md"
                  onPress={() => {
                    list?.scrollToEnd({ animated: true });
                    atBottom.set(true);
                  }}
                />
              </Appear>
            </If>
          </View>
          <View class={floatDetails() ? "pr-[296px]" : ""}>
            <Composer host={props.host} disabled={props.host.phase().kind !== "ready"} sessionId={props.sessionId}
              onSend={(draft) => props.host.prompt(props.sessionId, draft.text, draft.images)} />
          </View>
        </SplitPane>
        <SplitPane
          size={compact() ? undefined : inspectorWidth()}
          minSize={300}
          fixed={!compact()}
          visible={!!panel()}
          onResize={(size) => { if (!compact()) inspectorWidth.set(size); }}
          class="min-w-0"
        >
          <If when={panel()}>
            {(active) => <WorkspacePanel host={props.host} cwd={summary()?.cwd} panel={active()} compact={compact()} modalOpen={props.modalOpen}
              status={compact() ? status() : undefined}
              turns={turns()} file={file()} turnNumber={turnIndex() + 1} onClose={closePanel}
              onChange={(next) => props.host.setPanel(props.sessionId, next)} />}
          </If>
        </SplitPane>
      </SplitGroup>
    </View>
  );
}
