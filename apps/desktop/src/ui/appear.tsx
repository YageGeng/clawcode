import { createMotion, View } from "nsh";
import type { MotionSpec } from "nsh";
import type { JSX } from "nsh/jsx-runtime";

import { motion } from "./motion";

/** How a view comes in: rising 8pt (cards, messages), rising 4pt (small floating controls), growing from 0.96, or fading. */
export type Entrance = "rise" | "riseSm" | "grow" | "fade";

/** Each entrance's keyframes and duration, from the motion tokens. */
const entrances: Readonly<Record<Entrance, MotionSpec>> = {
  rise: { keyframes: { opacity: [0, 1], translateY: [motion.distance.md, 0] }, duration: motion.slow },
  riseSm: { keyframes: { opacity: [0, 1], translateY: [motion.distance.sm, 0] }, duration: motion.fast },
  grow: { keyframes: { opacity: [0, 1], scale: [motion.scale.enter, 1] }, duration: motion.base },
  fade: { keyframes: { opacity: [0, 1] }, duration: motion.fast },
};

/**
 * Plays an entrance once, natively, as its content mounts (it lands at once under reduced motion). With
 * `play={false}` the content simply appears: a row a virtualized list remounts on scroll is not arriving.
 * `class` is layout only.
 */
export function Appear(props: { children: JSX.Element; entrance?: Entrance; play?: boolean; class?: string }) {
  // Read once: the entrance belongs to this mount.
  const spec = entrances[props.entrance ?? "rise"];
  const entering = createMotion(props.play === false ? {} : { enter: { ...spec, easing: "easeOut" } });
  return (
    <View class={props.class} style={entering.style}>
      {props.children}
    </View>
  );
}
