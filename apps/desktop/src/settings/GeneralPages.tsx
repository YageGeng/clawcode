import { Text, View } from "nsh";
import { ExternalEditors } from "clawcode-backend";
import type { JSX } from "nsh/jsx-runtime";
import { t } from "nsh/i18n";

import {
  APPEARANCES,
  FILE_MANAGER,
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
    <View class="flex-row items-center justify-between gap-4 py-3">
      <View class="min-w-0 flex-1 gap-0.5">
        <View class="flex-row items-center gap-2">
          <Text class={`${inlineLabel} ${type.ui} font-medium text-foreground`}>{props.label}</Text>
          {props.badge}
        </View>
        <Text class={`${type.meta} text-muted-foreground`}>{props.description}</Text>
      </View>
      <View class="shrink-0">{props.children}</View>
    </View>
  );
}

/** The General page: interface language and where "Open in" opens a local workspace by default. */
export function GeneralPage(props: { settings: SettingsStore }) {
  const current = () => props.settings.settings();
  // The same choices the details card offers, read once: installing an editor takes an app restart to show.
  const targets = ExternalEditors.targets();
  return (
    <View class="rounded-(--radius-lg) border border-border bg-card">
      <View class="p-4">
        <Field label={t("settings.language")}>
          <Select
            accessibilityLabel={t("settings.language")}
            items={LANGUAGES.map((language) => ({ value: language, title: language === "system" ? t("language.system") : LANGUAGE_NAMES[language] }))}
            value={current().language}
            onChange={(value) => value !== null && props.settings.update({ language: value as Language })}
          />
        </Field>
      </View>
      <Separator />
      <View class="p-4">
        <Field label={t("settings.openIn")} description={t("settings.openInHelp")}>
          <Select
            accessibilityLabel={t("settings.openIn")}
            items={targets.map((target) => ({
              value: target.command,
              title: target.name || (target.command === FILE_MANAGER ? t("details.fileManager") : target.command),
            }))}
            value={targets.some((target) => target.command === current().openIn) ? current().openIn : FILE_MANAGER}
            onChange={(value) => value !== null && props.settings.update({ openIn: value })}
          />
        </Field>
      </View>
    </View>
  );
}

/** The Appearance page: light, dark or system, and the file icon theme. */
export function AppearancePage(props: { settings: SettingsStore }) {
  const current = () => props.settings.settings();
  return (
    <View class="gap-4">
      <View class="rounded-(--radius-lg) border border-border bg-card p-4">
        <Field label={t("settings.appearance")}>
          <Select
            accessibilityLabel={t("settings.appearance")}
            items={APPEARANCES.map((appearance) => ({ value: appearance, title: t(`appearance.${appearance}`) }))}
            value={current().appearance}
            onChange={(value) => value !== null && props.settings.update({ appearance: value as Appearance })}
          />
        </Field>
      </View>
      <View class="overflow-hidden rounded-(--radius-lg) border border-border bg-card">
        <View class="px-4 py-0.5">
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
        </View>
        <If when={current().iconTheme === "dynamic"}>
          <View class="border-t border-border bg-muted/30 px-4 py-0.5">
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
    </View>
  );
}
