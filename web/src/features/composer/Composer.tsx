import { ImagePlus, Link2, Send, Square, Trash2, X } from "lucide-react";
import { useRef, useState } from "react";

import type { SessionId } from "../../acp/protocol";
import type { ImageMimeType } from "../../acp/protocol";
import type { AvailableCommandEntity, PendingMessages, PromptImage, PromptInput, PromptResourceLink, QueuedMessage } from "../../domain/model";
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

const SUPPORTED_IMAGE_MIME_TYPES: readonly ImageMimeType[] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
const MAX_IMAGE_COUNT = 5;
const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
const MAX_TOTAL_IMAGE_BYTES = 20 * 1024 * 1024;

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
    const text = blocks.map((block) => {
      if (typeof block !== "object" || block === null || Array.isArray(block)) return "";
      const text = (block as Record<string, unknown>).text;
      return typeof text === "string" ? text : "";
    }).join("");
    const imageCount = blocks.filter((block) => typeof block === "object" && block !== null && !Array.isArray(block) && (block as Record<string, unknown>).type === "image").length;
    return [text, imageCount === 0 ? "" : `${imageCount} 张图片`].filter((part) => part.length > 0).join(" · ") || item.queueId;
  }
} as const;

export function Composer({ productSlug, sessionId, running, outcomeUnknown, pending, availableCommands, controller }: ComposerProps) {
  const storageKey = `${productSlug}:agent-draft:${sessionId}`;
  const initial = DraftCodec.parse(sessionStorage.getItem(storageKey));
  const [text, setText] = useState(initial.text);
  const [resources, setResources] = useState(initial.resources);
  const [images, setImages] = useState<readonly PromptImage[]>([]);
  const [resourceName, setResourceName] = useState("");
  const [resourceUri, setResourceUri] = useState("");
  const [showResourceForm, setShowResourceForm] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [readingImages, setReadingImages] = useState(false);
  const [error, setError] = useState<string>();
  const [paletteDismissed, setPaletteDismissed] = useState(false);
  const [selectedCommandIndex, setSelectedCommandIndex] = useState(0);
  const imageInput = useRef<HTMLInputElement>(null);
  const imageReadInProgress = useRef(false);
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
    if (imageReadInProgress.current) {
      setError("图片仍在读取，请稍候");
      return;
    }
    setSubmitting(true);
    setError(undefined);
    try {
      await controller.send(input);
      persist({ text: "", resources: [] });
      setImages([]);
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
        {images.length === 0 ? null : <div className="image-previews">{images.map((image) => <figure className="image-preview" key={image.id}><img src={`data:${image.mimeType};base64,${image.data}`} alt={image.name} /><figcaption title={image.name}>{image.name}</figcaption><button type="button" title={`移除 ${image.name}`} onClick={() => setImages((current) => current.filter((item) => item.id !== image.id))}><X size={12} /></button></figure>)}</div>}
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
              if (!submitting && !readingImages) void submit({ text, resources, images });
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
        {outcomeUnknown ? <div className="outcome-warning">上次请求结果未知，系统不会自动重发。<button type="button" disabled={readingImages} onClick={() => void submit({ text, resources, images })}>确认后重新发送</button></div> : null}
        <div className="composer-actions">
          <input ref={imageInput} className="visually-hidden" type="file" accept={SUPPORTED_IMAGE_MIME_TYPES.join(",")} multiple disabled={readingImages} onChange={(event) => {
            const files = [...(event.target.files ?? [])];
            event.target.value = "";
            if (files.length === 0) return;
            if (imageReadInProgress.current) {
              setError("图片仍在读取，请稍候");
              return;
            }
            if (images.length + files.length > MAX_IMAGE_COUNT) {
              setError(`每条消息最多选择 ${MAX_IMAGE_COUNT} 张图片`);
              return;
            }
            const unsupported = files.find((file) => !SUPPORTED_IMAGE_MIME_TYPES.includes(file.type as ImageMimeType));
            if (unsupported !== undefined) {
              setError(`不支持图片格式：${unsupported.name}`);
              return;
            }
            const oversized = files.find((file) => file.size > MAX_IMAGE_BYTES);
            if (oversized !== undefined) {
              setError(`图片不能超过 10 MiB：${oversized.name}`);
              return;
            }
            if (images.reduce((total, image) => total + image.size, 0) + files.reduce((total, file) => total + file.size, 0) > MAX_TOTAL_IMAGE_BYTES) {
              setError("图片总大小不能超过 20 MiB");
              return;
            }
            imageReadInProgress.current = true;
            setReadingImages(true);
            void Promise.all(files.map((file) => new Promise<PromptImage>((resolve, reject) => {
              const reader = new FileReader();
              reader.addEventListener("error", () => reject(new Error(`无法读取图片：${file.name}`)), { once: true });
              reader.addEventListener("load", () => {
                if (typeof reader.result !== "string") {
                  reject(new Error(`无法读取图片：${file.name}`));
                  return;
                }
                const separator = reader.result.indexOf(",");
                if (separator < 0) {
                  reject(new Error(`图片编码无效：${file.name}`));
                  return;
                }
                resolve({ id: crypto.randomUUID(), name: file.name, size: file.size, data: reader.result.slice(separator + 1), mimeType: file.type as ImageMimeType });
              }, { once: true });
              reader.readAsDataURL(file);
            }))).then((selected) => {
              setImages((current) => [...current, ...selected]);
              setError(undefined);
            }).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason))).finally(() => {
              imageReadInProgress.current = false;
              setReadingImages(false);
            });
          }} />
          <button className="icon-button" type="button" title={readingImages ? "正在读取图片" : "添加图片"} disabled={readingImages} onClick={() => imageInput.current?.click()}><ImagePlus size={17} /></button>
          <button className="icon-button" type="button" title="添加资源链接" onClick={() => setShowResourceForm((value) => !value)}><Link2 size={17} /></button>
          <span>{readingImages ? "正在读取图片…" : "Ctrl/⌘ + Enter 发送"}</span>
          {running ? <button className="danger-button" type="button" onClick={() => controller.cancel()}><Square size={13} /> 停止</button> : null}
          <button className="primary-button" type="button" disabled={submitting || readingImages || (text.trim().length === 0 && resources.length === 0 && images.length === 0)} onClick={() => void submit({ text, resources, images })}><Send size={14} /> {running ? "加入队列" : "发送"}</button>
        </div>
      </div>
    </div>
  );
}
