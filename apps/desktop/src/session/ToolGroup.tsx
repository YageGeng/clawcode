import { Text, View } from "nsh";
import { t } from "nsh/i18n";

import type { ToolCallId } from "../client/acp/protocol";
import type { ToolCallEntity } from "../client/domain/model";
import type { ToolActivity } from "../client/turns";
import { Disclosure } from "../ui/disclosure";
import { Icon } from "../ui/icon";
import type { IconName } from "../ui/icon";
import { type } from "../ui/tokens";

/** Each activity's icon: the group shows its first activity's. */
const activityIcon: Readonly<Record<ToolActivity, IconName>> = {
  ran: "terminal",
  edited: "pencil",
  read: "file-text",
  searched: "search",
  fetched: "file-text",
  other: "settings",
};

/** One summary line for consecutive tool calls ("Ran commands, Read files"), unfolding to one row per call. */
export function ToolGroup(props: {
  toolCallIds: readonly ToolCallId[];
  activities: readonly ToolActivity[];
  tool: (id: ToolCallId) => ToolCallEntity | undefined;
  open: boolean;
  onToggle: () => void;
}) {
  const label = () => props.activities.map((activity) => t(`tools.${activity}`)).join(t("tools.separator"));
  return (
    <Disclosure label={label()} icon={activityIcon[props.activities[0] ?? "other"]} open={props.open} onToggle={props.onToggle}>
      <View class="ml-2.5 gap-1.5 border-l border-border pl-4">
        <Each of={props.toolCallIds}>
          {(id) => (
            <View class="flex-row items-center gap-2">
              <Icon name={activityIcon[props.activities[0] ?? "other"]} size="sm" color="muted-foreground" />
              <Text class={`${type.ui} text-muted-foreground truncate`}>{props.tool(id())?.title ?? id()}</Text>
            </View>
          )}
        </Each>
      </View>
    </Disclosure>
  );
}
