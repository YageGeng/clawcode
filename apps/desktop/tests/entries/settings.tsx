// Test entry: the settings dialog, open, over a fresh settings store.
import { mount, View } from "nsh";

import { SettingsDialog } from "../../src/settings/SettingsDialog";
import { createSettingsStore } from "../../src/stores/settings";

/** Mounts the dialog open. */
function Entry() {
  const settings = createSettingsStore();
  void settings.load();
  return (
    <View class="flex-1 bg-background">
      <SettingsDialog open settings={settings} onClose={() => {}} />
    </View>
  );
}

mount(Entry);
