import { computed, effect, keymap, List, signal, Text, View } from "nsh";
import type { ListRef, ViewRef } from "nsh";
import { ResizablePanel, ResizablePanelGroup } from "nsh/base";
import { t } from "nsh/i18n";

import type { MessageId, SessionId, ToolCallId } from "../client/acp/protocol";
import type { FileChange } from "../client/domain/model";
import { TurnGroups } from "../client/turns";
import { Composer } from "../composer/Composer";
import type { HostStore } from "../stores/host";
import { Appear } from "../ui/appear";
import { IconButton } from "../ui/button";
import { Icon } from "../ui/icon";
import { glideNextLayout } from "../ui/motion";
import { Badge } from "../ui/surface";
import { type } from "../ui/tokens";
import { workspaceName } from "../shell/workspaces";
import { ChangePanel } from "./ChangePanel";
import { TurnRow } from "./TurnRow";
import type { TurnSource } from "./TurnRow";

/** Within this many points of the end the list counts as at the bottom and follows new content. */
const BOTTOM_THRESHOLD = 48;

/** Remembers the user's dragged Changes inspector width across Session switches. */
const inspectorWidth = signal(420);

/** Tracks the button that opened the inspector while its virtualized cell remains mounted. */
export type ChangeOpener = Readonly<{ node: ViewRef; isMounted: () => boolean }>;

/** One open Session: its header, its transcript as turns, and the composer. */
export function SessionView(props: { host: HostStore; sessionId: SessionId; modalOpen?: boolean }) {
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

  const selection = computed(() => props.host.changeSelections().get(props.sessionId));
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
  let opener: { turnKey: string; path: string; target: ChangeOpener } | undefined;
  /** Closes the inspector and restores focus only if the originating card cell is still mounted. */
  const closeChange = () => {
    glideNextLayout();
    const current = selection();
    props.host.selectChange(props.sessionId);
    const saved = opener;
    opener = undefined;
    if (saved && current && saved.turnKey === current.turnKey && saved.path === current.path && saved.target.isMounted()) {
      setTimeout(() => {
        if (saved.target.isMounted()) saved.target.node.focus();
      }, 0);
    }
  };
  /** Binds the inspector to the card's turn and file, or closes it when clicking the already active card. */
  const openChange = (selectedTurnKey: string, change: FileChange, trigger?: ChangeOpener) => {
    const current = selection();
    if (current?.turnKey === selectedTurnKey && current.path === change.path) {
      closeChange();
      return;
    }
    opener = trigger ? { turnKey: selectedTurnKey, path: change.path, target: trigger } : undefined;
    if (!file()) glideNextLayout();
    props.host.selectChange(props.sessionId, { turnKey: selectedTurnKey, path: change.path, editId: change.edits[0].id });
  };
  // Bind Escape whenever the inspector is open and no modal dialog sits above the Session.
  effect(() => {
    if (!props.modalOpen && file()) return keymap.bind("escape", closeChange, { when: compact() ? "always" : "notEditing" });
  });
  return (
    <View class="flex-1" onLayout={({ bounds }) => compact.set(bounds.size.width < 760)}>
      <ResizablePanelGroup
        class="flex-1"
        handleClass="bg-border group-hover:bg-ring group-active:bg-ring transition-colors duration-120 motion-reduce:transition-none"
        handleThumbClass="h-6 w-1 rounded-full bg-muted-foreground opacity-0 group-hover:opacity-60 group-active:opacity-100 transition-opacity duration-120 motion-reduce:transition-none"
        handleAccessibilityLabel={t("changes.resize")}
      >
        <ResizablePanel minSize={360} visible={!compact() || !file()} class="min-w-0">
          <View class="h-12 flex-row items-center justify-between gap-3 border-b border-border px-5">
            <View class="min-w-0 flex-1 flex-row items-center gap-2.5">
              <View class="size-7 items-center justify-center rounded-lg bg-muted">
                <Icon name="folder" size="sm" color="muted-foreground" />
              </View>
              <Text class={`${type.ui} font-semibold text-foreground truncate`}>{summary()?.title ?? props.sessionId}</Text>
            </View>
            <If when={summary()?.cwd}>
              {(cwd) => <Badge label={workspaceName(cwd())} />}
            </If>
          </View>
          <View class="flex-1">
            <List
              ref={list}
              each={turns()}
              keyExtractor={(turn) => turn.key}
              estimatedItemSize={160}
              maintainVisibleContentPosition={{ autoscrollToBottomThreshold: BOTTOM_THRESHOLD }}
              onScroll={(event) => atBottom.set(event.contentSize.height - (event.offset.y + event.size.height) <= BOTTOM_THRESHOLD)}
              class="flex-1"
            >
              {(turn) => (
                <TurnRow
                  turn={turn()}
                  source={source}
                  selectedPath={selection()?.turnKey === turn().key ? selection()?.path : undefined}
                  onViewChange={openChange}
                />
              )}
            </List>
            <If when={!atBottom()}>
              <Appear entrance="riseSm" class="absolute bottom-4 left-0 right-0 items-center">
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
          <Composer host={props.host} disabled={props.host.phase().kind !== "ready"} sessionId={props.sessionId}
            onSend={(draft) => props.host.prompt(props.sessionId, draft.text, draft.images)} />
        </ResizablePanel>
        <ResizablePanel
          size={compact() ? undefined : inspectorWidth()}
          minSize={300}
          fixed={!compact()}
          visible={!!file()}
          onResize={({ size }) => { if (!compact()) inspectorWidth.set(size); }}
          class="min-w-0"
        >
          <If when={file()}>
            {(change) => <ChangePanel change={change()} turnNumber={turnIndex() + 1} editId={selection()?.editId}
              compact={compact()} onClose={closeChange}
              onEdit={(editId) => {
                const current = selection();
                if (current) props.host.selectChange(props.sessionId, { ...current, editId });
              }} />}
          </If>
        </ResizablePanel>
      </ResizablePanelGroup>
    </View>
  );
}
