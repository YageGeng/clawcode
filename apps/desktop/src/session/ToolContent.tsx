import { computed, Image, Text, View } from "nsh";
import { t } from "nsh/i18n";

import { AcpProtocol } from "../client/acp/protocol";
import type { ToolCallEntity } from "../client/domain/model";
import { Disclosure } from "../ui/disclosure";
import { type } from "../ui/tokens";
import type { TurnSource } from "./TurnRow";

type DisplayBlock = Readonly<{ title?: string; text?: string; image?: string; terminalId?: string }>;

/** Counts decoded bytes without allocating a second copy of a base64 attachment. */
function byteCount(data: string): number {
  return Math.max(0, Math.floor(data.length * 3 / 4) - (data.endsWith("==") ? 2 : data.endsWith("=") ? 1 : 0));
}

/** Preserves ordinary JSON, replacing media bytes and data URLs with readable size metadata. */
function displayValue(value: unknown): string {
  // Both the ACP blocks and the persisted raw result can contain an encoded copy of the same image.
  /** Substitutes byte counts for binary fields while leaving other string values intact. */
  function printable(this: Record<string, unknown>, key: string, item: unknown): unknown {
    if (typeof item !== "string") return item;
    const url = /^data:([^,]*?);base64,/i.exec(item);
    if (url) return [url[1], t("toolDetail.bytes", { count: byteCount(item.slice(url[0].length)) })].filter(Boolean).join(" · ");
    if (key === "blob" || key === "base64" || (key === "data" &&
      (this.type === "image" || this.type === "audio" || this.mimeType || this.mime_type ||
        (item.length > 256 && /^[a-z\d+/=\s]+$/i.test(item))))) {
      return t("toolDetail.bytes", { count: byteCount(item) });
    }
    return item;
  }
  return typeof value === "string" ? String(printable.call({}, "", value)) : JSON.stringify(value, printable, 2) ?? "";
}

/** Converts ACP blocks to visible sections without ever treating arbitrary extension data as markup. */
function displayBlocks(value: unknown): DisplayBlock[] {
  const wrapper = AcpProtocol.record(value);
  const block = wrapper?.type === "content" ? AcpProtocol.record(wrapper.content) : wrapper;
  if (!block) return [{ text: displayValue(value) }];
  switch (block.type) {
    case "text":
      if (typeof block.text === "string") return [{ text: displayValue(block.text) }];
      break;
    case "terminal":
      if (typeof block.terminalId === "string") return [{ terminalId: block.terminalId }];
      break;
    case "diff": {
      const patch = AcpProtocol.record(block.patch);
      if (typeof patch?.text === "string") return [{ text: patch.text }];
      if (typeof block.newText === "string") {
        // Older sessions describe both file versions instead of providing a unified patch.
        const label = typeof block.path === "string" ? block.path : "";
        return [
          ...(typeof block.oldText === "string" ? [{ title: [label, t("toolDetail.before")].filter(Boolean).join(" · "), text: block.oldText }] : []),
          { title: [label, t("toolDetail.after")].filter(Boolean).join(" · "), text: block.newText },
        ];
      }
      if (Array.isArray(block.changes)) return [{ text: displayValue(block.changes) }];
      break;
    }
    case "resource":
    case "resource_link":
    case "audio":
    case "image": {
      const resource = AcpProtocol.record(block.resource) ?? block;
      const mime = typeof resource.mimeType === "string" ? resource.mimeType : undefined;
      const data = resource.blob ?? resource.data;
      const name = resource.title ?? resource.name;
      const caption = [
        typeof name === "string" ? name : t(`toolDetail.${block.type}`),
        mime,
        typeof data === "string" ? t("toolDetail.bytes", { count: byteCount(data) })
          : typeof resource.size === "number" ? t("toolDetail.bytes", { count: resource.size }) : undefined,
      ].filter(Boolean).join(" · ");
      const detail = [resource.uri, resource.description, resource.text].filter((item): item is string => typeof item === "string");
      return [{
        title: caption,
        ...(detail.length ? { text: detail.map(displayValue).join("\n") } : {}),
        ...(typeof data === "string" && mime && /^image\/(png|jpeg|gif|webp)$/i.test(mime)
          ? { image: `data:${mime};base64,${data}` } : {}),
      }];
    }
    default:
      // The extension namespace is negotiated. Other block kinds retain their complete JSON shape.
      if (typeof block.type === "string" && block.type.endsWith("/structured_content") && "value" in block) {
        return [{ text: displayValue(block.value) }];
      }
  }
  return [{ text: displayValue(block) }];
}

/** A selectable, literal code or output section that preserves whitespace from the host. */
function OutputText(props: { text: string }) {
  return (
    <View selectable class="min-w-0 rounded-(--radius) bg-muted px-3 py-2">
      <Text class={`font-mono ${type.meta} text-foreground`}>{props.text || t("toolDetail.emptyValue")}</Text>
    </View>
  );
}

