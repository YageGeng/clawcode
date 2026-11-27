import { computed, effect, Text, View } from "nsh";
import { t } from "nsh/i18n";

import type { FileChange } from "../client/domain/model";
import { IconButton } from "../ui/button";
import { UnifiedDiff } from "../ui/diff";
import { FileIcon } from "../ui/fileIcon";
import { FilePath } from "../ui/filePath";
import { Tooltip } from "../ui/menu";
import { LineCounts } from "../ui/surface";
import { inlineLabel, type } from "../ui/tokens";

/** Inspects one file from the selected turn; successive edits can mix patches and file snapshots. */
export function ChangePanel(props: {
  change: FileChange;
  editId?: string;
  onBack?: () => void;
  onEdit: (id: string) => void;
}) {
  const rawIndex = computed(() => props.change.edits.findIndex((edit) => edit.id === props.editId));
  // Clamp to a valid index immediately so withdrawn edits never flash `0 of N` or step out of bounds.
  const index = computed(() => Math.max(0, rawIndex()));
  const selected = computed(() => props.change.edits[index()]!);
  // A correction may withdraw the selected edit. Remember the first surviving edit as the new selection.
  effect(() => {
    if (rawIndex() < 0 && props.editId !== undefined) props.onEdit(selected().id);
  });
  return (
    <View class="min-w-0 flex-1 bg-background">
      <View class="min-h-9 flex-row flex-wrap items-center gap-x-2 gap-y-1 border-b border-border bg-muted/45 px-3 py-1">
        {/* Keep back navigation and the file path together before wrapping edit controls. */}
        <View class="min-w-0 grow basis-40 flex-row items-center gap-1.5">
          <If when={props.onBack !== undefined}>
            <IconButton size="sm" icon="chevron-left" label={t("changes.backToList")}
              tooltip={<Tooltip text={t("changes.backToList")} />} onPress={() => props.onBack?.()} />
          </If>
          <FileIcon path={props.change.path} size="sm" />
          <FilePath path={props.change.path} class={`flex-1 ${inlineLabel} font-mono ${type.meta} text-foreground`} />
        </View>
        <View class="flex-row items-center gap-2.5">
          <If when={props.change.edits.length > 1}>
            <View class="flex-row items-center gap-0.5 rounded-(--radius) border border-border bg-background px-1 py-0.5">
              <IconButton size="sm" icon="arrow-left" label={t("changes.previous")} disabled={index() === 0}
                tooltip={<Tooltip text={t("changes.previous")} />}
                onPress={() => props.onEdit(props.change.edits[index() - 1]!.id)} />
              <Text class={`px-1 tabular-nums ${inlineLabel} ${type.meta} text-muted-foreground`}>{t("changes.editIndex", { current: index() + 1, total: props.change.edits.length })}</Text>
              <IconButton size="sm" icon="arrow-right" label={t("changes.next")} disabled={index() + 1 === props.change.edits.length}
                tooltip={<Tooltip text={t("changes.next")} />}
                onPress={() => props.onEdit(props.change.edits[index() + 1]!.id)} />
            </View>
          </If>
          <LineCounts added={selected().added} removed={selected().removed} />
        </View>
      </View>
      {/* Mount a fresh viewer so a different edit cannot inherit the previous scroll offset or line selection. */}
      <Each of={[selected()]} key={(edit) => edit.id}>
        {(edit) => <UnifiedDiff diff={edit().diff} path={edit().path} />}
      </Each>
    </View>
  );
}
