import { Text } from "nsh";
import { Tab, Tabs as BaseTabs } from "nsh/base";

import { motionClass } from "./motion";
import { focusRing, type } from "./tokens";

/** One tab of `Tabs`: its value and its label. */
export type TabItem = Readonly<{ value: string; label: string; disabled?: boolean }>;

/**
 * A row of tabs with one indicator that slides to the selected tab. The app owns `value` and shows that
 * tab's content next to the list.
 */
export function Tabs(props: { items: readonly TabItem[]; value: string; accessibilityLabel: string; class?: string; onChange: (value: string) => void }) {
  return (
    <BaseTabs
      accessibilityLabel={props.accessibilityLabel}
      class={`flex-row gap-1 border-b border-border ${props.class ?? ""}`}
      indicatorClass={`bottom-0 h-[2px] rounded-full bg-primary ${motionClass.move}`}
    >
      <Each of={[...props.items]} key={(item) => item.value}>
        {(item) => (
          <Tab
            selected={props.value === item().value}
            disabled={item().disabled === true}
            onPress={() => props.onChange(item().value)}
            class={`group h-8 justify-center rounded-t-(--radius) px-3 hover:bg-accent disabled:opacity-50 ${focusRing} ${motionClass.wash}`}
          >
            <Text class={`${type.ui} text-muted-foreground group-selected:text-foreground group-selected:font-medium`}>{item().label}</Text>
          </Tab>
        )}
      </Each>
    </BaseTabs>
  );
}
