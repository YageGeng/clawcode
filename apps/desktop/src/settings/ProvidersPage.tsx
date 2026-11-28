import { cleanup, effect, openUrl, signal, Text, View } from "nsh";
import { t } from "nsh/i18n";

import type { AuthMethod, ProviderSignIn, SignInFlow } from "../client/domain/model";
import { messageOf } from "../client/errors";
import type { HostStore } from "../stores/host";
import { Appear } from "../ui/appear";
import { Button } from "../ui/button";
import { CopyButton } from "../ui/copy";
import { Empty, Spinner } from "../ui/feedback";
import { Icon } from "../ui/icon";
import { Badge } from "../ui/surface";
import type { Tone } from "../ui/tokens";
import { type } from "../ui/tokens";

/** How each sign-in state reads at a glance. */
const statusTone: Readonly<Record<ProviderSignIn["status"], Tone>> = { signedIn: "success", signInRequired: "warning", unknown: "neutral" };

/** The device code a sign-in waits on: the code to enter, where to enter it, and the host's own instructions. */
function DeviceCode(props: { code: NonNullable<SignInFlow["code"]> }) {
  // Only https links may open; any other address, or a launch failure, is left for the user to copy.
  const openFailure = signal<string | undefined>(undefined);
  /** Opens the verification address in the browser, reporting a failure under the button. */
  const open = () => {
    openFailure.set(undefined);
    /** Shows why the address did not open. */
    const fail = (detail: string) => openFailure.set(t("settings.providers.openFailed", { url: props.code.url, detail }));
    // No app for the address resolves unopened rather than rejecting.
    openUrl(props.code.url).then(({ opened }) => { if (!opened) fail(t("settings.providers.noBrowser")); }, (error: unknown) => fail(messageOf(error)));
  };
  return (
    // Inside the provider's card: a quiet fill sets it apart without a second outline.
    <Appear entrance="riseSm">
      <View class="gap-3 rounded-(--radius) bg-muted/75 p-4">
        <If when={props.code.userCode}>
          {(userCode) => (
            <View class="flex-row items-center gap-2 self-start rounded-(--radius) border border-border bg-card px-3 py-1.5">
              <View selectable>
                <Text class={`font-mono ${type.title} tracking-widest text-foreground`}>{userCode()}</Text>
              </View>
              <CopyButton label={t("settings.providers.copyCode")} text={userCode()} />
            </View>
          )}
        </If>
        <Text class={`${type.meta} text-muted-foreground`}>{props.code.message}</Text>
        <Button class="self-start" label={t("settings.providers.openBrowser")} icon="external-link" variant="primary" onPress={open} />
        <If when={openFailure()}>
          {(text) => (
            <View class="flex-row items-center gap-2">
              <Text class={`min-w-0 flex-1 ${type.meta} text-destructive`}>{text()}</Text>
              <CopyButton label={t("settings.providers.copyUrl")} text={props.code.url} />
            </View>
          )}
        </If>
      </View>
    </Appear>
  );
}

/**
 * The Providers page: this computer's sign-in methods, whether each has a usable credential, and signing in to one.
 * Only the local host is managed here; a paired computer manages its own credentials. `startSignIn` begins one sign-in
 * as the page opens (a notice asked for it).
 */
