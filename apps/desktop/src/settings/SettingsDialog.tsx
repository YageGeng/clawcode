import { View } from "nsh";
import { t } from "nsh/i18n";

import { APPEARANCES, LANGUAGES } from "../stores/settings";
import type { Appearance, Language, SettingsStore } from "../stores/settings";
import { Button } from "../ui/button";
import { Dialog } from "../ui/dialog";
import { Field } from "../ui/input";
import { Select } from "../ui/selection";

/** Each language names itself, so it is found whatever the interface shows. */
const LANGUAGE_NAMES: Readonly<Record<Exclude<Language, "system">, string>> = { en: "English", "zh-CN": "简体中文" };

/** The settings dialog: appearance and interface language, applied as they change. */
export function SettingsDialog(props: { open: boolean; settings: SettingsStore; onClose: () => void; onRemoteSettings?: () => void }) {
  return (
    <Dialog open={props.open} title={t("settings.title")} onClose={props.onClose}>
      <View class="gap-4">
        <Field label={t("settings.appearance")}>
          <Select
            accessibilityLabel={t("settings.appearance")}
            items={APPEARANCES.map((appearance) => ({ value: appearance, title: t(`appearance.${appearance}`) }))}
            value={props.settings.settings().appearance}
            onChange={(value) => value !== null && props.settings.update({ appearance: value as Appearance })}
          />
        </Field>
        <Field label={t("settings.language")}>
          <Select
            accessibilityLabel={t("settings.language")}
            items={LANGUAGES.map((language) => ({ value: language, title: language === "system" ? t("language.system") : LANGUAGE_NAMES[language] }))}
            value={props.settings.settings().language}
            onChange={(value) => value !== null && props.settings.update({ language: value as Language })}
          />
        </Field>
        <If when={props.onRemoteSettings !== undefined}>
          <Button class="self-start" label={t("remote.title")} icon="laptop" variant="outline" onPress={() => props.onRemoteSettings?.()} />
        </If>
      </View>
    </Dialog>
  );
}
