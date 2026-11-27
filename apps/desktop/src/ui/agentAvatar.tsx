import { colorScheme, token, View } from "nsh";

import { Icon } from "./icon";
import { motionClass } from "./motion";

/** Catppuccin accents an agent's icon may take; each exists in the Latte (light) and Mocha (dark) theme tokens. */
export const AGENT_HUES = ["blue", "green", "mauve", "peach", "pink", "teal", "sapphire", "yellow"] as const;

/** One of `AGENT_HUES`. */
export type AgentHue = (typeof AGENT_HUES)[number];

/** The hue a nickname always gets: a 31-based string hash over the pool, so the same agent keeps its color. */
export function agentHue(name: string): AgentHue {
  let hash = 0;
  for (let i = 0; i < name.length; i++) hash = (hash * 31 + name.charCodeAt(i)) >>> 0;
  return AGENT_HUES[hash % AGENT_HUES.length]!;
}

/** A subagent's robot icon in its name's color, with a pulsing dot while it works. */
export function AgentAvatar(props: { name: string; running?: boolean }) {
  const tint = () => String(token(`ctp-${colorScheme() === "dark" ? "mocha" : "latte"}-${agentHue(props.name)}`));
  return (
    <View class="size-4 shrink-0">
      <Icon name="bot" size="md" tint={tint()} />
      <If when={props.running}>
        <View class={`absolute -bottom-0.5 -right-0.5 size-2 rounded-full border border-card bg-info ${motionClass.pulse}`} />
      </If>
    </View>
  );
}
