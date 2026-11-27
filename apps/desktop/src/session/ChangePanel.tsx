import { computed, effect, Text, View } from "nsh";
import { t } from "nsh/i18n";

import type { FileChange } from "../client/domain/model";
import { IconButton } from "../ui/button";
import { UnifiedDiff } from "../ui/diff";
import { FilePath } from "../ui/filePath";
import { Icon } from "../ui/icon";
import { Tooltip } from "../ui/menu";
import { Badge, LineCounts } from "../ui/surface";
import { type } from "../ui/tokens";

/** Inspects one file from the selected turn; successive edits can mix patches and file snapshots. */
export function ChangePanel(props: {
  change: FileChange;
  turnNumber: number;
  editId?: string;
  compact?: boolean;
  onEdit: (id: string) => void;
  onClose: () => void;
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
    <View class="flex-1 min-w-0 bg-background">
      <View class="h-12 flex-row items-center justify-between gap-3 border-b border-border px-4">
        <View class="min-w-0 flex-1 flex-row items-center gap-2.5">
          <If
            when={props.compact}
            else={
              <View class="size-7 items-center justify-center rounded-lg bg-muted">
                <Icon name="file-text" size="sm" color="muted-foreground" />
              </View>
            }
          >
            <IconButton icon="arrow-left" label={t("changes.close")}
              tooltip={<Tooltip text={t("changes.close")} />} onPress={props.onClose} />
          </If>
          <Text class={`${type.ui} font-semibold text-foreground`}>{t("changes.title")}</Text>
          <Badge label={t("changes.turn", { number: props.turnNumber })} />
        </View>
        <If when={!props.compact}>
          <IconButton icon="x" label={t("changes.close")} tooltip={<Tooltip text={t("changes.close")} />} onPress={props.onClose} />
        </If>
      </View>
      <View class="min-h-9 flex-row flex-wrap items-center gap-x-2.5 gap-y-1 border-b border-border bg-muted/45 px-4 py-1.5">
        {/* Give the path a readable line before wrapping the edit controls together. */}
        <FilePath path={props.change.path} class={`grow basis-40 font-mono ${type.meta} text-foreground`} />
        <View class="flex-row items-center gap-2.5">
          <If when={props.change.edits.length > 1}>
            <View class="flex-row items-center gap-0.5 rounded-(--radius) border border-border bg-background px-1 py-0.5">
              <IconButton size="sm" icon="arrow-left" label={t("changes.previous")} disabled={index() === 0}
                onPress={() => props.onEdit(props.change.edits[index() - 1]!.id)} />
              <Text class={`px-1 ${type.meta} text-muted-foreground`}>{t("changes.editIndex", { current: index() + 1, total: props.change.edits.length })}</Text>
              <IconButton size="sm" icon="arrow-right" label={t("changes.next")} disabled={index() + 1 === props.change.edits.length}
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
