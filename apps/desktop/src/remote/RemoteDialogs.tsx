import { computed, effect, Scroll, signal, Text, View } from "nsh";
import { t } from "nsh/i18n";

import type { RemoteStore } from "../stores/remote";
import { Button } from "../ui/button";
import { CopyButton } from "../ui/copy";
import { Dialog } from "../ui/dialog";
import { Field, Input } from "../ui/input";
import { QrCode } from "../ui/qr";
import { Switch } from "../ui/selection";
import { Separator } from "../ui/surface";
import { type } from "../ui/tokens";

export type RemotePage = "sharing" | "pair" | "invite" | null;

/** Local identity and incoming access settings. Trusted devices can be disconnected or removed independently. */
function Sharing(props: {
  remote: RemoteStore;
  localReady: boolean;
  onInvite: () => void;
  onRevoke: (peer: { id: string; name: string }) => void;
}) {
  const savedName = computed(() => props.remote.snapshot?.name ?? "");
  const name = signal(savedName());
  // Native changes to the other device lists must not overwrite the name while it is being edited.
  effect(() => name.set(savedName()));
  return (
    <View class="gap-4">
      <Field label={t("remote.deviceName")}>
        <View class="flex-row items-center gap-2">
          <Input class="flex-1" accessibilityLabel={t("remote.deviceName")} value={name()} onChange={(value) => name.set(value)} />
          <Button label={t("remote.save")} disabled={!props.remote.snapshot || !name().trim() || name().trim() === savedName()}
            loading={props.remote.busy("rename")} onPress={() => void props.remote.rename(name())} />
        </View>
      </Field>
      <View class="gap-1">
        <Switch label={t("remote.allowControl")} checked={props.remote.sharingRequested || props.remote.snapshot?.enabled === true}
          disabled={!props.remote.snapshot || props.remote.busy("sharing") || (!props.localReady && !props.remote.sharingRequested && !props.remote.snapshot.enabled)}
          onChange={(enabled) => void props.remote.setSharing(enabled)} />
        <If when={!props.localReady}>
          <Text class={`${type.meta} text-muted-foreground`}>{t("remote.localUnavailable")}</Text>
        </If>
      </View>
      <If when={props.remote.snapshot?.enabled}>
        <Button class="self-start" label={t("remote.addDevice")} icon="plus" variant="outline"
          loading={props.remote.busy("invite")} onPress={props.onInvite} />
      </If>
      <Separator />
      <Text class={`${type.ui} font-medium text-foreground`}>{t("remote.trustedDevices")}</Text>
      <If when={props.remote.snapshot?.trusted.length}
        else={<Text class={`${type.meta} text-muted-foreground`}>{t("remote.noDevices")}</Text>}>
        <Scroll class="max-h-40">
          <Each of={props.remote.snapshot?.trusted ?? []} key={(peer) => peer.id}>
            {(peer) => (
              <View class="flex-row items-center gap-2 py-1">
                <View class="flex-1">
                  <Text class={`${type.ui} text-foreground truncate`}>{peer().name}</Text>
                  <Text class={`${type.meta} text-muted-foreground`}>{peer().id.slice(0, 12)}</Text>
                </View>
                <If when={props.remote.snapshot?.online.some((device) => device.id === peer().id)}>
                  <Button label={t("remote.disconnect")} size="sm" variant="outline" disabled={props.remote.busy(peer().id)}
                    onPress={() => void props.remote.disconnect(peer().id)} />
                </If>
                <Button label={t("remote.revoke")} size="sm" variant="ghost" disabled={props.remote.busy(peer().id)}
                  onPress={() => props.onRevoke(peer())} />
              </View>
            )}
          </Each>
        </Scroll>
      </If>
    </View>
  );
}

/** One modal surface prioritizes incoming host confirmation, including while the local user views another host. */
export function RemoteDialogs(props: {
  remote: RemoteStore;
  page: RemotePage;
  localReady: boolean;
  onNavigate: (page: RemotePage) => void;
  onPaired: (id: string) => void;
}) {
  const code = signal("");
  const removing = signal<{ id: string; name: string } | null>(null);
  const request = () => props.remote.snapshot?.pending[0];
  const mode = () => request() ? "approve" : removing() ? "revoke" : props.page;
  const title = () => {
    const next = mode();
    if (next === "approve") return t("remote.approveTitle", { name: request()?.name ?? "" });
    if (next === "revoke") return t("remote.revokeTitle", { name: removing()?.name ?? "" });
    return t(next === "invite" ? "remote.addDevice" : next === "pair" ? "remote.addHost" : "remote.title");
  };
  /** Closing a confirmation rejects only that request; closing an invite returns to the device list. */
  const close = () => {
    const pending = request();
    if (pending) {
      void props.remote.approve(pending.requestId, false);
    } else if (removing()) {
      removing.set(null);
    } else {
      if (props.page === "pair") props.remote.cancelPair();
      code.set("");
      props.onNavigate(props.page === "invite" ? "sharing" : null);
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
  /** Replaces the current invitation, then shows its code and expiration together. */
  const showInvite = async () => {
    if (await props.remote.createInvitation() && props.page === "sharing") props.onNavigate("invite");
  };
  return (
    <Dialog controlledDismissal open={mode() !== null} title={title()} onClose={close}>
      <View class="gap-3">
        <If when={mode() === "sharing"}>
          <Sharing remote={props.remote} localReady={props.localReady} onInvite={() => void showInvite()} onRevoke={(peer) => removing.set(peer)} />
        </If>
        <If when={mode() === "invite"}>
          <If when={props.remote.invite} else={
            <View class="gap-3">
              <Text class={`${type.ui} text-foreground`}>{t("remote.inviteEnded")}</Text>
              <Button label={t("remote.newCode")} loading={props.remote.busy("invite")} onPress={() => void props.remote.createInvitation()} />
            </View>
          }>
            {(invitation) => (
              <View class="gap-3">
                <QrCode uri={invitation().qrUri} />
                <Field label={t("remote.code")} description={t("remote.expires", {
                  time: new Date(invitation().expiresAt * 1_000).toLocaleTimeString(),
                })}>
                  <View class="flex-row items-center gap-2">
                    <Input class="flex-1" value={invitation().code} accessibilityLabel={t("remote.code")} disabled onChange={() => {}} />
                    <CopyButton label={t("remote.copyCode")} text={invitation().code} />
                  </View>
                </Field>
              </View>
            )}
          </If>
        </If>
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
        <If when={mode() === "revoke" ? removing() : null}>
          {(peer) => (
            <View class="gap-3">
              <Text class={`${type.ui} text-foreground`}>{t("remote.revokeDetails")}</Text>
              <View class="flex-row justify-end gap-2">
                <Button label={t("common.cancel")} onPress={() => removing.set(null)} />
                <Button label={t("remote.revokeAccess")} variant="danger" loading={props.remote.busy(peer().id)}
                  onPress={async () => { if (await props.remote.revoke(peer().id)) removing.set(null); }} />
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
