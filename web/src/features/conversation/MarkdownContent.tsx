import { lazy, memo, Suspense } from "react";

const MarkdownRenderer = lazy(() => import("./MarkdownRenderer"));

export type MarkdownContentProps = Readonly<{
  text: string;
  // Skip syntax highlighting while a message is streaming; a settled block only
  // highlights once, avoiding a re-highlight pass on every growing frame.
  highlight?: boolean;
}>;

export type CodeBlockProps = Readonly<{
  language: string;
  text: string;
}>;

/** Loads the Markdown parser and syntax highlighter only after display content exists. */
export const MarkdownContent = memo(function MarkdownContent({ text, highlight = true }: MarkdownContentProps) {
  return (
    <Suspense fallback={<span className="markdown-fallback">{text}</span>}>
      <MarkdownRenderer text={text} highlight={highlight} />
    </Suspense>
  );
});

/** Renders arbitrary text through a collision-safe fenced Markdown code block. */
export function CodeBlock({ language, text }: CodeBlockProps) {
  // Scan incrementally so very large tool output cannot overflow the call stack
  // by spreading every backtick run into Math.max.
  let longestFence = 0;
  for (const match of text.matchAll(/`+/g)) longestFence = Math.max(longestFence, match[0].length);
  const fence = "`".repeat(Math.max(3, longestFence + 1));
  return <MarkdownContent text={`${fence}${language}\n${text}\n${fence}`} />;
}

/** Formats one JSON-compatible value as a highlighted, copyable code block. */
export function JsonBlock({ value }: Readonly<{ value: unknown }>) {
  const serialized = JSON.stringify(value, null, 2);
  return <CodeBlock language="json" text={serialized ?? String(value)} />;
}
