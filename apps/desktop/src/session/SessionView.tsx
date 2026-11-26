import { computed, effect, List, signal, Text, View } from "nsh";
import type { ListRef } from "nsh";
import { t } from "nsh/i18n";

import type { MessageId, SessionId, ToolCallId } from "../client/acp/protocol";
import { groupTurns } from "../client/turns";
import { Composer } from "../composer/Composer";
import type { HostStore } from "../stores/host";
import { Appear } from "../ui/appear";
import { IconButton } from "../ui/button";
import { Icon } from "../ui/icon";
import { type } from "../ui/tokens";
import { TurnRow } from "./TurnRow";
import type { TurnSource } from "./TurnRow";

/** Within this many points of the end the list counts as at the bottom and follows new content. */
const BOTTOM_THRESHOLD = 48;

/** One open Session: its header, its transcript as turns, and the composer. */
export function SessionView(props: { host: HostStore; sessionId: SessionId }) {
  const summary = () => props.host.sessions().find((session) => session.sessionId === props.sessionId);
  const session = computed(() => props.host.workspace().sessionWorkspaces.get(props.sessionId));
  const turns = computed(() => {
    const current = session();
    return current === undefined ? [] : groupTurns(current);
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
    isOpen: (key) => opened().has(key),
    toggle: (key) =>
      opened.update((set) => {
        const next = new Set(set);
        if (!next.delete(key)) next.add(key);
        return next;
      }),
  };

  return (
    <View class="flex-1">
      <View class="h-12 flex-row items-center gap-2 px-4">
        <Icon name="folder" size="md" color="muted-foreground" />
        <Text class={`${type.ui} font-medium text-foreground truncate`}>{summary()?.title ?? props.sessionId}</Text>
      </View>
      <View class="flex-1">
        <List
          ref={list}
          each={[...turns()]}
          keyExtractor={(turn) => turn.key}
          estimatedItemSize={160}
          maintainVisibleContentPosition={{ autoscrollToBottomThreshold: BOTTOM_THRESHOLD }}
          onScroll={(event) => atBottom.set(event.contentSize.height - (event.offset.y + event.size.height) <= BOTTOM_THRESHOLD)}
          class="flex-1"
        >
          {(turn) => <TurnRow turn={turn()} source={source} />}
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
      <Composer host={props.host} sessionId={props.sessionId} onSend={(text) => props.host.prompt(props.sessionId, text)} />
    </View>
  );
}
