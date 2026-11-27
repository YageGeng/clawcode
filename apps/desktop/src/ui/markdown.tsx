import { TextView } from "nsh/base";
import type { JSX } from "nsh/jsx-runtime";

import { type } from "./tokens";

/** Each Markdown part's look, from tokens; `muted` is for commentary inside folded work. */
const parts = (tone: "default" | "muted") => ({
  root: `gap-3.5 ${type.body} ${tone === "muted" ? "text-muted-foreground" : "text-foreground"}`,
  paragraph: type.body,
  heading1: "text-[20px] leading-[28px] font-semibold tracking-tight text-foreground pt-2 pb-1.5 border-b border-border",
  heading2: "text-[17px] leading-[25px] font-semibold tracking-tight text-foreground pt-2",
  heading3: "text-[15px] leading-[22px] font-semibold text-foreground pt-1",
  strong: "font-semibold",
  code: "font-mono rounded-[6px] bg-muted px-1.5 py-0.5 text-[13px] text-foreground",
  codeBlock: "rounded-(--radius) border border-border bg-muted/80 p-3",
  codeBlockCode: "font-mono text-[13px] leading-[20px]",
  link: "text-link",
  blockquote: "rounded-r-(--radius) border-l-[3px] border-primary/25 bg-muted/45 py-2 pl-3.5 pr-3 text-muted-foreground",
  list: "gap-1.5",
  listMarker: "font-medium text-muted-foreground",
  tableHead: "bg-muted/75",
  tableHeaderCell: "px-3 py-2 font-semibold text-foreground",
  tableCell: "px-3 py-2",
  rule: "my-1 border-t border-border",
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
