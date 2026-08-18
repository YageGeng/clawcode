import { Check, Copy } from "lucide-react";
import { Children, isValidElement, useState } from "react";
import type { ReactNode } from "react";
import ReactMarkdown, { defaultUrlTransform } from "react-markdown";
import type { Components } from "react-markdown";
import remarkGfm from "remark-gfm";

import type { MessageEntity } from "../../domain/model";
import { CommandMessageCard } from "./CommandMessageCard";
import { ReasoningBlock } from "./ReasoningBlock";

export type MessageViewProps = Readonly<{
  message: MessageEntity;
}>;

function CodeBlock({ text }: Readonly<{ text: string }>) {
  const [copied, setCopied] = useState(false);
  const value = text.replace(/\n$/, "");
  return (
    <div className="code-block">
      <button type="button" title="复制代码" onClick={() => {
        void navigator.clipboard.writeText(value).then(() => {
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1_200);
        });
      }}>{copied ? <Check size={14} /> : <Copy size={14} />} {copied ? "已复制" : "复制"}</button>
      <pre><code>{text}</code></pre>
    </div>
  );
}

const MARKDOWN_COMPONENTS: Components = {
  a: ({ children, ...props }) => <a {...props} target="_blank" rel="noreferrer noopener">{children}</a>,
  pre: ({ children }) => {
    const code = Children.count(children) === 1 ? Children.only(children) : undefined;
    if (isValidElement<Readonly<{ children?: ReactNode }>>(code) && code.type === "code") {
      const text = Children.toArray(code.props.children).map((child) =>
        typeof child === "string" ? child : typeof child === "number" || typeof child === "bigint" ? child.toString() : ""
      ).join("");
      return <CodeBlock text={text} />;
    }
    return <pre>{children}</pre>;
  }
};

const MarkdownUrlPolicy = {
  transform: (url: string): string => {
    const scheme = /^([a-z][a-z0-9+.-]*):/i.exec(url)?.[1]?.toLowerCase();
    if (scheme === undefined) return defaultUrlTransform(url);
    return ["http", "https", "mailto", "file", "urn"].includes(scheme) ? url : "";
  }
} as const;

export function MessageView({ message }: MessageViewProps) {
  if (message.slashCommand !== undefined) {
    return <CommandMessageCard message={message} />;
  }
  const assistant = message.assistant;
  return (
    <article className="message" data-role={message.role} data-error={assistant?.error === undefined ? undefined : "true"}>
      <div className="message__label">{message.role === "user" ? "你" : message.role === "assistant" ? "Agent" : "System"}</div>
      <ReasoningBlock message={message} />
      {assistant?.error === undefined ? null : <div className="message-error" role="alert"><strong>模型调用失败</strong><span>{assistant.error}</span></div>}
      {message.text.length === 0 ? null : (
        <div className="message__body">
          <ReactMarkdown remarkPlugins={[remarkGfm]} components={MARKDOWN_COMPONENTS} urlTransform={MarkdownUrlPolicy.transform}>{message.text}</ReactMarkdown>
          {message.streaming ? <span className="streaming-cursor" aria-label="正在生成" /> : null}
        </div>
      )}
      {message.images.length === 0 ? null : (
        <div className="message__images">
          {message.images.map((image, index) => <img key={`${image.mimeType}:${index}`} src={`data:${image.mimeType};base64,${image.data}`} alt={`消息图片 ${index + 1}`} />)}
        </div>
      )}
      <details className="message__diagnostics">
        <summary>消息详情</summary>
        <dl className="diagnostics-list">
          <dt>MessageId</dt><dd>{message.messageId}</dd>
          <dt>TurnId</dt><dd>{message.turnId}</dd>
          <dt>TimestampMs</dt><dd>{message.timestampMs}</dd>
          <dt>StartedAtMs</dt><dd>{message.startedAtMs}</dd>
          <dt>EndedAtMs</dt><dd>{message.endedAtMs}</dd>
          {assistant === undefined ? null : <>
            <dt>Provider</dt><dd>{assistant.providerId}</dd>
            <dt>Model</dt><dd>{assistant.modelId}</dd>
            <dt>StopReason</dt><dd>{assistant.stopReason}{assistant.rawStopReason === undefined ? "" : ` (${assistant.rawStopReason})`}</dd>
            <dt>Usage</dt><dd>in {assistant.usage.inputTokens} · out {assistant.usage.outputTokens} · cache read {assistant.usage.cacheReadTokens} · cache write {assistant.usage.cacheWriteTokens} · total {assistant.usage.totalTokens}</dd>
            {assistant.error === undefined ? null : <><dt>Error</dt><dd>{assistant.error}</dd></>}
          </>}
        </dl>
      </details>
    </article>
  );
}
