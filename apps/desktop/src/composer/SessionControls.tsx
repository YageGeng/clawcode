import { computed, effect, keymap, signal, Text, View } from "nsh";
import type { ViewRef } from "nsh";
import type { JSX } from "nsh/jsx-runtime";
import { t } from "nsh/i18n";
import type { TranslationKey } from "nsh/i18n";

import type { SessionId } from "../client/acp/protocol";
import type { SessionConfigSelect } from "../client/acp/sessionConfig";
import type { HostStore } from "../stores/host";
import { Button, IconButton } from "../ui/button";
import { Spinner } from "../ui/feedback";
import type { IconName } from "../ui/icon";
import { Field } from "../ui/input";
import { Menu, MenuItem, Popover, Tooltip } from "../ui/menu";
import { Select, StepSlider } from "../ui/selection";
import { type } from "../ui/tokens";

/** Known protocol values have UI translations; the host supplies model names and any unfamiliar choices. */
const translatedChoices: Readonly<Record<string, readonly string[]>> = {
  mode: ["default", "plan"],
  thinking_level: ["off", "minimal", "low", "medium", "high", "x_high"],
};

/** Approval choices exposed by Clawcode have localized descriptions and distinct menu icons. */
const approvalIcons = new Map<string, IconName>([["host", "laptop"], ["ask", "shield-check"], ["allow", "triangle-alert"]]);

/** Presents exactly the choices offered for this Session, keeping their order and opaque protocol values. */
function choices(option: SessionConfigSelect) {
  return option.options.map(({ value, name }) => ({
    value,
    // `translatedChoices` lists exactly the values with a key, so the built key names an existing translation.
    title: translatedChoices[option.id]?.includes(value)
      ? t(`composer.${option.id}.${value}` as Extract<TranslationKey, `composer.${string}.${string}`>) : name,
  }));
}

