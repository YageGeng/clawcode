import { cleanup, mount } from "nsh";
import { Application } from "nsh/desktop";

import { messageOf } from "./src/client/errors";
import { Shell } from "./src/shell/Shell";
import { createAppStore } from "./src/stores/app";
import { createHostStore, LOCAL_HOST } from "./src/stores/host";
import { RemoteStore } from "./src/stores/remote";
import { createSettingsStore } from "./src/stores/settings";
import { NativeTransport } from "./src/transport/native";
import { configureDesktopFonts } from "./src/ui/fonts";

/** The app's root: owns the stores, this computer's host and the in-process backend behind it. */
function App() {
  const transport = new NativeTransport();
  const store = createAppStore([createHostStore(LOCAL_HOST, transport)]);
  const settings = createSettingsStore();
  const remote = new RemoteStore(store, transport);
  void settings.load().catch((error: unknown) => console.warn(`loading appearance and language: ${messageOf(error)}`));
  void remote.load(store.start());
  // Quitting waits for the backend (its SQLite lease, queued state syncs, exec
  // processes); NSH awaits quit promises for only 200 ms, so this one blocks.
  const stopQuit = Application.on("quit", () => {
    remote.close();
    transport.closeNow();
  });
  // A hot reload or a closed root stops every host without holding the engine.
  cleanup(() => {
    stopQuit();
    remote.close();
    store.stop();
  });
  return <Shell store={store} settings={settings} remote={remote} />;
}

configureDesktopFonts();
mount(App);
