import { cleanup, computed, effect, signal, Text, View } from "nsh";
import { t } from "nsh/i18n";

import type { SessionId } from "../client/acp/protocol";
import type { PromptDraft } from "../client/acp/prompt";
import { Composer } from "../composer/Composer";
import { ContextStrip } from "../composer/ContextStrip";
import type { AppStore } from "../stores/app";
import { SessionPreparationError } from "../stores/host";
import type { HostStore } from "../stores/host";
import { Appear } from "../ui/appear";
import { Icon } from "../ui/icon";
import { type } from "../ui/tokens";
import { WorkspacePicker } from "./WorkspacePicker";
import { groupByWorkspace } from "./workspaces";

/** No Session open: what to build, in which workspace, and the composer that starts it. */
export function EmptyState(props: {
  store: AppStore;
  cwd: string | null;
  onCwdChange: (cwd: string) => void;
  onAddHost?: () => void;
  onStartError: (reason: unknown) => void;
}) {
  const host = computed(() => props.store.currentHost());
  const workspace = computed(() => props.cwd);
  const workspaces = () => groupByWorkspace(host().sessions()).map((group) => group.cwd);
  const prepared = signal<{ host: HostStore; cwd: string; id: SessionId; needsAttach?: boolean } | null>(null);
  const pending = signal<(() => Promise<void>) | null>(null);
  let disposed = false;
  cleanup(() => { disposed = true; });
  /** Read the confirmed id only within its original host and workspace. */
  const saved = () => {
    const record = prepared();
    return record && record.host.info.id === host().info.id && record.cwd === props.cwd ? record : null;
  };
  /** Only an attached Session can advertise usable commands, settings and interactions. */
  const sessionId = () => {
    const record = saved();
    return record?.host === host() && !record.needsAttach ? record.id : null;
  };
  // Each workspace visit owns its creation and any confirmed id that still needs to be reattached.
  const sessionForDraft = computed<() => Promise<SessionId>>(() => {
    const target = host();
    const cwd = workspace();
    /** Acquire the original Session for configuration or the first prompt, preserving its id when setup fails. */
    return async function acquire() {
      if (cwd === null) throw new Error("A workspace is required to start a session");
      const record = saved();
      try {
        if (!record) return await target.newSession(cwd);
        if (record.host !== target || record.needsAttach) {
          // Keep the confirmed id after a failed retry so the next explicit action can try this attachment again.
          const retry = { ...record, host: target, needsAttach: true };
          prepared.set(retry);
          await target.openSession(record.id);
          if (!disposed && prepared() === retry && host() === target) prepared.set({ ...retry, needsAttach: false });
        }
        return record.id;
      } catch (error) {
        if (!disposed && sessionForDraft() === acquire) {
          if (error instanceof SessionPreparationError) prepared.set({ host: target, cwd, id: error.sessionId, needsAttach: true });
          props.onStartError(error);
        }
        throw error;
      }
    };
  });
  // A retained slash draft can configure a new workspace while the previous workspace's request is pending.
  const configure = computed<() => Promise<void>>(() => {
    const acquire = sessionForDraft();
    const target = host();
    const cwd = workspace();
    /** Apply this creation, its error and its pending state only to the draft that requested them. */
    return async function prepare() {
      if (disposed || cwd === null || sessionId() !== null || pending() === prepare) return;
      pending.set(prepare);
      try {
        const id = await acquire();
        if (!disposed && configure() === prepare) prepared.set({ host: target, cwd, id });
      } finally {
        if (!disposed && pending() === prepare) pending.set(null);
      }
    };
  });
  // A replacement connection automatically rejoins once; a failed rejoin can be retried by any explicit draft action.
  effect(() => {
    const record = saved();
    if (record && record.host !== host() && host().phase().kind === "ready") {
      void configure()().catch(() => { /* The preparation error is visible beside this draft. */ });
    }
  });
  /** Send the draft using this workspace's confirmed configuration and surface any creation error. */
  const send = async (draft: PromptDraft, unchanged: () => boolean) => {
    const visit = sessionForDraft();
    await props.store.startSession(host().info.id, draft, {
      session: visit,
      canOpen: () => !disposed && sessionForDraft() === visit && unchanged(),
    });
    // A newer edit on this page starts its own Session; the prior prompt consumed the prepared one.
    if (!disposed && sessionForDraft() === visit) prepared.set(null);
  };
  return (
    <Appear entrance="fade" class="flex-1 items-center justify-center gap-6 px-6">
      <View class="size-12 items-center justify-center rounded-(--radius-lg) border border-border bg-muted/60 shadow-2xs">
        <Icon name="terminal" size="md" color="foreground" />
      </View>
      {/* Full width so a narrow window wraps the heading instead of pushing it past the edges. */}
      <View class="w-full flex-row flex-wrap items-center justify-center gap-2">
        <Text class={`${type.heading} text-foreground`}>{t("empty.heading")}</Text>
        {/* The picker and the closing words stay together, so "?" never wraps onto a line alone. */}
        <View class="flex-row items-center gap-2">
          <WorkspacePicker remote={host().info.kind === "remote"} workspaces={workspaces()} value={props.cwd} onChange={props.onCwdChange} size="md" />
          <Text class={`${type.heading} text-foreground`}>{t("empty.headingAfter")}</Text>
        </View>
      </View>
      <Composer host={host()} sessionId={sessionId()} disabled={props.cwd === null || host().phase().kind !== "ready"}
        context={<ContextStrip store={props.store} onAddHost={props.onAddHost} />}
        preparing={pending() === configure()} onConfigureNew={configure()} onSend={send} />
    </Appear>
  );
}
