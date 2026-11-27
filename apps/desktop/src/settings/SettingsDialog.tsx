import { Scroll, Text, View } from "nsh";
import type { JSX } from "nsh/jsx-runtime";
import { t } from "nsh/i18n";

import {
  APPEARANCES,
  ICON_THEME_MODES,
  ICON_THEME_SELECTIONS,
  ICON_THEMES,
  LANGUAGES,
} from "../stores/settings";
import type {
  Appearance,
  IconThemeMode,
  IconThemeName,
  IconThemeSelection,
  Language,
  SettingsStore,
} from "../stores/settings";
import { Button } from "../ui/button";
import { Dialog } from "../ui/dialog";
import { FileIcon } from "../ui/fileIcon";
import { Field } from "../ui/input";
import { Select } from "../ui/selection";
import { Separator } from "../ui/surface";
import { inlineLabel, type } from "../ui/tokens";

/** Each language names itself, so it is found whatever the interface shows. */
const LANGUAGE_NAMES: Readonly<Record<Exclude<Language, "system">, string>> = { en: "English", "zh-CN": "简体中文" };

/** Proper names of Catppuccin flavors stay identical across locales; generic packs localize. */
function iconThemeTitle(theme: IconThemeSelection): string {
  switch (theme) {
    case "dynamic":
      return t("iconTheme.dynamic");
    case "catppuccin-latte":
      return "Catppuccin Latte";
    case "catppuccin-frappe":
      return "Catppuccin Frappé";
    case "catppuccin-macchiato":
      return "Catppuccin Macchiato";
    case "catppuccin-mocha":
      return "Catppuccin Mocha";
    case "minimal":
      return t("iconTheme.minimal");
  }
}

/** One horizontal label + description row paired with a right-aligned control inside a settings group. */
function SettingRow(props: { label: string; description: string; badge?: JSX.Element; children: JSX.Element }) {
  return (
    <View class="flex-row items-center justify-between gap-4 py-2">
      <View class="min-w-0 flex-1 gap-0.5">
        <View class="flex-row items-center gap-2">
          <Text class={`${inlineLabel} ${type.ui} font-medium text-popover-foreground`}>{props.label}</Text>
          {props.badge}
        </View>
        <Text class={`${type.meta} text-muted-foreground`}>{props.description}</Text>
      </View>
      <View class="shrink-0">{props.children}</View>
    </View>
  );
}

/** The settings dialog: appearance, interface language, and file icon theme, applied as they change. */
export function SettingsDialog(props: { open: boolean; settings: SettingsStore; onClose: () => void; onRemoteSettings?: () => void }) {
  const current = () => props.settings.settings();
  return (
    <Dialog open={props.open} title={t("settings.title")} onClose={props.onClose}>
      <Scroll class="max-h-[480px]" contentClass="gap-3.5 pr-0.5">
        <Field label={t("settings.appearance")}>
          <Select
            accessibilityLabel={t("settings.appearance")}
            items={APPEARANCES.map((appearance) => ({ value: appearance, title: t(`appearance.${appearance}`) }))}
            value={current().appearance}
            onChange={(value) => value !== null && props.settings.update({ appearance: value as Appearance })}
          />
        </Field>
        <Field label={t("settings.language")}>
          <Select
            accessibilityLabel={t("settings.language")}
            items={LANGUAGES.map((language) => ({ value: language, title: language === "system" ? t("language.system") : LANGUAGE_NAMES[language] }))}
            value={current().language}
            onChange={(value) => value !== null && props.settings.update({ language: value as Language })}
          />
        </Field>
        <Separator />
        <View class="gap-1.5">
          <SettingRow
            label={t("settings.iconTheme")}
            description={t("settings.iconThemeHelp")}
            badge={
              <View accessibilityElementsHidden class="flex-row items-center gap-1.5 rounded-full border border-border bg-muted/60 px-2 py-0.5">
                <FileIcon path="src" kind="directory" open size="sm" />
                <FileIcon path="main.rs" size="sm" />
                <FileIcon path="App.tsx" size="sm" />
                <FileIcon path="Cargo.toml" size="sm" />
              </View>
            }
          >
            <Select
              size="sm"
              accessibilityLabel={t("settings.iconTheme")}
              items={ICON_THEME_SELECTIONS.map((item) => ({ value: item, title: iconThemeTitle(item) }))}
              value={current().iconTheme}
              onChange={(value) => {
                if (value !== null) props.settings.update({ iconTheme: value as IconThemeSelection });
              }}
            />
          </SettingRow>
          <If when={current().iconTheme === "dynamic"}>
            <View class="rounded-(--radius) border border-border/80 bg-muted/45 px-3.5 py-0.5">
              <SettingRow label={t("iconTheme.mode")} description={t("iconTheme.modeHelp")}>
                <Select
                  size="sm"
                  accessibilityLabel={t("iconTheme.mode")}
                  items={ICON_THEME_MODES.map((mode) => ({ value: mode, title: t(`iconTheme.modes.${mode}`) }))}
                  value={current().iconThemeMode}
                  onChange={(value) => value !== null && props.settings.update({ iconThemeMode: value as IconThemeMode })}
                />
              </SettingRow>
              <Separator />
              <SettingRow label={t("iconTheme.lightTheme")} description={t("iconTheme.lightThemeHelp")}>
                <Select
                  size="sm"
                  accessibilityLabel={t("iconTheme.lightTheme")}
                  items={ICON_THEMES.map((item) => ({ value: item, title: iconThemeTitle(item) }))}
                  value={current().lightIconTheme}
                  onChange={(value) => value !== null && props.settings.update({ lightIconTheme: value as IconThemeName })}
                />
              </SettingRow>
              <Separator />
              <SettingRow label={t("iconTheme.darkTheme")} description={t("iconTheme.darkThemeHelp")}>
                <Select
                  size="sm"
                  accessibilityLabel={t("iconTheme.darkTheme")}
                  items={ICON_THEMES.map((item) => ({ value: item, title: iconThemeTitle(item) }))}
                  value={current().darkIconTheme}
                  onChange={(value) => value !== null && props.settings.update({ darkIconTheme: value as IconThemeName })}
                />
              </SettingRow>
            </View>
          </If>
        </View>
        <If when={props.onRemoteSettings !== undefined}>
          <Separator />
          <Button class="self-start" label={t("remote.title")} icon="laptop" variant="outline" onPress={() => props.onRemoteSettings?.()} />
        </If>
      </Scroll>
    </Dialog>
  );
}
