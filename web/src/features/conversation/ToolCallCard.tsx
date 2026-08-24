import { Ban, Check, ChevronDown, CircleAlert, LoaderCircle, ScanSearch, Wrench } from "lucide-react";
import { memo, useState } from "react";

import type { ToolCallEntity } from "../../domain/model";
import { CodeBlock, JsonBlock, MarkdownContent } from "./MarkdownContent";

/** Parses one JSON object or array string once while tolerating arbitrary text. */
function parseJsonLike(text: string): Readonly<{ value: unknown }> | undefined {
  const trimmed = text.trim();
  if (trimmed.length === 0 || !(trimmed.startsWith("{") || trimmed.startsWith("["))) return undefined;
  try {
    return { value: JSON.parse(trimmed) as unknown };
  } catch {
    return undefined;
  }
}

export type ToolCallCardProps = Readonly<{
  tool: ToolCallEntity;
  inspecting?: boolean;
  onInspect?: (tool: ToolCallEntity) => void;
}>;

/** Renders ACP text as Markdown while preserving image, diff, and structured JSON output. */
function ToolOutput({ content, rawOutput }: Pick<ToolCallEntity, "content" | "rawOutput">) {
  if (content.length === 0) {
    if (rawOutput === undefined) return null;
    if (typeof rawOutput !== "string") return <JsonBlock value={rawOutput} />;
    const parsed = parseJsonLike(rawOutput);
    return parsed === undefined
      ? <div className="tool-output__markdown"><MarkdownContent text={rawOutput} /></div>
      : <JsonBlock value={parsed.value} />;
  }
  return <div className="tool-output">
    {content.map((item, index) => {
      if (typeof item !== "object" || item === null || Array.isArray(item)) {
        if (typeof item !== "string") return <JsonBlock key={index} value={item} />;
        const parsed = parseJsonLike(item);
        return parsed === undefined
          ? <div className="tool-output__markdown" key={index}><MarkdownContent text={item} /></div>
          : <JsonBlock key={index} value={parsed.value} />;
      }
      const entry = item as Readonly<Record<string, unknown>>;
      if (entry.type === "content" && typeof entry.content === "object" && entry.content !== null && !Array.isArray(entry.content)) {
        const block = entry.content as Readonly<Record<string, unknown>>;
        if (block.type === "text" && typeof block.text === "string") {
          return <div className="tool-output__markdown" key={index}><MarkdownContent text={block.text} /></div>;
        }
        if (block.type === "image" && typeof block.data === "string" && typeof block.mimeType === "string") {
          return <figure className="tool-output__image" key={index}>
            <img src={`data:${block.mimeType};base64,${block.data}`} alt="工具返回的图片" />
            <figcaption>{block.mimeType}</figcaption>
          </figure>;
        }
      }
      if (entry.type === "diff" && typeof entry.patch === "object" && entry.patch !== null && !Array.isArray(entry.patch)) {
        const patch = entry.patch as Readonly<Record<string, unknown>>;
        if (typeof patch.text === "string") return <CodeBlock language="diff" key={index} text={patch.text} />;
      }
      return <JsonBlock key={index} value={entry} />;
    })}
  </div>;
}

/** Renders one ACP tool lifecycle with product-specific blocked refinement. */
export const ToolCallCard = memo(function ToolCallCard({ tool, inspecting = false, onInspect }: ToolCallCardProps) {
  // Bash cards first mount from the in-progress ACP update, so reveal streamed
  // output immediately while preserving the user's later expand/collapse choice.
  const [open, setOpen] = useState(tool.title === "bash" && tool.status === "in_progress");
  const statusLabel = tool.status === "completed" ? "完成" : tool.status === "blocked" ? "已阻止" : tool.status === "failed" ? "失败" : tool.status === "in_progress" ? "执行中" : "等待中";
  const icon = tool.status === "completed" ? <Check size={14} /> : tool.status === "blocked" ? <Ban size={14} /> : tool.status === "failed" ? <CircleAlert size={14} /> : tool.status === "in_progress" ? <LoaderCircle className="spin" size={14} /> : <Wrench size={14} />;
  const elapsed = tool.startedAtMs === undefined || tool.endedAtMs === undefined ? undefined : BigInt(tool.endedAtMs) - BigInt(tool.startedAtMs);
  return (
    <details className="tool-card" data-status={tool.status} open={open}>
      <summary onClick={(event) => {
        // Mount large tool payloads only on demand; collapsed tool cards are
        // common during replay and must not parse hidden Markdown or JSON.
        event.preventDefault();
        setOpen((current) => !current);
      }}>
        <span className="tool-card__icon">{icon}</span>
        <span className="tool-card__title">{tool.title}</span>
        <span className="tool-card__status">{statusLabel}{elapsed === undefined ? "" : ` · ${elapsed} ms`}</span>
        <ChevronDown className="tool-card__chevron" size={14} aria-hidden="true" />
      </summary>
      {open ? <div className="tool-card__details">
        {onInspect === undefined ? null : <button className="tool-card__inspect" type="button" aria-label="在 Context Lens 中检查工具调用" aria-pressed={inspecting} onClick={() => onInspect(tool)}><ScanSearch size={13} aria-hidden="true" />Context Lens</button>}
        <div className="tool-card__lifecycle" aria-label="工具生命周期">
          <span><i data-phase="start" />Start{tool.startedAtMs === undefined ? " · 等待事件" : ` · ${tool.startedAtMs}`}</span>
          <span><i data-phase="end" data-complete={tool.endedAtMs !== undefined} />End{tool.endedAtMs === undefined ? " · 等待结果" : ` · ${tool.endedAtMs}`}</span>
        </div>
        {tool.rawInput === undefined ? null : <section><h4>输入</h4><JsonBlock value={tool.rawInput} /></section>}
        {tool.content.length === 0 && tool.rawOutput === undefined ? null : <section><h4>输出</h4><ToolOutput content={tool.content} rawOutput={tool.rawOutput} /></section>}
        <dl className="diagnostics-list">
          <dt>ToolCallId</dt><dd>{tool.toolCallId}</dd>
          {tool.meta === undefined ? null : <><dt>TurnId</dt><dd>{tool.meta.turnId}</dd><dt>TimestampMs</dt><dd>{tool.meta.timestampMs}</dd></>}
          {tool.startedAtMs === undefined ? null : <><dt>StartedAtMs</dt><dd>{tool.startedAtMs}</dd></>}
          {tool.endedAtMs === undefined ? null : <><dt>EndedAtMs</dt><dd>{tool.endedAtMs}</dd></>}
          {tool.sequenceRange === undefined ? null : <><dt>SequenceRange</dt><dd>{tool.sequenceRange.start}–{tool.sequenceRange.end}</dd></>}
        </dl>
      </div> : null}
    </details>
  );
});
