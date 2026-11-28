import { signal, Text, View } from "nsh";
import { t } from "nsh/i18n";

import type { RemoteStore } from "../stores/remote";
import { Button } from "../ui/button";
import { Dialog } from "../ui/dialog";
import { Field, Input } from "../ui/input";
import { type } from "../ui/tokens";

/** The main window's remote dialog: adding a remote computer, or none. Sharing settings live in the settings window. */
export type RemotePage = "pair" | null;

/** One modal surface for pairing requests from other devices (they take priority) and adding a remote computer. */
export function RemoteDialogs(props: {
  remote: RemoteStore;
  page: RemotePage;
  localReady: boolean;
  onNavigate: (page: RemotePage) => void;
  onPaired: (id: string) => void;
}) {
  const code = signal("");
  const request = () => props.remote.snapshot?.pending[0];
  const mode = () => request() ? "approve" : props.page;
  const title = () => mode() === "approve" ? t("remote.approveTitle", { name: request()?.name ?? "" }) : t("remote.addHost");
  /** Closing a confirmation rejects only that request; closing pairing cancels it. */
  const close = () => {
    const pending = request();
    if (pending) {
      void props.remote.approve(pending.requestId, false);
    } else {
      props.remote.cancelPair();
      code.set("");
      props.onNavigate(null);
    }
  };
  /** Keeps one copy of the code only until the request has been submitted. */
  const pair = async () => {
    const value = code().trim();
    if (!value || props.remote.busy("pair")) return;
    code.set("");
    const hostId = await props.remote.pair(value);
    if (hostId && props.page === "pair") props.onPaired(hostId);
  };
  return (
    <Dialog controlledDismissal open={mode() !== null} title={title()} onClose={close}>
      <View class="gap-3">
        <If when={mode() === "pair"}>
          <Field label={t("remote.code")} description={props.remote.busy("pair") ? t("remote.waiting") : t("remote.enterCode")}>
            <Input value={code()} accessibilityLabel={t("remote.code")} disabled={props.remote.busy("pair")}
              onChange={(value) => code.set(value)} onSubmit={() => void pair()} />
          </Field>
          <Button class="self-end" variant="primary" label={t("remote.pair")} disabled={!code().trim() || !props.remote.snapshot}
            loading={props.remote.busy("pair")} onPress={() => void pair()} />
        </If>
        <If when={request()}>
          {(pending) => (
            <View class="gap-3">
              <Text class={`${type.ui} text-foreground`}>{t("remote.approveDetails")}</Text>
              <Text class={`${type.meta} text-muted-foreground`}>{t("remote.deviceKey", { key: pending().id.slice(0, 12) })}</Text>
              <View class="flex-row justify-end gap-2">
                <Button label={t("remote.deny")} disabled={props.remote.busy("approve")} onPress={() => void props.remote.approve(pending().requestId, false)} />
                <Button label={t("remote.allow")} variant="primary" disabled={props.remote.busy("approve")} onPress={() => void props.remote.approve(pending().requestId, true)} />
              </View>
            </View>
          )}
        </If>
        <If when={props.remote.error}>
          {(reason) => <Text role="alert" class={`${type.meta} text-destructive`}>{reason()}</Text>}
        </If>
      </View>
    </Dialog>
  );
}
