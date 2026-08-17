import { Link2, Send, Square, Trash2, X } from "lucide-react";
import { useState } from "react";

import type { SessionId } from "../../acp/protocol";
import type { AvailableCommandEntity, PendingMessages, PromptInput, PromptResourceLink, QueuedMessage } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";
import { CommandPalette } from "./CommandPalette";
import { CommandPaletteModel } from "./commandPaletteModel";

export type ComposerProps = Readonly<{
  productSlug: string;
  sessionId: SessionId;
  running: boolean;
  outcomeUnknown: boolean;
  pending: PendingMessages;
  availableCommands: readonly AvailableCommandEntity[];
  controller: WorkspaceController;
}>;

type StoredDraft = Readonly<{ text: string; resources: readonly PromptResourceLink[] }>;

const DraftCodec = {
  parse(raw: string | null): StoredDraft {
    if (raw === null) return { text: "", resources: [] };
    try {
      const value: unknown = JSON.parse(raw);
      if (typeof value !== "object" || value === null || Array.isArray(value)) return { text: "", resources: [] };
      const record = value as Record<string, unknown>;
      const resources = Array.isArray(record.resources) ? record.resources.filter((item): item is PromptResourceLink => {
        if (typeof item !== "object" || item === null || Array.isArray(item)) return false;
        const resource = item as Record<string, unknown>;
        return typeof resource.name === "string" && typeof resource.uri === "string";
      }) : [];
      return { text: typeof record.text === "string" ? record.text : "", resources };
    } catch {
      return { text: "", resources: [] };
    }
  }
} as const;

const QueuedMessageView = {
  text(item: QueuedMessage): string {
    const content = item.message.content;
    if (typeof content !== "object" || content === null || Array.isArray(content)) return item.queueId;
    const contentRecord = content as Record<string, unknown>;
    const expansion = typeof contentRecord.expansion === "object" && contentRecord.expansion !== null && !Array.isArray(contentRecord.expansion)
      ? contentRecord.expansion as Record<string, unknown>
      : undefined;
    const invocation = typeof expansion?.invocation === "object" && expansion.invocation !== null && !Array.isArray(expansion.invocation)
      ? expansion.invocation as Record<string, unknown>
      : undefined;
    if (typeof invocation?.original === "string") return invocation.original;
    const blocks = contentRecord.blocks;
    if (!Array.isArray(blocks)) return item.queueId;
    return blocks.map((block) => {
      if (typeof block !== "object" || block === null || Array.isArray(block)) return "";
      const text = (block as Record<string, unknown>).text;
      return typeof text === "string" ? text : "";
    }).join("") || item.queueId;
  }
} as const;

