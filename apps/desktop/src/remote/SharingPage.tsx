import { computed, effect, signal, Text, View } from "nsh";
import { t } from "nsh/i18n";

import type { RemoteStore } from "../stores/remote";
import { Appear } from "../ui/appear";
import { Button } from "../ui/button";
import { CopyButton } from "../ui/copy";
import { confirm } from "../ui/dialog";
import { Field, Input } from "../ui/input";
import { QrCode } from "../ui/qr";
import { Switch } from "../ui/selection";
import { Badge, Separator } from "../ui/surface";
import { inlineLabel, type } from "../ui/tokens";

/** Local identity and incoming access settings, unfolding a pairing invitation directly under the sharing switch. */
function Sharing(props: {
  remote: RemoteStore;
  localReady: boolean;
  inviting: boolean;
  onInvite: () => void;
  onDoneInvite: () => void;
  onRevoke: (peer: { id: string; name: string }) => void;
}) {
  const savedName = computed(() => props.remote.snapshot?.name ?? "");
  const name = signal(savedName());
  // Native changes to the other device lists must not overwrite the name while it is being edited.
  effect(() => name.set(savedName()));
  return (
    <View class="gap-5">
      <View class="overflow-hidden rounded-(--radius-lg) border border-border bg-card">
        <View class="p-4">
          <Field label={t("remote.deviceName")}>
            <View class="flex-row items-center gap-2">
              <Input class="flex-1" accessibilityLabel={t("remote.deviceName")} value={name()} onChange={(value) => name.set(value)} />
              <Button label={t("remote.save")} disabled={!props.remote.snapshot || !name().trim() || name().trim() === savedName()}
                loading={props.remote.busy("rename")} onPress={() => void props.remote.rename(name())} />
            </View>
          </Field>
        </View>
        <Separator />
        <View class="gap-3 p-4">
          <View class="flex-row items-center justify-between gap-3">
            <View class="min-w-0 flex-1 gap-1">
              <Switch label={t("remote.allowControl")} checked={props.remote.sharingRequested || props.remote.snapshot?.enabled === true}
                disabled={!props.remote.snapshot || props.remote.busy("sharing") || (!props.localReady && !props.remote.sharingRequested && !props.remote.snapshot.enabled)}
                onChange={(enabled) => void props.remote.setSharing(enabled)} />
              <If when={!props.localReady}>
                <Text class={`${type.meta} text-muted-foreground`}>{t("remote.localUnavailable")}</Text>
              </If>
            </View>
            <If when={props.remote.snapshot?.enabled && !props.inviting}>
              <Button label={t("remote.addDevice")} icon="plus" variant="outline" size="sm"
                loading={props.remote.busy("invite")} onPress={props.onInvite} />
            </If>
          </View>
          <If when={props.inviting && props.remote.snapshot?.enabled}>
            <Invitation remote={props.remote} onDone={props.onDoneInvite} />
          </If>
        </View>
      </View>
      <View class="gap-2">
        <Text role="header" class={`${type.ui} font-medium text-foreground`}>{t("remote.trustedDevices")}</Text>
        <If when={props.remote.snapshot?.trusted.length}
          else={
            <View class="rounded-(--radius-lg) border border-border bg-card px-4 py-5">
              <Text class={`${type.meta} text-muted-foreground`}>{t("remote.noDevices")}</Text>
            </View>
          }>
          <View class="overflow-hidden rounded-(--radius-lg) border border-border bg-card">
            <Each of={props.remote.snapshot?.trusted ?? []} key={(peer) => peer.id}>
              {(peer, index) => {
                const online = () => props.remote.snapshot?.online.some((device) => device.id === peer().id) === true;
                return (
                  <View class={`flex-row items-center gap-3 px-4 py-3 ${index() > 0 ? "border-t border-border" : ""}`}>
                    <View class="min-w-0 flex-1 gap-0.5">
                      <View class="flex-row items-center gap-2">
                        <Text class={`${inlineLabel} ${type.ui} font-medium text-foreground truncate`}>{peer().name}</Text>
                        <If when={online()}><Badge label={t("remote.status.ready")} tone="success" /></If>
                      </View>
                      <Text class={`font-mono ${type.meta} text-muted-foreground`}>{peer().id.slice(0, 12)}</Text>
                    </View>
                    <If when={online()}>
                      <Button label={t("remote.disconnect")} size="sm" variant="outline" disabled={props.remote.busy(peer().id)}
                        onPress={() => void props.remote.disconnect(peer().id)} />
                    </If>
                    <Button label={t("remote.revoke")} size="sm" variant="ghost" disabled={props.remote.busy(peer().id)}
                      onPress={() => props.onRevoke(peer())} />
                  </View>
                );
              }}
            </Each>
          </View>
        </If>
      </View>
    </View>
  );
}

