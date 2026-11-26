import { Text, View } from "nsh";
import { t } from "nsh/i18n";

import type { FileChange } from "../client/turns";
import { workspaceName } from "../shell/workspaces";
import { Icon } from "../ui/icon";
import { LineCounts, Surface } from "../ui/surface";
import { type } from "../ui/tokens";

/** One file a turn changed: its name and line counts ("View changes" opens the diff from N3). */
export function ChangeCard(props: { change: FileChange }) {
  return (
    <Surface class="flex-row items-center gap-3 p-3">
      <View class="size-9 items-center justify-center rounded-(--radius) bg-muted">
        <Icon name="file-text" size="md" color="muted-foreground" />
      </View>
      <View class="flex-1 gap-0.5">
        <Text class={`${type.ui} text-card-foreground truncate`}>{t("changes.edited", { file: workspaceName(props.change.path) })}</Text>
        <LineCounts added={props.change.added} removed={props.change.removed} />
      </View>
    </Surface>
  );
}