/** The toolbar and setting editor for the selected host and Session. Only confirmed choices appear at rest. */
export function SessionControls(props: {
  host: HostStore;
  sessionId: SessionId | null;
  disabled: boolean;
  turnBusy: boolean;
  preparing: boolean;
  picking: boolean;
  atImageLimit: boolean;
  onAttach: () => void;
  onConfigureNew?: () => Promise<void>;
  children: JSX.Element;
}) {
  // A prepared draft can change workspace without remounting. Its popup and errors belong to one connection and ID.
  type Target = Readonly<{ host: HostStore; id: SessionId }>;
  const popup = signal<Target | null>(null);
  const approvalPopup = signal<Target | null>(null);
  const failure = signal<Target | null>(null);
  const modelMenuOpen = signal(false);
  const matches = (target: Target | null) => target?.host === props.host && target.id === props.sessionId;
  const settings = computed(() => props.sessionId === null ? [] : props.host.sessionConfig(props.sessionId));
  const model = computed(() => settings().find((option) => option.id === "model"));
  const level = computed(() => settings().find((option) => option.id === "thinking_level"));
  const mode = computed(() => settings().find((option) => option.id === "mode"));
  const approval = computed(() => settings().find((option) => option.id === "file_approval"));
  const levels = computed(() => level() ? choices(level()!) : []);
  const saving = () => props.sessionId !== null && props.host.configPending(props.sessionId);
  /** File policy applies to subsequent mutations even when a prompt is still running. */
  const locked = (setting?: string) => props.disabled || props.preparing || saving() || props.host.phase().kind !== "ready"
    || (props.turnBusy && setting !== "file_approval");
  /** The model's display name, or the generic label when the host offers none. */
  const modelName = (selected: SessionConfigSelect | undefined) =>
    selected?.options.find((item) => item.value === selected.currentValue)?.name ?? selected?.currentValue ?? t("composer.settings");
  const summary = () => modelName(model());
  // A draft has no Session yet: it names the model a new Session on this host would start with.
  const draftSummary = () => modelName(props.host.defaultConfig().find((option) => option.id === "model"));
  let modelTrigger: ViewRef | undefined;

  /** Ask for a Session only on explicit configuration, then open its real choices above the existing draft. */
  const prepare = async (panel: "model" | "approval") => {
    const configure = props.onConfigureNew;
    if (locked() || !configure) return;
    try {
      await configure();
      if (props.onConfigureNew === configure && props.sessionId !== null) {
        (panel === "approval" ? approvalPopup : popup).set({ host: props.host, id: props.sessionId });
      }
    } catch {
      // The shell shows the failed session/new request beside the retained empty composer.
      if (props.onConfigureNew === configure) popup.set(null);
    }
  };

  /** Validate a choice again at activation; a live host update may have replaced an open menu's options. */
  const change = async (id: string, value: string | null) => {
    const host = props.host;
    const sessionId = props.sessionId;
    const option = settings().find((candidate) => candidate.id === id);
    if (locked(id) || !sessionId || !option || value === null || option.currentValue === value ||
        !option.options.some((candidate) => candidate.value === value)) return;
    failure.set(null);
    try {
      await host.setSessionConfig(sessionId, id, value);
    } catch {
      if (props.host === host && props.sessionId === sessionId) failure.set({ host, id: sessionId });
    }
  };

  // A pointer can open the popup without focusing GTK's slider thumb. Left and Right still step the visible
  // slider, except while the nested select owns navigation or the host is applying the previous edit.
  effect(() => {
    if (!matches(popup()) || modelMenuOpen() || locked()) return;
    const items = levels();
    const index = items.findIndex((item) => item.value === level()?.currentValue);
    if (index < 0 || items.length < 2) return;
    const removeLeft = keymap.bind("left", () => {
      if (index > 0) void change("thinking_level", items[index - 1]!.value);
    });
    const removeRight = keymap.bind("right", () => {
      if (index + 1 < items.length) void change("thinking_level", items[index + 1]!.value);
    });
    return () => { removeLeft(); removeRight(); };
  });

  return (
    <View class="gap-1">
      <View class="flex-row items-center gap-1 border-t border-border/60 pt-2">
        <IconButton icon="plus" label={t("composer.attach")} loading={props.picking} disabled={locked() || props.atImageLimit}
          onPress={props.onAttach} tooltip={<Tooltip text={t(props.atImageLimit ? "attachments.tooMany" : "composer.attach")} />} />
        <If when={props.sessionId === null && props.onConfigureNew !== undefined}>
          <IconButton icon="shield-check" label={t("fileApproval.title")} disabled={locked()}
            onPress={() => void prepare("approval")} tooltip={<Tooltip text={t("fileApproval.title")} />} />
        </If>
        <If when={approval()}>
          {(option) => (
            <Menu title={t("fileApproval.title")} class="w-72" open={matches(approvalPopup())}
              onOpenChange={(next) => approvalPopup.set(next && props.sessionId ? { host: props.host, id: props.sessionId } : null)} trigger={
              <IconButton icon={option().currentValue === "allow" ? "triangle-alert" : "shield-check"}
                label={t("fileApproval.title")} tooltip={<Tooltip text={t("fileApproval.title")} />} />
            }>
              <Each of={option().options} key={(item) => item.value}>
                {(item) => {
                  const icon = () => approvalIcons.get(item().value);
                  return <MenuItem icon={icon()} label={icon() ? t(`fileApproval.${item().value}` as Extract<TranslationKey, `fileApproval.${string}`>) : item().name}
                    description={icon() ? t(`fileApproval.${item().value}Help` as Extract<TranslationKey, `fileApproval.${string}Help`>) : undefined}
                    checked={item().value === option().currentValue} warning={item().value === "allow"} disabled={locked("file_approval")}
                    onSelect={() => void change("file_approval", item().value)} />;
                }}
              </Each>
            </Menu>
          )}
        </If>
        <If when={mode()}>
          {(option) => (
            <Select size="sm" variant="toolbar" class="max-w-28" accessibilityLabel={t("composer.modeLabel")}
              items={choices(option())} value={option().currentValue} placeholder={option().currentValue}
              disabled={locked() || option().options.length === 0} onChange={(value) => void change("mode", value)} />
          )}
        </If>
        <View class="flex-1" />
        <If when={props.sessionId === null && props.onConfigureNew !== undefined}>
          <Button label={draftSummary()} accessibilityLabel={t("composer.settings")} variant="ghost" size="sm"
            class="max-w-52" trailingIcon="chevron-down" loading={props.preparing} disabled={locked()} onPress={() => void prepare("model")} />
        </If>
        <If when={props.sessionId !== null && (model() || level() || props.onConfigureNew)}>
          <Popover class="w-64 gap-3" open={matches(popup())}
            onOpenChange={(next) => popup.set(next && props.sessionId !== null ? { host: props.host, id: props.sessionId } : null)}
            onOpen={() => modelTrigger?.focus()}
            trigger={<Button label={summary()} accessibilityLabel={t("composer.settings")} variant="ghost" size="sm"
              class="max-w-52" trailingIcon="chevron-down" selected={matches(popup())} />}>
            <If when={model()}>
              {(option) => (
                <Field label={t("composer.modelLabel")}>
                  <Select accessibilityLabel={t("composer.modelLabel")} items={choices(option())}
                    triggerRef={(node) => { modelTrigger = node; }}
                    onOpenChange={(next) => modelMenuOpen.set(next)}
                    value={option().currentValue} placeholder={option().currentValue}
                    disabled={locked() || option().options.length === 0} onChange={(value) => void change("model", value)} />
                </Field>
              )}
            </If>
            <If when={level()}>
              {(option) => (
                <If when={levels().length > 1 && levels().some((item) => item.value === option().currentValue)}
                  else={
                    <Field label={t("composer.thinkingLabel")}>
                      <Select accessibilityLabel={t("composer.thinkingLabel")} items={levels()} value={option().currentValue}
                        placeholder={option().currentValue} disabled={locked() || levels().length < 1}
                        onChange={(value) => void change("thinking_level", value)} />
                    </Field>
                  }>
                  <StepSlider label={t("composer.thinkingLabel")} items={levels()} value={option().currentValue}
                    disabled={locked()} onChange={(value) => void change("thinking_level", value)} />
                </If>
              )}
            </If>
            <If when={!model() && !level()}>
              <Text class={`${type.ui} text-muted-foreground`}>{t("composer.noModelSettings")}</Text>
            </If>
          </Popover>
        </If>
        <If when={saving()}><Spinner label={t("composer.updating")} size="sm" /></If>
        {props.children}
      </View>
      <If when={matches(failure())}>
        <Text role="alert" class={`px-1 ${type.meta} text-destructive`}>{t("composer.updateFailed")}</Text>
      </If>
    </View>
  );
}
