/** The type scale as classes (spec §5.1); sizes come from theme tokens so settings can change them. */
export const type = {
  body: "text-(length:--text-body) leading-[24px]",
  ui: "text-(length:--text-ui) leading-[20px]",
  meta: "text-(length:--text-meta) leading-[18px]",
  heading: "text-(length:--text-heading) leading-[36px] font-medium",
} as const;

/** A meaning a status color carries; `neutral` carries none. */
export type Tone = "neutral" | "success" | "warning" | "danger" | "info";

/** A quiet tinted background and its text, for badges and banners of each tone. */
export const toneSoft: Readonly<Record<Tone, string>> = {
  neutral: "bg-muted text-muted-foreground",
  success: "bg-success/15 text-success",
  warning: "bg-warning/15 text-warning",
  danger: "bg-destructive/15 text-destructive",
  info: "bg-info/15 text-info",
};

/** The solid fill of each tone, for dots and indicators. */
export const toneFill: Readonly<Record<Tone, string>> = {
  neutral: "bg-muted-foreground",
  success: "bg-success",
  warning: "bg-warning",
  danger: "bg-destructive",
  info: "bg-info",
};

/** The theme token of each tone's text color, for `token()` where no class reaches (an icon's tint). */
export const toneToken = {
  neutral: "muted-foreground",
  success: "success",
  warning: "warning",
  danger: "destructive",
  info: "info",
} as const;

/**
 * Lifts single-line UI labels by 1pt inside `items-center` rows and fixed-height pills.
 * System sans-serif fonts reserve more ascent (diacritic headroom above capitals) than descent,
 * placing the cap/x-height and CJK optical center ~1pt below the line box's geometric midpoint.
 */
export const inlineLabel = "-translate-y-px";

/** The focus ring every pressable control draws for the keyboard, outside its border. */
export const focusRing = "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background";
