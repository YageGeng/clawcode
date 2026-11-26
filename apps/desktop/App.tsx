import { WindowControls } from "clawcode-backend";
import { cleanup, mount, platform } from "nsh";
import { Application, currentWindow, isAvailable } from "nsh/desktop";

import { Shell } from "./src/shell/Shell";
import { TITLE_BAR_HEIGHT } from "./src/shell/TitleBar";
import { createAppStore } from "./src/stores/app";
import { createHostStore, LOCAL_HOST } from "./src/stores/host";
import { createSettingsStore } from "./src/stores/settings";
import { NativeTransport } from "./src/transport/native";
import { configureDesktopFonts } from "./src/ui/fonts";

/** The app's root: owns the stores, this computer's host and the in-process backend behind it. */
function App() {
  // The shell draws its own title bar; the system keeps only its window buttons. The main window's size, least
  // size and restored placement are launch options in nsh.json's "window".
  if (isAvailable) {
    if (platform.os === "linux") WindowControls.styleTitleBar();
    currentWindow().set({ titleBar: { style: "hidden", height: TITLE_BAR_HEIGHT } });
  }
  const transport = new NativeTransport();
  const store = createAppStore([createHostStore(LOCAL_HOST, transport)]);
  const settings = createSettingsStore();
  void settings.load();
  void store.start();
  // Quitting waits for the backend (its SQLite lease, queued state syncs, exec
  // processes); NSH awaits quit promises for only 200 ms, so this one blocks.
  const stopQuit = Application.on("quit", () => transport.closeNow());
  // A hot reload or a closed root stops every host without holding the engine.
  cleanup(() => {
    stopQuit();
    store.stop();
  });
  return <Shell store={store} settings={settings} />;
}

configureDesktopFonts();
mount(App);
