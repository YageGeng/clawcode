import { cleanup, computed, effect, signal, Text, View } from "nsh";
import type { JSX } from "nsh/jsx-runtime";
import { t } from "nsh/i18n";

import type { SessionId } from "../client/acp/protocol";
import type { PromptDraft } from "../client/acp/prompt";
import { messageOf } from "../client/errors";
import type { HostStore } from "../stores/host";
import { IconButton } from "../ui/button";
import { SlashCommands } from "./SlashCommands";
import { Appear } from "../ui/appear";
import { glideNextLayout } from "../ui/motion";
import { Surface } from "../ui/surface";
import { type } from "../ui/tokens";
import { AttachmentStrip } from "./AttachmentStrip";
import { imageLimit, pickImages } from "./attachments";
import { PermissionCard } from "./PermissionCard";
import { SessionControls } from "./SessionControls";
import { SessionInteractions } from "./SessionInteractions";

/**
 * The composer: the floating card at the bottom of the conversation column, with the Session's pending approval
 * above it. Enter sends, Shift+Enter starts a line; the round button sends, or stops the running turn.
 */
export function Composer(props: {
  host: HostStore;
  /** The open Session; `null` on the empty state, where sending creates one. */
  sessionId: SessionId | null;
  /** Sends a snapshot; first-session navigation can check whether the editor still contains that submission. */
  onSend: (draft: PromptDraft, unchanged: () => boolean) => Promise<void>;
  /** Prepares a Session for configuration before its first prompt; the draft stays mounted. */
  onConfigureNew?: () => Promise<void>;
  /** The current workspace is waiting for its first Session to be prepared. */
  preparing?: boolean;
  /** Sending is off (no workspace to start a Session in yet). */
  disabled?: boolean;
  /** What sits above the card: the context strip of a new Session. */
  context?: JSX.Element;
}) {
  const draft = signal<PromptDraft>({ text: "", images: [] });
  const picking = signal(false);
  const sending = signal(false);
  const imageError = signal("");
  let disposed = false;
  cleanup(() => { disposed = true; });
  const running = () => props.sessionId !== null && props.host.running(props.sessionId);
  const canSend = () => props.disabled !== true && !props.preparing && !picking() && !sending() && !running() &&
    !(props.sessionId !== null && props.host.configPending(props.sessionId)) && (draft().text.trim() !== "" || draft().images.length > 0);
  // A value of its own: the Escape binding below follows it, and must not be bound again on every streamed token.
  const pending = computed(() => (props.sessionId === null ? undefined : props.host.permissionFor(props.sessionId)));
  // The round button grows in only when it swaps (send ↔ stop), not when the composer first shows.
  let swapped = false;
  setTimeout(() => (swapped = true), 0);

  // Keep a failed draft intact at the host while the user is composing a different one.
  effect(() => {
    const sessionId = props.sessionId;
    if (sessionId === null || !props.host.returnedDrafts().has(sessionId) || picking() || draft().text || draft().images.length) return;
    const returned = props.host.takeReturnedDraft(sessionId);
    if (returned) draft.set(returned);
  });

  /** A cancelled picker preserves the draft. A late pick belongs only to the host and Session that opened it. */
  const attach = async () => {
    if (picking() || running() || props.disabled || draft().images.length >= imageLimit) return;
    const host = props.host;
    const sessionId = props.sessionId;
    picking.set(true);
    imageError.set("");
    try {
      const added = await pickImages(draft().images);
      if (!disposed && props.host === host && props.sessionId === sessionId && added.length) {
        draft.update((previous) => ({ ...previous, images: [...previous.images, ...added] }));
      }
    } catch (error: unknown) {
      if (!disposed) imageError.set(messageOf(error));
    } finally {
      if (!disposed) picking.set(false);
    }
  };

  /** Keep a new Session's draft until creation succeeds; an open Session hands failed submissions back through its store. */
  const send = async () => {
    if (!canSend()) return;
    const submission = draft();
    const firstPrompt = props.onConfigureNew !== undefined;
    /** Every edit replaces the draft, including editing back to the same text. */
    const unchanged = () => !disposed && draft() === submission;
    sending.set(true);
    imageError.set("");
    if (!firstPrompt) draft.set({ text: "", images: [] });
    try {
      await props.onSend(submission, unchanged);
      if (firstPrompt && unchanged()) draft.set({ text: "", images: [] });
    } catch {
      console.warn("Could not start the session for the composer draft");
    } finally {
      if (!disposed) sending.set(false);
    }
  };

  /** Answers the pending approval; its card fades while the composer settles. */
  const answer = (key: string, optionId: string | undefined) => {
    glideNextLayout();
    props.host.answerPermission(key, optionId);
  };

  return (
    <View class="w-full max-w-[760px] gap-2 self-center px-6 pb-4">
      {props.context}
      <SessionInteractions host={props.host} sessionId={props.sessionId} />
      <If when={pending()}>
        {(prompt) => <PermissionCard prompt={prompt()} onAnswer={(optionId) => answer(prompt().key, optionId)} />}
      </If>
      <Surface variant="floating">
        <If when={draft().images.length > 0}>
          <AttachmentStrip images={draft().images} onRemove={(id) => {
            draft.update((previous) => ({ ...previous, images: previous.images.filter((image) => image.id !== id) }));
            imageError.set("");
          }} />
        </If>
        <If when={imageError()}>
          {(error) => <Text role="alert" class={`px-1 ${type.meta} text-destructive`}>{error()}</Text>}
        </If>
        <SlashCommands host={props.host} sessionId={props.sessionId} value={draft().text}
          disabled={props.disabled === true || sending() || picking() || running()} preparing={props.preparing === true} onPrepare={props.onConfigureNew}
          onChange={(text) => draft.update((previous) => ({ ...previous, text }))} onSubmit={send}
          onEscape={pending() ? () => { const request = pending(); if (request) answer(request.key, undefined); } : undefined} />
        <SessionControls host={props.host} sessionId={props.sessionId} disabled={props.disabled === true} turnBusy={sending() || running()}
          picking={picking()} atImageLimit={draft().images.length >= imageLimit} onAttach={() => void attach()}
          preparing={props.preparing === true} onConfigureNew={props.onConfigureNew}>
          <If
            when={running()}
            else={
              <Appear entrance="grow" play={swapped}>
                <IconButton icon="arrow-up" label={t("composer.send")} variant="primary" round disabled={!canSend()} onPress={send} />
              </Appear>
            }
          >
            <Appear entrance="grow" play={swapped}>
              <IconButton
                icon="square"
                label={t("composer.stop")}
                variant="primary"
                round
                onPress={() => props.sessionId !== null && props.host.cancel(props.sessionId)}
              />
            </Appear>
          </If>
        </SessionControls>
      </Surface>
    </View>
  );
}
