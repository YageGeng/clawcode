import { cleanup, View } from "nsh";
import type { ViewRef } from "nsh";
import { t } from "nsh/i18n";

import type { FileChange } from "../client/domain/model";
import { Button } from "../ui/button";
import { FilePath } from "../ui/filePath";
import { Icon } from "../ui/icon";
import { LineCounts, Surface } from "../ui/surface";
import { type } from "../ui/tokens";
import type { ChangeOpener } from "./SessionView";

/** Opens the selected file’s recorded edits from this turn, highlighting the card while active. */
export function ChangeCard(props: { change: FileChange; selected?: boolean; onOpen: (trigger?: ChangeOpener) => void }) {
  let trigger: ViewRef | undefined;
  let mounted = true;
  cleanup(() => { mounted = false; });
  return (
    <Surface class={`flex-row items-center gap-3 p-3 ${props.selected ? "border-ring/50 bg-accent/40" : ""}`}>
      <View class={`size-9 items-center justify-center rounded-(--radius) ${props.selected ? "bg-info/15" : "bg-muted"}`}>
        <Icon name="file-text" size="md" color={props.selected ? "info" : "muted-foreground"} />
      </View>
      <View class="min-w-0 flex-1 gap-0.5">
        <FilePath path={props.change.path} label={t("changes.edited", { file: props.change.path })}
          class={`${type.ui} ${props.selected ? "font-medium" : ""} text-card-foreground`} />
        <LineCounts added={props.change.added} removed={props.change.removed} />
      </View>
      <Button
        label={t("changes.view")}
        accessibilityLabel={t("changes.viewFile", { path: props.change.path })}
        size="sm"
        variant="outline"
        selected={props.selected === true}
        buttonRef={(node) => { trigger = node; }}
        onPress={() => props.onOpen(trigger ? { node: trigger, isMounted: () => mounted } : undefined)}
      />
    </Surface>
  );
}
