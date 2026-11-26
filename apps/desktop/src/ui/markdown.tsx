import { TextView } from "nsh/base";
import type { JSX } from "nsh/jsx-runtime";

import { type } from "./tokens";

/** Each Markdown part's look, from tokens; `muted` is for commentary inside folded work. */
const parts = (tone: "default" | "muted") => ({
  root: `gap-3 ${type.body} ${tone === "muted" ? "text-muted-foreground" : "text-foreground"}`,
  paragraph: type.body,
  heading1: "text-xl font-semibold",
  heading2: "text-lg font-semibold",
  heading3: "text-base font-semibold",
  code: "font-mono rounded-md bg-muted px-1 text-[13px]",
  codeBlock: "rounded-(--radius) bg-muted",
  codeBlockCode: "font-mono text-[13px] leading-[20px]",
  link: "text-link",
  blockquote: "border-l-2 border-border pl-3 text-muted-foreground",
  listMarker: "text-muted-foreground",
});

/** Markdown text of the conversation; `streaming` renders a reply as it grows, with an optional caret. */
export function Markdown(props: { text: string; streaming?: boolean; tone?: "default" | "muted"; caret?: JSX.Element }) {
  return (
    <TextView
      markdown={props.text}
      selectable
      options={{ streaming: props.streaming === true }}
      classes={parts(props.tone ?? "default")}
      streamingCaret={props.caret}
    />
  );
}
