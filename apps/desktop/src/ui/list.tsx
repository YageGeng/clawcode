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
  /** Keeps hover-revealed `actions` visible while a menu owned by this row is open. */
  actionsVisible?: boolean;
  onPress?: () => void;
}) {
  // Multi-line rows anchor their leading icon to the 20pt title line and use generous vertical padding;
  // single-line rows use 4pt vertical padding so a 24pt `sm` action button keeps the row at the 32pt grid.
  const multiline = () => props.description !== undefined;
  // When a row has both a resting status indicator and hover-revealed actions, overlay the actions on the
  // trailing slot so the resting indicator sits flush at the row's right spine and labels keep full width.
  const overlayActions = "actions" in props && "trailing" in props;
  const actionsShown = () => props.actionsVisible === true;
  return (
    <BaseButton
      accessibilityLabel={props.accessibilityLabel}
      selected={props.selected === true}
      disabled={props.disabled === true}
      onPress={props.onPress}
      class={`group relative min-h-8 flex-row gap-2 rounded-(--radius) px-2.5 ${multiline() ? "items-start py-1.5" : "items-center py-1"} hover:bg-accent active:bg-sidebar-accent/80 selected:bg-sidebar-accent disabled:opacity-50 ${focusRing} ${motionClass.wash}`}
    >
      <If
        when={"leading" in props}
        else={
          <If when={props.icon}>
            {(icon) => (
              <View class={multiline() ? "h-5 shrink-0 items-center justify-center" : "shrink-0"}>
                <Icon name={icon()} size="md" color="muted-foreground" />
              </View>
            )}
          </If>
        }
      >
        <View class={multiline() ? "h-5 shrink-0 items-center justify-center" : "shrink-0"}>{props.leading}</View>
      </If>
      <View class={`min-w-0 flex-1 ${multiline() ? "gap-0.5" : ""} ${overlayActions ? (actionsShown() ? "pr-6" : "group-hover:pr-6 group-focus-within:pr-6") : ""}`}>
        <If when={props.mono === true} else={
          <Text class={`${multiline() ? "" : inlineLabel} ${type.ui} text-foreground group-selected:font-medium truncate`}>{props.label}</Text>
        }>
          <FilePath path={props.label} class={`${multiline() ? "" : inlineLabel} font-mono ${type.meta} text-foreground group-selected:font-medium`} />
        </If>
        <If when={props.description}>{(text) => <Text class={`${type.meta} text-muted-foreground truncate`}>{text()}</Text>}</If>
      </View>
      <If when={overlayActions} else={props.trailing}>
        <View class={`shrink-0 flex-row items-center ${actionsShown() ? "opacity-0" : "group-hover:opacity-0 group-focus-within:opacity-0"} transition-opacity duration-120 ease-out motion-reduce:transition-none`}>
          {props.trailing}
        </View>
      </If>
      <If when={"actions" in props}>
        <View class={`${overlayActions ? "absolute inset-y-0 right-1.5 items-center" : ""} flex-row gap-0.5 ${actionsShown() ? "opacity-100" : "opacity-0 group-hover:opacity-100 group-focus-within:opacity-100"} transition-opacity duration-120 ease-out motion-reduce:transition-none`}>
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
