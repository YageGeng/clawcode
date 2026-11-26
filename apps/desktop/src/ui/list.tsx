import { Text, View } from "nsh";
import { Button as BaseButton, Tree } from "nsh/base";
import type { TreeItem } from "nsh/base";
import type { JSX } from "nsh/jsx-runtime";

import { Icon } from "./icon";
import type { IconName } from "./icon";
import { motionClass } from "./motion";
import { focusRing, type } from "./tokens";

/**
 * One pressable row of a list (a sidebar row): icon, a one-line label, secondary text, a trailing slot, and
 * `actions` that appear on hover or keyboard focus (they must also be reachable from the row's menu).
 */
export function ListItem(props: {
  label: string;
  description?: string;
  icon?: IconName;
  selected?: boolean;
  disabled?: boolean;
  trailing?: JSX.Element;
  actions?: JSX.Element;
  onPress?: () => void;
}) {
  return (
    <BaseButton
      selected={props.selected === true}
      disabled={props.disabled === true}
      onPress={props.onPress}
      class={`group min-h-8 flex-row items-center gap-2 rounded-(--radius) px-2.5 py-1.5 hover:bg-accent selected:bg-sidebar-accent disabled:opacity-50 ${focusRing} ${motionClass.wash}`}
    >
      <If when={props.icon}>{(icon) => <Icon name={icon()} size="md" color="muted-foreground" />}</If>
      <View class="flex-1">
        <Text class={`${type.ui} text-foreground truncate`}>{props.label}</Text>
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
      class={props.class}
      listClass="gap-px"
      itemClass={`group h-8 flex-row items-center rounded-(--radius) px-1.5 hover:bg-accent selected:bg-sidebar-accent focus:ring-1 focus:ring-ring ${motionClass.wash}`}
      renderItem={(entry) => (
        <View class="flex-row items-center gap-1" style={{ paddingLeft: entry.depth * 12 }}>
          <View class="size-4 items-center justify-center">
            <If when={entry.folder}>
              <View class={`group-aria-expanded:rotate-90 transition-transform duration-180 motion-reduce:transition-none`}>
                <Icon name="chevron-right" size="sm" color="muted-foreground" />
              </View>
            </If>
          </View>
          <Text class={`${type.ui} text-foreground truncate`}>{entry.label}</Text>
        </View>
      )}
    />
  );
}
