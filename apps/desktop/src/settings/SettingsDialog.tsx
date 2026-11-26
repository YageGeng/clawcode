import { View } from "nsh";
import { t } from "nsh/i18n";

import type { Appearance, Language, SettingsStore } from "../stores/settings";
import { Dialog } from "../ui/dialog";
import { Field } from "../ui/input";
import { Select } from "../ui/selection";

/** The settings dialog: appearance and interface language, applied as they change. */
export function SettingsDialog(props: { open: boolean; settings: SettingsStore; onClose: () => void }) {
  return (
    <Dialog open={props.open} title={t("settings.title")} onClose={props.onClose}>
      <View class="gap-4">
        <Field label={t("settings.appearance")}>
          <Select
            accessibilityLabel={t("settings.appearance")}
            items={[
              { value: "system", title: t("appearance.system") },
              { value: "light", title: t("appearance.light") },
              { value: "dark", title: t("appearance.dark") },
            ]}
            value={props.settings.settings().appearance}
            onChange={(value) => value !== null && props.settings.update({ appearance: value as Appearance })}
          />
        </Field>
        <Field label={t("settings.language")}>
          <Select
            accessibilityLabel={t("settings.language")}
            items={[
              { value: "system", title: t("language.system") },
              // Each language names itself, so it is found whatever the interface shows.
              { value: "en", title: "English" },
              { value: "zh-CN", title: "简体中文" },
            ]}
            value={props.settings.settings().language}
            onChange={(value) => value !== null && props.settings.update({ language: value as Language })}
          />
        </Field>
      </View>
    </Dialog>
  );
}
