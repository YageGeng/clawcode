import { computed, Text, View } from "nsh";
import { t } from "nsh/i18n";

import { AcpProtocol } from "../client/acp/protocol";
import type { ToolCallId } from "../client/acp/protocol";
import type { ToolCallEntity, ToolKind } from "../client/domain/model";
import { Disclosure } from "../ui/disclosure";
import type { IconName } from "../ui/icon";
import { type } from "../ui/tokens";
import { ToolDetails } from "./ToolContent";
import type { TurnSource } from "./TurnRow";

/** Each call gets its own ACP kind's icon, including adjacent calls of different kinds. */
const icons: Readonly<Record<ToolKind, IconName>> = {
  execute: "terminal", read: "file-text", edit: "pencil", delete: "x", move: "arrow-right",
  search: "search", fetch: "arrow-down", think: "ellipsis", switch_mode: "settings", other: "settings",
};

/** Picks one identifying argument for the summary; full multiline commands and paths stay in the input. */
function toolHint(tool: ToolCallEntity): string | undefined {
  const input = AcpProtocol.record(tool.rawInput);
  const name = tool.name || tool.title;
  const hint = [input?.cmd, input?.command, input?.path, input?.file_path, input?.pattern, input?.query, input?.url,
    tool.title !== tool.toolCallId ? tool.title : undefined, tool.changes[0]?.path]
    .find((value): value is string => typeof value === "string" && !!value.trim() && value.trim() !== name);
  if (hint === undefined) return undefined;
  const summary = hint.trim();
  const firstLine = summary.split("\n", 1)[0]!;
  return firstLine.length > 52 || firstLine.length < summary.length ? firstLine.slice(0, 51) + "…" : firstLine;
}

/** A tool invocation with its own persistent disclosure state and lazily rendered payload. */
export function ToolRow(props: { id: ToolCallId; source: TurnSource }) {
  const tool = computed(() => props.source.tool(props.id));
  const openKey = () => `tool:${props.id}`;
  return (
    <If when={tool()}>
      {(call) => (
        <Disclosure label={call().name || call().title || props.id} icon={icons[call().kind ?? "other"]}
          open={props.source.isOpen(openKey())} onToggle={() => props.source.toggle(openKey())}
          trailing={
            <View class="min-w-0 shrink flex-row items-center gap-2">
              <If when={toolHint(call())}>
                {(hint) => <Text class={`shrink truncate font-mono ${type.meta} text-muted-foreground`}>{hint()}</Text>}
              </If>
              <Text class={`${type.meta} ${call().status === "failed" ? "text-destructive" : "text-muted-foreground"}`}>
                {t(`toolDetail.status.${call().status}`)}
              </Text>
            </View>
          }>
          <ToolDetails tool={call()} source={props.source} />
        </Disclosure>
      )}
    </If>
  );
}
