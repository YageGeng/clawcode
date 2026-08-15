import { Ban, Check, ChevronDown, CircleAlert, LoaderCircle, Wrench } from "lucide-react";

import type { ToolCallEntity } from "../../domain/model";

export type ToolCallCardProps = Readonly<{
  tool: ToolCallEntity;
}>;

/** Renders ACP v2 text, image, and diff tool content without exposing transport JSON. */
function ToolOutput({ content, rawOutput }: Pick<ToolCallEntity, "content" | "rawOutput">) {
  if (content.length === 0) {
    return rawOutput === undefined ? null : <pre>{JSON.stringify(rawOutput, null, 2)}</pre>;
  }
  return <div className="tool-output">
    {content.map((item, index) => {
      if (typeof item !== "object" || item === null || Array.isArray(item)) {
        return <pre key={index}>{JSON.stringify(item, null, 2)}</pre>;
      }
      const entry = item as Readonly<Record<string, unknown>>;
      if (entry.type === "content" && typeof entry.content === "object" && entry.content !== null && !Array.isArray(entry.content)) {
        const block = entry.content as Readonly<Record<string, unknown>>;
        if (block.type === "text" && typeof block.text === "string") return <pre key={index}>{block.text}</pre>;
        if (block.type === "image" && typeof block.data === "string" && typeof block.mimeType === "string") {
          return <figure className="tool-output__image" key={index}>
            <img src={`data:${block.mimeType};base64,${block.data}`} alt="工具返回的图片" />
            <figcaption>{block.mimeType}</figcaption>
          </figure>;
        }
      }
      if (entry.type === "diff" && typeof entry.patch === "object" && entry.patch !== null && !Array.isArray(entry.patch)) {
        const patch = entry.patch as Readonly<Record<string, unknown>>;
        if (typeof patch.text === "string") return <pre className="tool-output__diff" key={index}>{patch.text}</pre>;
      }
      return <pre key={index}>{JSON.stringify(entry, null, 2)}</pre>;
    })}
  </div>;
}

/** Renders one ACP tool lifecycle with product-specific blocked refinement. */
export function ToolCallCard({ tool }: ToolCallCardProps) {
  const statusLabel = tool.status === "completed" ? "完成" : tool.status === "blocked" ? "已阻止" : tool.status === "failed" ? "失败" : tool.status === "in_progress" ? "执行中" : "等待中";
  const icon = tool.status === "completed" ? <Check size={14} /> : tool.status === "blocked" ? <Ban size={14} /> : tool.status === "failed" ? <CircleAlert size={14} /> : tool.status === "in_progress" ? <LoaderCircle className="spin" size={14} /> : <Wrench size={14} />;
  const elapsed = tool.startedAtMs === undefined || tool.endedAtMs === undefined ? undefined : BigInt(tool.endedAtMs) - BigInt(tool.startedAtMs);
  return (
    <details className="tool-card" data-status={tool.status}>
      <summary>
        <span className="tool-card__icon">{icon}</span>
        <span className="tool-card__title">{tool.title}</span>
        <span className="tool-card__status">{statusLabel}{elapsed === undefined ? "" : ` · ${elapsed} ms`}</span>
        <ChevronDown className="tool-card__chevron" size={14} aria-hidden="true" />
      </summary>
      <div className="tool-card__details">
        {tool.rawInput === undefined ? null : <section><h4>输入</h4><pre>{JSON.stringify(tool.rawInput, null, 2)}</pre></section>}
        {tool.content.length === 0 && tool.rawOutput === undefined ? null : <section><h4>输出</h4><ToolOutput content={tool.content} rawOutput={tool.rawOutput} /></section>}
        <dl className="diagnostics-list">
          <dt>ToolCallId</dt><dd>{tool.toolCallId}</dd>
          {tool.meta === undefined ? null : <><dt>TurnId</dt><dd>{tool.meta.turnId}</dd><dt>TimestampMs</dt><dd>{tool.meta.timestampMs}</dd></>}
          {tool.startedAtMs === undefined ? null : <><dt>StartedAtMs</dt><dd>{tool.startedAtMs}</dd></>}
          {tool.endedAtMs === undefined ? null : <><dt>EndedAtMs</dt><dd>{tool.endedAtMs}</dd></>}
        </dl>
      </div>
    </details>
  );
}
