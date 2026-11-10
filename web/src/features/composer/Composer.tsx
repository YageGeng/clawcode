import { ArrowUpToLine, ImagePlus, Link2, ListOrdered, Send, Square, X } from "lucide-react";
import { useRef, useState } from "react";

import { AcpRequestError } from "../../acp/connection";
import type { SessionId } from "../../acp/protocol";
import type { ImageMimeType } from "../../acp/protocol";
import type { AvailableCommandEntity, ContextUsage, PendingMessages, PromptImage, PromptInput, PromptResourceLink } from "../../domain/model";
import { ModelUsages } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";
import { CommandPalette } from "./CommandPalette";
import { CommandPaletteModel } from "./commandPaletteModel";
import { BackgroundTerminals } from "./BackgroundTerminals";
import { PendingQueue } from "./PendingQueue";
import { PlanPanel } from "./PlanPanel";
import { sessionWorkspace } from "../../workspace/state";
import { backgroundTerminalCount } from "../../workspace/selectors";
import { useWorkspaceStore } from "../../workspace/store";

export type ComposerProps = Readonly<{
  productSlug: string;
  sessionId: SessionId;
  running: boolean;
  contextUsage: ContextUsage | undefined;
  outcomeUnknown: boolean;
  pending: PendingMessages;
  availableCommands: readonly AvailableCommandEntity[];
  initialDraft?: PromptInput;
  controller: WorkspaceController;
}>;

type StoredDraft = Readonly<{ text: string; resources: readonly PromptResourceLink[] }>;

type ContextMeterProps = Readonly<{ usage: ContextUsage | undefined }>;

/** Computes a bounded percentage without losing precision for large token counts. */
function contextUsagePercent(usage: ContextUsage | undefined): number | undefined {
  if (usage?.used === undefined) return undefined;
  try {
    const used = BigInt(usage.used);
    const size = BigInt(usage.size);
    if (size <= 0n) return undefined;
    const perMille = (used * 1000n) / size;
    return Number(perMille > 1000n ? 1000n : perMille) / 10;
  } catch {
    return undefined;
  }
}

/** Renders the current context budget as a compact, keyboard-accessible ring. */
function ContextMeter({ usage }: ContextMeterProps) {
  const percentage = contextUsagePercent(usage);
  // Keep the ring visually compact while leaving a balanced touch target around it.
  const radius = 15.25;
  const circumference = 2 * Math.PI * radius;
  const progress = percentage === undefined ? 0 : percentage / 100;
  const tone = percentage !== undefined && percentage >= 85 ? "warning" : "normal";
  const percentLabel = percentage === undefined ? "--" : `${percentage.toFixed(1)}%`;
  const usedLabel = usage === undefined ? "未知" : ModelUsages.formatTokens(usage.used);
  const sizeLabel = usage === undefined ? "未知" : ModelUsages.formatTokens(usage.size);
  const exact = usage?.exact;
  const description = `Context ${usedLabel} / ${sizeLabel} tokens，使用率 ${percentLabel}`;

  return (
    <div className={`context-meter context-meter--${tone}`} tabIndex={0} role="img" aria-label={description}>
      <svg className="context-meter__ring" viewBox="0 0 40 40" aria-hidden="true">
        <circle className="context-meter__track" cx="20" cy="20" r={radius} />
        <circle
          className="context-meter__progress"
          cx="20"
          cy="20"
          r={radius}
          strokeDasharray={circumference}
          strokeDashoffset={circumference * (1 - progress)}
        />
      </svg>
      <div className="context-meter__tooltip" role="tooltip">
        <strong>Context</strong>
        <span>{usedLabel} / {sizeLabel} tokens</span>
        <span>使用率 {percentLabel}</span>
        {exact === undefined ? null : <small>
          输入 {ModelUsages.formatTokens(exact.inputTokens)} · 输出 {ModelUsages.formatTokens(exact.outputTokens)}<br />
          缓存读取 {ModelUsages.formatTokens(exact.cacheReadTokens)} · 缓存写入 {ModelUsages.formatTokens(exact.cacheWriteTokens)}
        </small>}
      </div>
    </div>
  );
}

const SUPPORTED_IMAGE_MIME_TYPES: readonly ImageMimeType[] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
const MAX_IMAGE_COUNT = 5;
const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
const MAX_TOTAL_IMAGE_BYTES = 20 * 1024 * 1024;

