import { LayoutAnimation, reduceMotion } from "nsh";

/** The app's motion tokens (DESIGN.md, "Motion tokens"): durations in ms, easings, distances and scales. */
export const motion = {
  instant: 120,
  fast: 180,
  base: 240,
  slow: 360,
  slower: 500,
  enter: "ease-out",
  move: "ease-in-out",
  leave: "ease-in",
  settle: "cubic-bezier(0.22, 1, 0.36, 1)",
  distance: { sm: 4, md: 8, lg: 16 },
  scale: { press: 0.97, pressIcon: 0.95, enter: 0.96 },
} as const;

/**
 * The same tokens as Tailwind classes, so components never write durations or easings themselves.
 * Every class that moves something also stops moving under reduced motion.
 */
export const motionClass = {
  /** A hover wash or a color change. */
  wash: "transition-colors duration-120 ease-out motion-reduce:transition-none",
  /** Content dimming or coming back, such as turns an edit will replace. */
  fade: "transition-opacity duration-180 ease-out motion-reduce:transition-none",
  /** A press: the whole control scales down on the soft settle. */
  press: "transition duration-180 ease-[cubic-bezier(0.22,1,0.36,1)] active:scale-[0.97] motion-reduce:transition-none motion-reduce:active:scale-100",
  /** A press on a square icon control. */
  pressIcon: "transition duration-180 ease-[cubic-bezier(0.22,1,0.36,1)] active:scale-[0.95] motion-reduce:transition-none motion-reduce:active:scale-100",
  /** A tooltip fading in 4pt toward its trigger. */
  tooltip: "transition duration-180 ease-out motion-reduce:transition-none",
  tooltipStarting: "opacity-0 translate-y-1",
  tooltipEnding: "opacity-0 duration-120",
  /** A toast sliding in from the bottom edge. */
  toast: "transition duration-360 ease-out motion-reduce:transition-none",
  toastStarting: "opacity-0 translate-y-4",
  toastEnding: "opacity-0 translate-y-4 duration-240 ease-in",
  /** A selection or a thumb moving between two places. */
  move: "transition-all duration-240 ease-in-out motion-reduce:transition-none",
  /** A disclosure chevron turning toward what it opened; add `rotate-90` (or a variant of it) when open. */
  turn: "transition-transform duration-180 motion-reduce:transition-none",
  /** Something working without an end in sight: a spinner. */
  spin: "animate-spin motion-reduce:animate-none",
  /** Something live or loading: a caret, a dot, a skeleton. */
  pulse: "animate-pulse motion-reduce:animate-none",
} as const;

/** Glides the next layout change (a panel opening, the composer growing) unless the user asked for less motion. */
export function glideNextLayout(): void {
  if (!reduceMotion()) LayoutAnimation.configureNext(LayoutAnimation.create(motion.base, "easeInEaseOut", "opacity"));
}
