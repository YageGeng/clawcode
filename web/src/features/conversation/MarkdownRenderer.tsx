import { Check, Copy } from "lucide-react";
import { useRef, useState } from "react";
import type { ReactNode } from "react";
import ReactMarkdown, { defaultUrlTransform } from "react-markdown";
import type { Components } from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import rehypeKatex from "rehype-katex";
import type { PluggableList } from "unified";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";

import type { MarkdownContentProps } from "./MarkdownContent";

/** Frames one rendered Markdown code block with copy support. */
function CodeFrame({ children }: Readonly<{ children: ReactNode }>) {
  const [copied, setCopied] = useState(false);
  const pre = useRef<HTMLPreElement>(null);
  return (
    <div className="code-block">
      <button type="button" title="复制代码" onClick={() => {
        const value = pre.current?.textContent?.replace(/\n$/, "") ?? "";
        void navigator.clipboard.writeText(value).then(() => {
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1_200);
        });
      }}>{copied ? <Check size={14} /> : <Copy size={14} />} {copied ? "已复制" : "复制"}</button>
      <pre ref={pre}>{children}</pre>
    </div>
  );
}

const MARKDOWN_COMPONENTS: Components = {
  a: ({ children, ...props }) => <a {...props} target="_blank" rel="noreferrer noopener">{children}</a>,
  pre: ({ children }) => <CodeFrame>{children}</CodeFrame>
};
const REMARK_PLUGINS: PluggableList = [remarkGfm, remarkMath];
// Keep formula rendering active while syntax highlighting is deferred for streaming messages.
const MATH_REHYPE_PLUGINS: PluggableList = [rehypeKatex];
const SETTLED_REHYPE_PLUGINS: PluggableList = [rehypeKatex, [rehypeHighlight, { detect: false }]];

const MarkdownUrlPolicy = {
  /** Allows useful local and remote links while rejecting executable URL schemes. */
  transform: (url: string): string => {
    const scheme = /^([a-z][a-z0-9+.-]*):/i.exec(url)?.[1]?.toLowerCase();
    if (scheme === undefined) return defaultUrlTransform(url);
    return ["http", "https", "mailto", "file", "urn"].includes(scheme) ? url : "";
  }
} as const;

/** Renders trusted display text as safe GFM with optional syntax highlighting. */
export default function MarkdownRenderer({ text, highlight = true }: MarkdownContentProps) {
  return (
    <ReactMarkdown
      remarkPlugins={REMARK_PLUGINS}
      rehypePlugins={highlight ? SETTLED_REHYPE_PLUGINS : MATH_REHYPE_PLUGINS}
      components={MARKDOWN_COMPONENTS}
      urlTransform={MarkdownUrlPolicy.transform}
    >
      {text}
    </ReactMarkdown>
  );
}