export function Composer({ productSlug, sessionId, running, outcomeUnknown, pending, availableCommands, controller }: ComposerProps) {
  const storageKey = `${productSlug}:agent-draft:${sessionId}`;
  const initial = DraftCodec.parse(sessionStorage.getItem(storageKey));
  const [text, setText] = useState(initial.text);
  const [resources, setResources] = useState(initial.resources);
  const [resourceName, setResourceName] = useState("");
  const [resourceUri, setResourceUri] = useState("");
  const [showResourceForm, setShowResourceForm] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string>();
  const [paletteDismissed, setPaletteDismissed] = useState(false);
  const [selectedCommandIndex, setSelectedCommandIndex] = useState(0);
  const queued = [...pending.steering, ...pending.followUp];
  const commandQuery = text.startsWith("/") && !/\s/.test(text) ? text.slice(1) : undefined;
  const matchingCommands = commandQuery === undefined ? [] : CommandPaletteModel.matches(availableCommands, commandQuery);
  const paletteOpen = commandQuery !== undefined && !paletteDismissed;
  const activeCommandIndex = matchingCommands.length === 0 ? 0 : selectedCommandIndex % matchingCommands.length;

  const persist = (draft: StoredDraft) => {
    sessionStorage.setItem(storageKey, JSON.stringify(draft));
    setText(draft.text);
    setResources(draft.resources);
  };

  const submit = async (input: PromptInput) => {
    setSubmitting(true);
    setError(undefined);
    try {
      await controller.send(input);
      persist({ text: "", resources: [] });
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setSubmitting(false);
    }
  };

  /** Writes one discovered command into the draft without expanding it in the browser. */
  const selectCommand = (command: AvailableCommandEntity) => {
    persist({
      text: `/${command.name}${command.argumentHint === undefined ? "" : " "}`,
      resources
    });
    setSelectedCommandIndex(0);
    setPaletteDismissed(true);
  };

  return (
    <div className="composer-area">
      {queued.length === 0 ? null : (
        <section className="pending-queue" aria-label="待处理消息">
          <div className="pending-queue__heading"><span>{queued.length} 条 follow-up 等待执行</span><button type="button" onClick={() => void controller.clearPending().catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)))}><Trash2 size={13} /> 清空</button></div>
          {queued.map((item) => <div className="pending-item" key={item.queueId}><span>{QueuedMessageView.text(item)}</span><button type="button" title="移除" onClick={() => void controller.removePending(item.queueId).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)))}><X size={13} /></button></div>)}
        </section>
      )}
      <div className="composer-card">
        {resources.length === 0 ? null : <div className="resource-chips">{resources.map((resource) => <span className="resource-chip" key={`${resource.name}:${resource.uri}`}><Link2 size={12} />{resource.name}<button type="button" title="移除资源链接" onClick={() => persist({ text, resources: resources.filter((item) => item !== resource) })}><X size={12} /></button></span>)}</div>}
        {showResourceForm ? <div className="resource-form"><input aria-label="资源名称" placeholder="显示名称" value={resourceName} onChange={(event) => setResourceName(event.target.value)} /><input aria-label="资源 URI" placeholder="file:///path 或 https://…" value={resourceUri} onChange={(event) => setResourceUri(event.target.value)} /><button className="secondary-button" type="button" onClick={() => {
          const name = resourceName.trim();
          const uri = resourceUri.trim();
          try {
            if (name.length === 0) throw new Error("资源名称不能为空");
            void new URL(uri);
            persist({ text, resources: [...resources, { name, uri }] });
            setResourceName(""); setResourceUri(""); setShowResourceForm(false); setError(undefined);
          } catch (reason: unknown) {
            setError(reason instanceof Error ? reason.message : String(reason));
          }
        }}>添加</button></div> : null}
        {paletteOpen && commandQuery !== undefined ? (
          <CommandPalette
            commands={availableCommands}
            query={commandQuery}
            selectedIndex={activeCommandIndex}
            onSelect={selectCommand}
          />
        ) : null}
        <textarea
          aria-label="消息"
          aria-controls={paletteOpen ? "command-palette" : undefined}
          aria-expanded={paletteOpen}
          aria-activedescendant={paletteOpen && matchingCommands.length > 0 ? `command-option-${activeCommandIndex}` : undefined}
          placeholder={running ? "当前 Turn 运行中；发送内容将加入 follow-up…" : "发送消息…"}
          rows={3}
          value={text}
          onChange={(event) => {
            persist({ text: event.target.value, resources });
            setSelectedCommandIndex(0);
            setPaletteDismissed(false);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
              if (!submitting) void submit({ text, resources });
              return;
            }
            if (!paletteOpen) return;
            if (event.key === "Escape") {
              event.preventDefault();
              setPaletteDismissed(true);
            } else if (matchingCommands.length > 0 && event.key === "ArrowDown") {
              event.preventDefault();
              setSelectedCommandIndex((activeCommandIndex + 1) % matchingCommands.length);
            } else if (matchingCommands.length > 0 && event.key === "ArrowUp") {
              event.preventDefault();
              setSelectedCommandIndex((activeCommandIndex - 1 + matchingCommands.length) % matchingCommands.length);
            } else if (matchingCommands.length > 0 && (event.key === "Enter" || event.key === "Tab")) {
              event.preventDefault();
              const command = matchingCommands[activeCommandIndex];
              if (command !== undefined) selectCommand(command);
            }
          }}
        />
        {error === undefined ? null : <div className="form-error" role="alert">{error}</div>}
        {outcomeUnknown ? <div className="outcome-warning">上次请求结果未知，系统不会自动重发。<button type="button" onClick={() => void submit({ text, resources })}>确认后重新发送</button></div> : null}
        <div className="composer-actions">
          <button className="icon-button" type="button" title="添加资源链接" onClick={() => setShowResourceForm((value) => !value)}><Link2 size={17} /></button>
          <span>Ctrl/⌘ + Enter 发送</span>
          {running ? <button className="danger-button" type="button" onClick={() => controller.cancel()}><Square size={13} /> 停止</button> : null}
          <button className="primary-button" type="button" disabled={submitting || (text.trim().length === 0 && resources.length === 0)} onClick={() => void submit({ text, resources })}><Send size={14} /> {running ? "加入队列" : "发送"}</button>
        </div>
      </div>
    </div>
  );
}
