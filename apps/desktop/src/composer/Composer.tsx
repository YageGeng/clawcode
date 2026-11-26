import { effect, keymap, LayoutAnimation, reduceMotion, signal, View } from "nsh";
import type { JSX } from "nsh/jsx-runtime";
import { t } from "nsh/i18n";

import type { SessionId } from "../client/acp/protocol";
import type { HostStore } from "../stores/host";
import { IconButton } from "../ui/button";
import { Textarea } from "../ui/input";
import { Tooltip } from "../ui/menu";
import { Appear } from "../ui/appear";
import { motion } from "../ui/motion";
import { Surface } from "../ui/surface";
import { PermissionCard } from "./PermissionCard";

/**
 * The composer: the floating card at the bottom of the conversation column, with the Session's pending approval
 * above it. Enter sends, Shift+Enter starts a line; the round button sends, or stops the running turn.
 */
export function Composer(props: {
  host: HostStore;
  /** The open Session; `null` on the empty state, where sending creates one. */
  sessionId: SessionId | null;
  /** Sends the text; a rejection puts it back in the field (a failed prompt comes back through the host instead). */
  onSend: (text: string) => Promise<void>;
  /** Sending is off (no workspace to start a Session in yet). */
  disabled?: boolean;
  /** What sits above the card: the context strip of a new Session. */
  context?: JSX.Element;
}) {
  const draft = signal("");
  const running = () => props.sessionId !== null && props.host.workspace().sessionWorkspaces.get(props.sessionId)?.running === true;
  const canSend = () => props.disabled !== true && !running() && draft().trim() !== "";
  const pending = () => props.host.workspace().permissions.find((prompt) => prompt.sessionId === props.sessionId);
  // The round button grows in only when it swaps (send ↔ stop), not when the composer first shows.
  let swapped = false;
  setTimeout(() => (swapped = true), 0);

  // A failed prompt hands its text back: put it in the field unless the user has typed something new since.
  effect(() => {
    const sessionId = props.sessionId;
    if (sessionId === null || !props.host.returnedText().has(sessionId)) return;
    const text = props.host.takeReturnedText(sessionId);
    if (text !== undefined && draft() === "") draft.set(text);
  });

  /** Sends the draft when it may be sent, clearing the field at once. */
  const send = () => {
    if (!canSend()) return;
    const text = draft();
    draft.set("");
    props.onSend(text).catch((error: unknown) => {
      console.warn(`send: ${error instanceof Error ? error.message : String(error)}`);
      if (draft() === "") draft.set(text);
    });
  };

  /** Answers the pending approval; its card fades while the composer settles. */
  const answer = (key: string, optionId: string | undefined) => {
    if (!reduceMotion()) LayoutAnimation.configureNext(LayoutAnimation.create(motion.base, "easeInEaseOut", "opacity"));
    props.host.answerPermission(key, optionId);
  };

  // Escape in the field rejects a pending approval ("always": the field would take it). Bound only while the field
  // has the focus: a dialog or menu opened over it takes the focus, and its Escape must close it, not answer.
  const fieldFocused = signal(false);
  effect(() => {
    const prompt = pending();
    if (!fieldFocused() || prompt === undefined) return;
    return keymap.bind("escape", () => answer(prompt.key, undefined), { when: "always" });
  });

  return (
    <View class="w-full max-w-[760px] gap-2 self-center px-6 pb-4">
      {props.context}
      <If when={pending()}>
        {(prompt) => <PermissionCard prompt={prompt()} onAnswer={(optionId) => answer(prompt().key, optionId)} />}
      </If>
      <Surface variant="floating">
        <Textarea
          variant="bare"
          value={draft()}
          placeholder={t("composer.placeholder")}
          accessibilityLabel={t("composer.label")}
          onChange={(value) => draft.set(value)}
          onSubmit={send}
          onFocus={() => fieldFocused.set(true)}
          onBlur={() => fieldFocused.set(false)}
        />
        <View class="flex-row items-center justify-between">
          <IconButton icon="plus" label={t("composer.attach")} disabled tooltip={<Tooltip text={t("composer.attachSoon")} />} />
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
        </View>
      </Surface>
    </View>
  );
}