/** The pairing code and QR of an open invitation, or a way to make a new one once it ended. */
function Invitation(props: { remote: RemoteStore; onDone: () => void }) {
  return (
    <Appear entrance="riseSm">
      <View class="gap-3 rounded-(--radius) bg-muted/75 p-4">
        <View class="flex-row items-center justify-between gap-2">
          <Text class={`${inlineLabel} ${type.ui} font-medium text-foreground`}>{t("remote.addDevice")}</Text>
          <Button label={t("common.close")} size="sm" variant="ghost" onPress={props.onDone} />
        </View>
        <If when={props.remote.invite} else={
          <View class="gap-3">
            <Text class={`${type.ui} text-foreground`}>{t("remote.inviteEnded")}</Text>
            <Button class="self-start" label={t("remote.newCode")} loading={props.remote.busy("invite")}
              onPress={() => void props.remote.createInvitation()} />
          </View>
        }>
          {(invitation) => (
            <View class="gap-3">
              <View class="self-center overflow-hidden rounded-(--radius)">
                <QrCode uri={invitation().qrUri} />
              </View>
              <Field label={t("remote.code")} description={t("remote.expires", {
                time: new Date(invitation().expiresAt * 1_000).toLocaleTimeString(),
              })}>
                <View class="flex-row items-center gap-2">
                  <Input class="flex-1 font-mono" value={invitation().code} accessibilityLabel={t("remote.code")} disabled onChange={() => {}} />
                  <CopyButton label={t("remote.copyCode")} text={invitation().code} />
                </View>
              </Field>
            </View>
          )}
        </If>
      </View>
    </Appear>
  );
}

/** The settings window's Remote control page: this computer's sharing, a pairing invitation in place, trusted devices. */
export function SharingPage(props: { remote: RemoteStore; localReady: boolean }) {
  const inviting = signal(false);
  // Turning off remote control closes any open invitation panel immediately.
  effect(() => {
    if (props.remote.snapshot?.enabled === false) inviting.set(false);
  });
  /** Opens a fresh invitation directly inside the sharing card. */
  const invite = async () => {
    props.remote.clearError();
    if (await props.remote.createInvitation()) inviting.set(true);
  };
  /** Asks before revoking: the device disconnects at once and must pair again. */
  const revoke = async (peer: { id: string; name: string }) => {
    const yes = await confirm({ title: t("remote.revokeTitle", { name: peer.name }), description: t("remote.revokeDetails"),
      action: t("remote.revokeAccess"), danger: true });
    if (yes) await props.remote.revoke(peer.id);
  };
  return (
    <View class="gap-4">
      <Sharing remote={props.remote} localReady={props.localReady} inviting={inviting()}
        onInvite={() => void invite()} onDoneInvite={() => inviting.set(false)} onRevoke={(peer) => void revoke(peer)} />
      <If when={props.remote.error}>
        {(reason) => <Text role="alert" class={`${type.meta} text-destructive`}>{reason()}</Text>}
      </If>
    </View>
  );
}
