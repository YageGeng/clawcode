import { Text, View } from "nsh";
import { Tab, Tabs as BaseTabs } from "nsh/base";

import { motionClass } from "./motion";
import { focusRing, inlineLabel, type } from "./tokens";

/** One tab of `Tabs`: its value, label, and optional count badge. */
export type TabItem = Readonly<{ value: string; label: string; badge?: number; disabled?: boolean }>;

/**
 * A row of tabs with one indicator that slides to the selected tab.
 * `underline` draws its own bottom hairline; `bar` stretches inside a bordered header bar so the indicator rides on
 * the header's single bottom hairline without duplicating borders.
 */
export function Tabs(props: {
  items: readonly TabItem[];
  value: string;
  accessibilityLabel: string;
  variant?: "underline" | "bar";
  class?: string;
  onChange: (value: string) => void;
}) {
  const inBar = () => props.variant === "bar";
  return (
    <BaseTabs
      accessibilityLabel={props.accessibilityLabel}
      class={`flex-row items-center gap-1 ${inBar() ? "self-stretch" : "border-b border-border"} ${props.class ?? ""}`}
      indicatorClass={`bottom-0 h-[2px] rounded-full bg-primary ${motionClass.move}`}
    >
      <Each of={props.items} key={(item) => item.value}>
        {(item) => (
          <Tab
            selected={props.value === item().value}
            disabled={item().disabled === true}
            onPress={() => props.onChange(item().value)}
            class={`group ${inBar() ? "self-stretch justify-center px-0.5" : `h-8 justify-center rounded-t-(--radius) px-3 hover:bg-accent ${motionClass.wash}`} disabled:opacity-50 ${focusRing}`}
          >
            <View class={`flex-row items-center gap-1.5 ${inBar() ? `h-8 rounded-(--radius) px-2.5 group-hover:bg-accent group-disabled:hover:bg-transparent ${motionClass.wash}` : ""}`}>
              <Text class={`${inlineLabel} ${type.ui} text-muted-foreground group-selected:text-foreground group-selected:font-medium`}>{item().label}</Text>
              <If when={(item().badge ?? 0) > 0}>
                <View class="h-4.5 min-w-4.5 items-center justify-center rounded-full bg-muted px-1.5 group-selected:bg-secondary">
                  <Text class={`tabular-nums ${inlineLabel} ${type.meta} font-medium text-muted-foreground group-selected:text-foreground`}>{String(item().badge)}</Text>
                </View>
              </If>
            </View>
          </Tab>
        )}
      </Each>
    </BaseTabs>
  );
}
