import { Text, View } from "nsh";
import { Button as BaseButton, Tree } from "nsh/base";
import type { TreeItem } from "nsh/base";
import type { JSX } from "nsh/jsx-runtime";

import { FileIcon } from "./fileIcon";
import { FilePath } from "./filePath";
import { Icon } from "./icon";
import type { IconName } from "./icon";
import { motionClass } from "./motion";
import { focusRing, inlineLabel, type } from "./tokens";

/**
 * One pressable row of a list (a sidebar or inspector row): icon or custom leading slot, a one-line label, secondary
 * text, a trailing slot, and `actions` that appear on hover or keyboard focus (they must also be reachable from a menu).
 */
export function ListItem(props: {
  label: string;
  accessibilityLabel?: string;
  description?: string;
  icon?: IconName;
  leading?: JSX.Element;
  mono?: boolean;
  selected?: boolean;
  disabled?: boolean;
  trailing?: JSX.Element;
  actions?: JSX.Element;
  onPress?: () => void;
}) {
  return (
    <BaseButton
      accessibilityLabel={props.accessibilityLabel}
      selected={props.selected === true}
      disabled={props.disabled === true}
      onPress={props.onPress}
      class={`group min-h-8 flex-row items-center gap-2 rounded-(--radius) px-2.5 py-1.5 hover:bg-accent selected:bg-sidebar-accent disabled:opacity-50 ${focusRing} ${motionClass.wash}`}
    >
      {props.leading ?? <If when={props.icon}>{(icon) => <Icon name={icon()} size="md" color="muted-foreground" />}</If>}
      <View class="min-w-0 flex-1">
        <If when={props.mono === true} else={
          <Text class={`${props.description === undefined ? inlineLabel : ""} ${type.ui} text-foreground group-selected:font-medium truncate`}>{props.label}</Text>
        }>
          <FilePath path={props.label} class={`${props.description === undefined ? inlineLabel : ""} font-mono ${type.meta} text-foreground group-selected:font-medium`} />
        </If>
        <If when={props.description}>{(text) => <Text class={`${type.meta} text-muted-foreground truncate`}>{text()}</Text>}</If>
      </View>
      {props.trailing}
      <If when={props.actions !== undefined}>
        <View class={`flex-row gap-0.5 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 transition-opacity duration-120 ease-out motion-reduce:transition-none`}>
          {props.actions}
        </View>
      </If>
    </BaseButton>
  );
}

/** A tree of rows (files, sessions with children); the app owns what is expanded and selected. */
export function TreeList(props: {
  items: TreeItem[];
  symlinkPaths?: ReadonlySet<string>;
  expanded: string[];
  selected: string | null;
  accessibilityLabel: string;
  class?: string;
  onExpandedChange: (expanded: string[]) => void;
  onSelectedChange: (id: string) => void;
  onAction?: (id: string) => void;
}) {
  return (
    <Tree
      items={props.items}
      expanded={props.expanded}
      selected={props.selected}
      accessibilityLabel={props.accessibilityLabel}
      onExpandedChange={(event) => props.onExpandedChange(event.expanded)}
      onSelectedChange={(event) => props.onSelectedChange(event.selected)}
      onAction={(event) => props.onAction?.(event.id)}
      reveal
      class={props.class}
      listClass="py-1.5"
      itemClass={`group mx-2 h-7 justify-center rounded-(--radius) px-2 hover:bg-accent selected:bg-sidebar-accent disabled:opacity-70 disabled:hover:bg-transparent ${motionClass.wash}`}
      renderItem={(entry) => (
        <View class="min-w-0 flex-row items-center gap-1.5" style={{ paddingLeft: entry.depth * 12 }}>
          {/* The tree projects keyboard focus onto its active row. Keep that cue inside the selection. */}
          <View accessibilityElementsHidden class="absolute -left-0.5 h-4 w-0.5 rounded-full bg-transparent group-focus-visible:bg-ring" />
          <View class="size-4 shrink-0 items-center justify-center">
            <If when={entry.folder}>
              <View class={`group-aria-expanded:rotate-90 ${motionClass.turn}`}>
                <Icon name="chevron-right" size="sm" color="muted-foreground" />
              </View>
            </If>
          </View>
          <If when={entry.folder || !entry.id.endsWith("/")}>
            <View class="shrink-0">
              <FileIcon path={entry.label} kind={entry.folder ? "directory" : props.symlinkPaths?.has(entry.id) ? "symlink" : "file"}
                open={entry.folder && props.expanded.includes(entry.id)} size="md" />
            </View>
          </If>
          <Text class={`min-w-0 flex-1 ${inlineLabel} ${type.ui} text-foreground group-disabled:text-muted-foreground group-selected:font-medium truncate`}>{entry.label}</Text>
        </View>
      )}
    />
  );
}
