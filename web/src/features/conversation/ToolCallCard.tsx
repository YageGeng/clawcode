import { Ban, Check, ChevronDown, CircleAlert, CircleSlash, FilePen, FileText, LoaderCircle, ScanSearch, Search, SquareTerminal, Wrench } from "lucide-react";
import { memo, useEffect, useState } from "react";

import type { ToolCallEntity, ToolKind } from "../../domain/model";
import { CodeBlock, JsonBlock, MarkdownContent } from "./MarkdownContent";
import { TerminalView } from "./TerminalView";
import type { TerminalEntity } from "../../workspace/terminals";
import type { WorkspaceController } from "../../workspace/controller";
import { SubagentPanel } from "./SubagentPanel";

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
  terminal?: TerminalEntity;
  inspecting?: boolean;
  onInspect?: (tool: ToolCallEntity) => void;
  controller?: WorkspaceController;
}>;

const STATUS_LABELS: Readonly<Record<ToolCallEntity["status"], string>> = {
  pending: "等待中",
  in_progress: "执行中",
  completed: "完成",
  failed: "失败",
  blocked: "已阻止",
  cancelled: "已取消"
};

/** Picks the idle icon for a tool's ACP kind; unknown kinds keep the generic wrench. */
function KindIcon({ kind }: Readonly<{ kind: ToolKind | undefined }>) {
  if (kind === "read") return <FileText size={14} />;
  if (kind === "edit") return <FilePen size={14} />;
  if (kind === "execute") return <SquareTerminal size={14} />;
  if (kind === "search") return <Search size={14} />;
  return <Wrench size={14} />;
}

/** Renders ACP text as Markdown while preserving image, diff, and structured JSON output. */
function ToolOutput({ content, rawOutput, terminal }: Pick<ToolCallEntity, "content" | "rawOutput"> & Readonly<{ terminal?: TerminalEntity }>) {
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
      if (entry.type === "terminal") {
        return terminal !== undefined && entry.terminalId === terminal.terminalId ? <TerminalView key={index} terminal={terminal} /> : null;
      }
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
export const ToolCallCard = memo(function ToolCallCard({ tool, terminal, inspecting = false, onInspect, controller }: ToolCallCardProps) {
  // Bash cards first mount from the in-progress ACP update, so reveal streamed
  // output immediately while preserving the user's later expand/collapse choice.
  const childId = tool.subagentSessionId;
  const phase = childId === undefined ? "ordinary" : `${childId}:${tool.status}`;
  const [manual, setManual] = useState<Readonly<{ phase: string; open: boolean }>>();
  // Terminal output and subagents are what a running tool is worth watching.
  const open = manual?.phase === phase ? manual.open : (childId !== undefined || terminal !== undefined || tool.title === "bash") && tool.status === "in_progress";
  useEffect(() => {
    if (childId === undefined || controller === undefined) return;
    void controller.ensureSessionListed(childId).catch(() => {});
  }, [childId, controller]);
  const statusLabel = STATUS_LABELS[tool.status];
  const icon = tool.status === "completed" ? <Check size={14} /> : tool.status === "blocked" ? <Ban size={14} /> : tool.status === "failed" ? <CircleAlert size={14} /> : tool.status === "cancelled" ? <CircleSlash size={14} /> : tool.status === "in_progress" ? <LoaderCircle className="spin" size={14} /> : <KindIcon kind={tool.kind} />;
  const elapsed = tool.startedAtMs === undefined || tool.endedAtMs === undefined ? undefined : BigInt(tool.endedAtMs) - BigInt(tool.startedAtMs);
  return (
    <details className="tool-card" data-status={tool.status} data-kind={tool.kind} data-origin={tool.origin} open={open}>
      <summary onClick={(event) => {
        // Mount large tool payloads only on demand; collapsed tool cards are
        // common during replay and must not parse hidden Markdown or JSON.
        event.preventDefault();
        setManual({ phase, open: !open });
      }}>
        <span className="tool-card__icon">{icon}</span>
        <span className="tool-card__title">{tool.origin === "user" ? `$ ${tool.title}` : tool.title}</span>
        {tool.origin === "user" ? <span className="tool-card__origin">用户执行{tool.excludeFromContext === true ? " · 不进上下文" : ""}</span> : null}
        <span className="tool-card__status">{statusLabel}{elapsed === undefined ? "" : ` · ${elapsed} ms`}</span>
        <ChevronDown className="tool-card__chevron" size={14} aria-hidden="true" />
      </summary>
      {open ? <div className="tool-card__details">
        {childId === undefined || controller === undefined ? null : <SubagentPanel sessionId={childId} controller={controller} />}
        {onInspect === undefined ? null : <button className="tool-card__inspect" type="button" aria-label="在 Context Lens 中检查工具调用" aria-pressed={inspecting} onClick={() => onInspect(tool)}><ScanSearch size={13} aria-hidden="true" />Context Lens</button>}
        <div className="tool-card__lifecycle" aria-label="工具生命周期">
          <span><i data-phase="start" />Start{tool.startedAtMs === undefined ? " · 等待事件" : ` · ${tool.startedAtMs}`}</span>
          <span><i data-phase="end" data-complete={tool.endedAtMs !== undefined} />End{tool.endedAtMs === undefined ? " · 等待结果" : ` · ${tool.endedAtMs}`}</span>
        </div>
        {tool.rawInput === undefined ? null : <section><h4>输入</h4><JsonBlock value={tool.rawInput} /></section>}
        {tool.output === undefined ? null : <section aria-label="输出预算信息">
          <p>输出已限制：文本 {tool.output.visibleTextBytes.toLocaleString()} / {tool.output.originalTextBytes.toLocaleString()} 字节；省略媒体 {tool.output.omittedMedia} 块。</p>
          {tool.output.fullOutputPath === undefined ? null : <p>完整结果临时文件：<code>{tool.output.fullOutputPath}</code></p>}
          {tool.output.saveFailure === undefined ? null : <p>完整结果未保存：{tool.output.saveFailure === "too_large" ? "超过 64 MiB" : tool.output.saveFailure === "cancelled" ? "保存已取消" : "文件写入失败"}。</p>}
        </section>}
        {tool.content.length === 0 && tool.rawOutput === undefined ? null : <section><h4>输出</h4><ToolOutput content={tool.content} rawOutput={tool.rawOutput} {...(terminal === undefined ? {} : { terminal })} /></section>}
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
