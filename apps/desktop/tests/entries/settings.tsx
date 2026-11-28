// Test entry: the settings window's Appearance page over a fresh settings store.
import { mount, View } from "nsh";

import { AppearancePage } from "../../src/settings/GeneralPages";
import { createSettingsStore } from "../../src/stores/settings";

/** Mounts the page at the width the settings window gives it. */
function Entry() {
  const settings = createSettingsStore();
  void settings.load();
  return (
    <View class="flex-1 bg-background p-8">
      <View class="w-[640px]"><AppearancePage settings={settings} /></View>
    </View>
  );
}

mount(Entry);
