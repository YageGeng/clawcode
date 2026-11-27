import { cleanup, View } from "nsh";
import type { ViewRef } from "nsh";
import { t } from "nsh/i18n";

import type { FileChange } from "../client/domain/model";
import type { TurnView } from "../client/turns";
import { Button } from "../ui/button";
import { Disclosure } from "../ui/disclosure";
import { FileIcon } from "../ui/fileIcon";
import { FilePath } from "../ui/filePath";
import { LineCounts, Surface } from "../ui/surface";
import { type } from "../ui/tokens";

/** The inspector may restore focus only while this trigger belongs to its original turn. */
export type ChangeOpener = Readonly<{ node: ViewRef; isMounted: () => boolean }>;

/** Omitting the file reviews the turn without resetting a diff that is already open. */
export type ViewTurnChange = (turn: TurnView, trigger?: ChangeOpener, file?: FileChange) => void;

/** Opens the inspector and lets it return focus while this button still belongs to the same turn. */
function ChangeButton(props: { turnKey: string; path?: string; selected?: boolean; onOpen: (trigger?: ChangeOpener) => void }) {
  let trigger: ViewRef | undefined;
  let mounted = true;
  cleanup(() => { mounted = false; });
  return (
    <Button
      label={t(props.path === undefined ? "changes.review" : "changes.view")}
      accessibilityLabel={props.path === undefined ? undefined : t("changes.viewFile", { path: props.path })}
      icon={props.path === undefined ? "file-diff" : undefined}
      size="sm" variant={props.path === undefined ? "subtle" : "outline"} class="shrink-0"
      selected={props.selected === true}
      buttonRef={(node) => { trigger = node; }}
      onPress={() => {
        const key = props.turnKey;
        props.onOpen(trigger ? { node: trigger, isMounted: () => mounted && props.turnKey === key } : undefined);
      }}
    />
  );
}

/** A turn's folded change summary and the file cards revealed by its disclosure. */
export function TurnChanges(props: {
  turn: TurnView; open: boolean; selectedPath?: string; onToggle: () => void;
  onViewChange: ViewTurnChange;
}) {
  return (
    <Disclosure label={t(props.turn.changes.length === 1 ? "turn.changedFile" : "turn.changedFiles", { count: props.turn.changes.length })}
      accessibilityHint={t("changes.lineCounts", props.turn.lineCounts)}
      open={props.open} onToggle={props.onToggle}
      trailing={<LineCounts added={props.turn.lineCounts.added} removed={props.turn.lineCounts.removed} />}
      action={<ChangeButton turnKey={props.turn.key} onOpen={(trigger) => props.onViewChange(props.turn, trigger)} />}>
      <Each of={props.turn.changes} key={(change) => change.path}>
        {(change) => (
          <ChangeCard turnKey={props.turn.key} change={change()} selected={props.selectedPath === change().path}
            onOpen={(trigger) => props.onViewChange(props.turn, trigger, change())} />
        )}
      </Each>
    </Disclosure>
  );
}

/** Opens the selected file’s recorded edits from this turn, highlighting the card while active. */
function ChangeCard(props: { turnKey: string; change: FileChange; selected: boolean; onOpen: (trigger?: ChangeOpener) => void }) {
  return (
    <Surface class={`flex-row items-center gap-3 p-3 ${props.selected ? "border-ring/50 bg-accent/40" : ""}`}>
      <View class={`size-9 items-center justify-center rounded-(--radius) ${props.selected ? "bg-info/15" : "bg-muted"}`}>
        <FileIcon path={props.change.path} size="md" />
      </View>
      <View class="min-w-0 flex-1 gap-0.5">
        <FilePath path={props.change.path} label={t("changes.edited", { file: props.change.path })}
          class={`${type.ui} ${props.selected ? "font-medium" : ""} text-card-foreground`} />
        <LineCounts added={props.change.added} removed={props.change.removed} />
      </View>
      <ChangeButton turnKey={props.turnKey} path={props.change.path} selected={props.selected} onOpen={props.onOpen} />
    </Surface>
  );
}