/** Strips terminal controls and keeps the last carriage-return redraw of each output line. */
function terminalText(output: string): string {
  // Strip complete CSI/OSC sequences after assembling chunks so split color controls never become text.
  return output.replace(/\u001b\[[0-?]*[ -/]*[@-~]|\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)|\u001b[@-Z\\-_]/g, "")
    .split("\n").map((line) => line.split("\r").filter(Boolean).at(-1) ?? "").join("\n");
}

/** Follows a referenced ACP terminal even when its first frame arrives after the tool call. */
function TerminalContent(props: { id: string; lookup?: TurnSource["terminal"] }) {
  const terminal = computed(() => props.lookup?.(props.id));
  const exit = () => terminal()?.exitStatus;
  return (
    <View class="gap-1">
      <Text class={`${type.meta} text-muted-foreground`}>{terminal()?.command ?? t("toolDetail.terminal", { id: props.id })}</Text>
      <If when={terminal()?.cwd}>{(cwd) => <Text class={`${type.meta} text-muted-foreground`}>{cwd()}</Text>}</If>
      <OutputText text={terminalText(terminal()?.output ?? "") || t("toolDetail.noOutput")} />
      <If when={exit()}>
        {(status) => <Text class={`${type.meta} text-muted-foreground`}>
          {status().signal ? t("toolDetail.signal", { signal: status().signal! })
            : status().exitCode !== undefined ? t("toolDetail.exitCode", { code: status().exitCode! }) : t("toolDetail.status.completed")}
        </Text>}
      </If>
    </View>
  );
}

/** Renders shared tool and message blocks in order. Only tool content resolves terminal references. */
function ContentList(props: { blocks: readonly unknown[]; terminal?: TurnSource["terminal"] }) {
  const blocks = computed(() => props.blocks.flatMap(displayBlocks));
  return (
    <View class="gap-2">
      <Each of={blocks()} key="index">
        {(block) => (
          <View class="gap-1">
            <If when={block().title}>{(title) => <Text class={`${type.meta} text-muted-foreground`}>{title()}</Text>}</If>
            <If when={block().image}>
              {(image) => <Image src={image()} accessibilityLabel={block().title ?? t("toolDetail.image")}
                fadeDuration={0} class="h-32 w-48 max-w-full rounded-(--radius) object-contain" />}
            </If>
            <If when={block().text !== undefined}><OutputText text={block().text ?? ""} /></If>
            <If when={block().terminalId}>
              {(id) => <TerminalContent id={id()} lookup={props.terminal} />}
            </If>
          </View>
        )}
      </Each>
    </View>
  );
}

/** Non-text assistant attachments, shared by the streaming transcript and the final reply. */
export function AssistantContent(props: { blocks: readonly Readonly<Record<string, unknown>>[] }) {
  return <ContentList blocks={props.blocks} />;
}

/** Shows a call's arguments, native ACP result blocks and any additional raw result on demand. */
export function ToolDetails(props: { tool: ToolCallEntity; source: TurnSource }) {
  const rawKey = () => `tool-raw:${props.tool.toolCallId}`;
  const pending = () => props.tool.status === "pending" || props.tool.status === "in_progress";
  return (
    <View class="ml-2.5 gap-2 border-l border-border pl-4">
      <If when={props.tool.name && props.tool.title !== props.tool.name && props.tool.title !== props.tool.toolCallId}>
        <Text class={`${type.ui} text-foreground`}>{props.tool.title}</Text>
      </If>
      <If when={props.tool.rawInput !== undefined}>
        <Text class={`${type.meta} font-medium text-muted-foreground`}>{t("toolDetail.input")}</Text>
        <OutputText text={displayValue(props.tool.rawInput)} />
      </If>
      <Text class={`${type.meta} font-medium text-muted-foreground`}>{t("toolDetail.result")}</Text>
      <If when={!!props.tool.content?.length} else={
        <If when={props.tool.rawOutput !== undefined} else={
          <Text class={`${type.meta} text-muted-foreground`}>{t(pending() ? "toolDetail.waiting" : "toolDetail.noOutput")}</Text>
        }>
          <OutputText text={displayValue(props.tool.rawOutput)} />
        </If>
      }>
        <ContentList blocks={props.tool.content ?? []} terminal={props.source.terminal} />
        <If when={props.tool.rawOutput !== undefined}>
          <Disclosure label={t("toolDetail.rawResult")} open={props.source.isOpen(rawKey())} onToggle={() => props.source.toggle(rawKey())}>
            <OutputText text={displayValue(props.tool.rawOutput)} />
          </Disclosure>
        </If>
      </If>
    </View>
  );
}