/** Returns a stable display name for an image supplied by the system clipboard. */
function clipboardImageName(mimeType: ImageMimeType): string {
  return `clipboard-image.${mimeType.slice("image/".length)}`;
}

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

/** Composes drafts with an explicit delivery choice while the current task is running. */
export function Composer({ productSlug, sessionId, running, contextUsage, outcomeUnknown, pending, availableCommands, initialDraft, controller }: ComposerProps) {
  const storageKey = `${productSlug}:agent-draft:${sessionId}`;
  const planMode = useWorkspaceStore((state) => sessionWorkspace(state, sessionId).configOptions.some((option) => option.configId === "mode" && option.currentValue === "plan"));
  const backgroundTerminals = useWorkspaceStore((state) => backgroundTerminalCount(sessionWorkspace(state, sessionId)));
  const stored = DraftCodec.parse(sessionStorage.getItem(storageKey));
  const initial = initialDraft ?? { ...stored, images: [] };
  const [text, setText] = useState(initial.text);
  const [resources, setResources] = useState(initial.resources);
  const [images, setImages] = useState<readonly PromptImage[]>(initial.images);
  const [resourceName, setResourceName] = useState("");
  const [resourceUri, setResourceUri] = useState("");
  const [showResourceForm, setShowResourceForm] = useState(false);
  const [readingImages, setReadingImages] = useState(false);
  const [error, setError] = useState<string>();
  const [hint, setHint] = useState<string>();
  const [paletteDismissed, setPaletteDismissed] = useState(false);
  const [selectedCommandIndex, setSelectedCommandIndex] = useState(0);
  const [steering, setSteering] = useState(controller.bootstrap.whenBusy === "steer");
  const imageInput = useRef<HTMLInputElement>(null);
  const imageReadInProgress = useRef(false);
  const imagesRef = useRef(initial.images);
  const commandQuery = text.startsWith("/") && !/\s/.test(text) ? text.slice(1) : undefined;
  const matchingCommands = commandQuery === undefined ? [] : CommandPaletteModel.matches(availableCommands, commandQuery);
  const paletteOpen = commandQuery !== undefined && !paletteDismissed;
  const activeCommandIndex = matchingCommands.length === 0 ? 0 : selectedCommandIndex % matchingCommands.length;
  const canSubmit = !readingImages && (text.trim().length > 0 || resources.length > 0 || images.length > 0);

  /** Validates and reads selected or pasted image files into the composer state. */
  const importImages = (files: readonly File[]) => {
    if (files.length === 0) return;
    if (imageReadInProgress.current) {
      setError("图片仍在读取，请稍候");
      return;
    }
    const currentImages = imagesRef.current;
    if (currentImages.length + files.length > MAX_IMAGE_COUNT) {
      setError(`每条消息最多选择 ${MAX_IMAGE_COUNT} 张图片`);
      return;
    }
    const unsupported = files.find((file) => !SUPPORTED_IMAGE_MIME_TYPES.includes(file.type as ImageMimeType));
    if (unsupported !== undefined) {
      setError(`不支持图片格式：${unsupported.name || unsupported.type}`);
      return;
    }
    const oversized = files.find((file) => file.size > MAX_IMAGE_BYTES);
    if (oversized !== undefined) {
      setError(`图片不能超过 10 MiB：${oversized.name || "剪贴板图片"}`);
      return;
    }
    if (currentImages.reduce((total, image) => total + image.size, 0) + files.reduce((total, file) => total + file.size, 0) > MAX_TOTAL_IMAGE_BYTES) {
      setError("图片总大小不能超过 20 MiB");
      return;
    }
    imageReadInProgress.current = true;
    setReadingImages(true);
    void Promise.all(files.map((file) => new Promise<PromptImage>((resolve, reject) => {
      const reader = new FileReader();
      const name = file.name || clipboardImageName(file.type as ImageMimeType);
      reader.addEventListener("error", () => reject(new Error(`无法读取图片：${name}`)), { once: true });
      reader.addEventListener("load", () => {
        if (typeof reader.result !== "string") {
          reject(new Error(`无法读取图片：${name}`));
          return;
        }
        const separator = reader.result.indexOf(",");
        if (separator < 0) {
          reject(new Error(`图片编码无效：${name}`));
          return;
        }
        resolve({ id: crypto.randomUUID(), name, size: file.size, data: reader.result.slice(separator + 1), mimeType: file.type as ImageMimeType });
      }, { once: true });
      reader.readAsDataURL(file);
    }))).then((selected) => {
      persist({ text, resources, images: [...imagesRef.current, ...selected] });
      setError(undefined);
    }).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason))).finally(() => {
      imageReadInProgress.current = false;
      setReadingImages(false);
    });
  };

  const persist = (draft: PromptInput) => {
    sessionStorage.setItem(storageKey, JSON.stringify({ text: draft.text, resources: draft.resources } satisfies StoredDraft));
    controller.updateDraft(sessionId, draft);
    imagesRef.current = draft.images;
    setText(draft.text);
    setResources(draft.resources);
    setImages(draft.images);
  };

  /** Clears only after synchronous validation; delayed outcomes belong to the controller. */
  const submit = (input: PromptInput, alternate = false) => {
    if (imageReadInProgress.current) { setError("图片仍在读取，请稍候"); return; }
    setError(undefined);
    setHint(undefined);
    try {
      // Keep the existing backend policy while making the selected mode and Alt+Enter agree.
      const useSteering = alternate ? !steering : steering;
      controller.submit(input, { alternate: running ? useSteering !== (controller.bootstrap.whenBusy === "steer") : alternate });
      persist({ text: "", resources: [], images: [] });
      sessionStorage.removeItem(storageKey);
      controller.clearDraft(sessionId);
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  /** Explains uncertain queue mutations without implying that their input was returned. */
  const queueError = (reason: unknown) => {
    const data = reason instanceof AcpRequestError ? reason.rpc.data : undefined;
    setError(typeof data === "object" && data !== null && "outcomeUnknown" in data && data.outcomeUnknown === true
      ? "会话持久化异常，请关闭并重新打开会话"
      : reason instanceof Error ? reason.message : String(reason));
  };

  /** Writes one discovered command into the draft without expanding it in the browser. */
  const selectCommand = (command: AvailableCommandEntity) => {
    persist({
      text: `/${command.name}${command.argumentHint === undefined ? "" : " "}`,
      resources,
      images
    });
    setSelectedCommandIndex(0);
    setPaletteDismissed(true);
  };

  return (
    <div className="composer-area">
      <PlanPanel sessionId={sessionId} />
      {pending.steering.length + pending.followUp.length === 0 ? null : <PendingQueue pending={pending} controller={controller} onError={queueError} onHint={setHint} />}
      {backgroundTerminals === 0 ? null : <BackgroundTerminals sessionId={sessionId} count={backgroundTerminals} controller={controller} onError={queueError} />}
      <div className="composer-card">
        {running ? <div className="composer-delivery">
          <div className="composer-delivery__options" role="group" aria-label="运行时发送方式">
            <button type="button" aria-pressed={!steering} onClick={() => setSteering(false)}><ListOrdered size={13} aria-hidden="true" />稍后发送</button>
            <button type="button" aria-pressed={steering} onClick={() => setSteering(true)}><ArrowUpToLine size={13} aria-hidden="true" />本轮补充</button>
          </div>
          <span>{steering ? "在下一次模型请求前补充" : "当前任务完成后依次发送"}</span>
        </div> : null}
        {resources.length === 0 ? null : <div className="resource-chips">{resources.map((resource) => <span className="resource-chip" key={`${resource.name}:${resource.uri}`}><Link2 size={12} />{resource.name}<button type="button" title="移除资源链接" onClick={() => persist({ text, resources: resources.filter((item) => item !== resource), images })}><X size={12} /></button></span>)}</div>}
        {images.length === 0 ? null : <div className="image-previews">{images.map((image) => <figure className="image-preview" key={image.id}><img src={`data:${image.mimeType};base64,${image.data}`} alt={image.name} /><figcaption title={image.name}>{image.name}</figcaption><button type="button" title={`移除 ${image.name}`} onClick={() => persist({ text, resources, images: images.filter((item) => item.id !== image.id) })}><X size={12} /></button></figure>)}</div>}
        {showResourceForm ? <div className="resource-form"><input aria-label="资源名称" placeholder="显示名称" value={resourceName} onChange={(event) => setResourceName(event.target.value)} /><input aria-label="资源 URI" placeholder="file:///path 或 https://…" value={resourceUri} onChange={(event) => setResourceUri(event.target.value)} /><button className="secondary-button" type="button" onClick={() => {
          const name = resourceName.trim();
          const uri = resourceUri.trim();
          try {
            if (name.length === 0) throw new Error("资源名称不能为空");
            void new URL(uri);
            persist({ text, resources: [...resources, { name, uri }], images });
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
          autoFocus={initialDraft !== undefined}
          aria-controls={paletteOpen ? "command-palette" : undefined}
          aria-expanded={paletteOpen}
          aria-activedescendant={paletteOpen && matchingCommands.length > 0 ? `command-option-${activeCommandIndex}` : undefined}
          placeholder={running ? steering ? "补充当前任务的要求…" : "写下下一条消息…" : "发送消息…"}
          rows={3}
          value={text}
          onChange={(event) => {
            persist({ text: event.target.value, resources, images });
            setSelectedCommandIndex(0);
            setPaletteDismissed(false);
          }}
          onPaste={(event) => {
            const files = [...event.clipboardData.items]
              .filter((item) => item.kind === "file" && SUPPORTED_IMAGE_MIME_TYPES.includes(item.type as ImageMimeType))
              .map((item) => item.getAsFile())
              .filter((file): file is File => file !== null);
            if (files.length === 0) return;
            // Preserve text when a clipboard payload contains both text and an image.
            if (event.clipboardData.getData("text/plain").length === 0) event.preventDefault();
            importImages(files);
          }}
          onKeyDown={(event) => {
            // IME confirmation and command selection take precedence over the
            // Composer's Enter-to-send shortcut to prevent accidental prompts.
            if (event.nativeEvent.isComposing) return;
            if (paletteOpen) {
              if (event.key === "Escape") {
                event.preventDefault();
                setPaletteDismissed(true);
                return;
              }
              if (matchingCommands.length > 0 && event.key === "ArrowDown") {
                event.preventDefault();
                setSelectedCommandIndex((activeCommandIndex + 1) % matchingCommands.length);
                return;
              }
              if (matchingCommands.length > 0 && event.key === "ArrowUp") {
                event.preventDefault();
                setSelectedCommandIndex((activeCommandIndex - 1 + matchingCommands.length) % matchingCommands.length);
                return;
              }
              if (matchingCommands.length > 0 && ((event.key === "Enter" && !event.shiftKey) || event.key === "Tab")) {
                event.preventDefault();
                const command = matchingCommands[activeCommandIndex];
                if (command !== undefined) selectCommand(command);
                return;
              }
            }
            if (event.key === "Enter" && !event.shiftKey) {
              event.preventDefault();
              if (canSubmit) submit({ text, resources, images }, event.altKey);
            }
          }}
        />
        {error === undefined ? null : <div className="form-error" role="alert">{error}</div>}
        {hint === undefined ? null : <div className="form-hint" role="status">{hint}</div>}
        {outcomeUnknown ? <div className="outcome-warning">请求结果未知，原始输入已保留，系统不会自动重发。<button type="button" disabled={readingImages} onClick={() => controller.restoreUnknownInput(sessionId)}>取回输入并检查</button></div> : null}
        <div className="composer-actions">
          <input ref={imageInput} className="visually-hidden" type="file" accept={SUPPORTED_IMAGE_MIME_TYPES.join(",")} multiple disabled={readingImages} onChange={(event) => {
            const files = [...(event.target.files ?? [])];
            event.target.value = "";
            importImages(files);
          }} />
          <button className="icon-button" type="button" title={readingImages ? "正在读取图片" : "添加图片"} disabled={readingImages} onClick={() => imageInput.current?.click()}><ImagePlus size={17} /></button>
          <button className="icon-button" type="button" title="添加资源链接" onClick={() => setShowResourceForm((value) => !value)}><Link2 size={17} /></button>
          {planMode ? <span className="mode-badge" title="Plan 模式：只能调研，不会修改文件">Plan · 只读</span> : null}
          <span>{readingImages ? "正在读取图片…" : running ? `Enter ${steering ? "补充" : "排队"} · Alt+Enter ${steering ? "排队" : "补充"} · Shift+Enter 换行` : "Enter 发送 · Shift+Enter 换行"}</span>
          {running ? <button className="danger-button" type="button" onClick={() => controller.cancel()}><Square size={13} /> 停止</button> : null}
          <ContextMeter usage={contextUsage} />
          <button aria-keyshortcuts="Enter" className="primary-button" type="button" disabled={!canSubmit} onClick={() => void submit({ text, resources, images })}><Send size={14} /> {running ? steering ? "补充本轮" : "加入队列" : "发送"}</button>
        </div>
      </div>
    </div>
  );
}