export function ProvidersPage(props: { host?: HostStore; startSignIn?: string; onStarted?: () => void }) {
  const statuses = signal<ReadonlyMap<string, ProviderSignIn["status"]>>(new Map());
  const failure = signal<string | undefined>(undefined);
  let mounted = true;
  // The user's own cancel ends the sign-in with an error that is not a failure to report.
  let cancelledByUser = false;
  cleanup(() => { mounted = false; });
  const ready = () => props.host?.phase().kind === "ready";
  /** Reads every method's status again. */
  const refresh = (host: HostStore) => host.signInStatuses().then(
    (list) => {
      if (!mounted) return;
      statuses.set(new Map(list.map((item) => [item.methodId, item.status])));
      failure.set(undefined);
    },
    (error: unknown) => { if (mounted) failure.set(t("settings.providers.failed", { detail: messageOf(error) })); },
  );
  /** Signs in to `method`; the statuses are read again when the flow ends. A cancelled sign-in is not a failure. */
  const signIn = (method: AuthMethod) => {
    const host = props.host;
    if (!host) return;
    failure.set(undefined);
    cancelledByUser = false;
    void host.signIn(method.id).then(
      () => undefined,
      (error: unknown) => {
        if (mounted && !cancelledByUser) failure.set(t("settings.providers.signInFailed", { name: method.name, detail: messageOf(error) }));
      },
    );
  };
  // Read the statuses whenever the local backend becomes ready (again) and whenever a sign-in ends. The flow lives in
  // the host, so one started before the user left this page and came back still refreshes the page that shows now.
  effect(() => {
    const host = props.host;
    if (host && ready() && host.signInFlow() === null) void refresh(host);
  });
  // A notice asked for one sign-in: begin it once the page can.
  effect(() => {
    const method = props.host?.authMethods().find((item) => item.id === props.startSignIn);
    if (!method || !ready() || props.host?.signInFlow() !== null) return;
    props.onStarted?.();
    signIn(method);
  });
  return (
    <View class="gap-4">
      <Text class={`${type.ui} text-muted-foreground`}>{t("settings.providers.help")}</Text>
      <If when={ready()} else={<Empty icon="shield-check" title={t("settings.providers.unavailable")} />}>
        <If when={props.host?.authMethods().length} else={<Empty icon="shield-check" title={t("settings.providers.none")} />}>
          <View class="overflow-hidden rounded-(--radius-lg) border border-border bg-card">
            <Each of={props.host?.authMethods() ?? []} key={(method) => method.id}>
              {(method, index) => {
                const status = () => statuses().get(method().id) ?? "unknown";
                const flow = () => props.host?.signInFlow();
                const mine = () => flow()?.methodId === method().id;
                return (
                  <View class={`gap-3 px-4 py-3.5 ${index() > 0 ? "border-t border-border" : ""}`}>
                    <View class="flex-row items-center gap-3">
                      <View class="min-w-0 flex-1 gap-0.5">
                        <Text class={`${type.ui} font-medium text-foreground`}>{method().name}</Text>
                        <If when={method().description}>
                          {(description) => <Text class={`${type.meta} text-muted-foreground`}>{description()}</Text>}
                        </If>
                      </View>
                      <If when={mine()} else={
                        <View class="flex-row items-center gap-2.5">
                          <Badge label={t(`settings.providers.status.${status()}`)} tone={statusTone[status()]} />
                          <If when={status() !== "signedIn"}>
                            <Button label={t("auth.signIn")} size="sm" variant="outline" disabled={flow() !== null}
                              onPress={() => signIn(method())} />
                          </If>
                        </View>
                      }>
                        <View class="flex-row items-center gap-2">
                          <Spinner label={t("settings.providers.waiting")} size="sm" />
                          <Text class={`${type.meta} text-muted-foreground`}>{t("settings.providers.waiting")}</Text>
                          <Button label={t("common.cancel")} size="sm" variant="ghost" onPress={() => { cancelledByUser = true; props.host?.cancelSignIn(); }} />
                        </View>
                      </If>
                    </View>
                    <If when={mine() ? flow()?.code : undefined}>{(code) => <DeviceCode code={code()} />}</If>
                  </View>
                );
              }}
            </Each>
          </View>
        </If>
      </If>
      <If when={failure()}>{(text) => <Text role="alert" class={`${type.meta} text-destructive`}>{text()}</Text>}</If>
      <View class="flex-row items-center gap-1.5">
        <Icon name="info" size="sm" color="muted-foreground" />
        <Text class={`min-w-0 flex-1 ${type.meta} text-muted-foreground`}>{t("settings.providers.localOnly")}</Text>
      </View>
    </View>
  );
}
