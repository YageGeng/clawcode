import process from "process";
import { open, test } from "nsh/test";

import { DESKTOP } from "./support/desktop";

// The real backend reads `CLAW_CONFIG`; run with the offline fixture
// (`CLAW_CONFIG=$PWD/tests/fixture/config.toml nsh test`) so the tests never
// open the user's own sessions.
test("connects_and_lists_sessions", async () => {
  // Refuse to start the real backend on the user's own configuration and Sessions.
  const config = process.env.CLAW_CONFIG ?? "";
  if (!config.endsWith("tests/fixture/config.toml")) {
    throw new Error(`run with CLAW_CONFIG=$PWD/tests/fixture/config.toml (got ${JSON.stringify(config)})`);
  }
  const app = await open("../App.tsx", DESKTOP);
  // Rendered only once the host is ready: the backend answered initialize and session/list.
  await app.waitFor("text:New session", { timeout: 20000 });
});

test("shows_the_local_host_name", async () => {
  const app = await open("../App.tsx", {
    ...DESKTOP,
    modules: {
      "clawcode-backend": {
        "Backend.start": () => {
          throw new Error("not needed for this test");
        },
      },
    },
  });
  await app.waitFor("text:This computer");
});

test("startup_failure_reaches_the_screen", async () => {
  const app = await open("../App.tsx", {
    ...DESKTOP,
    modules: {
      "clawcode-backend": {
        "Backend.start": () => {
          throw new Error("config file not found: /nowhere/config.toml");
        },
      },
    },
  });
  await app.waitFor("text:Backend failed to start: config file not found: /nowhere/config.toml");
});

test("remote_pairing_is_reachable_when_the_local_backend_fails", async () => {
  const app = await open("../App.tsx", {
    ...DESKTOP,
    modules: { "clawcode-backend": { "Backend.start": () => { throw new Error("local backend unavailable"); } } },
  });
  await app.waitFor("text:Backend failed to start: local backend unavailable");
  await app.press("text:This computer");
  await app.press("text:Add remote computer");
  await app.waitFor("label:Pairing code");
  await app.waitFor("text:Pair");
});

test("remote_control_settings_page_shares_and_invites_a_device", async () => {
  const config = process.env.CLAW_CONFIG ?? "";
  if (!config.endsWith("tests/fixture/config.toml")) {
    throw new Error(`run with CLAW_CONFIG=$PWD/tests/fixture/config.toml (got ${JSON.stringify(config)})`);
  }
  const app = await open("../App.tsx", DESKTOP);
  await app.waitFor("text:New session", { timeout: 20000 });
  await app.press("text:Settings");
  await app.waitUntil(() => app.window("settings").state()?.visible === true);
  await app.press("text:Remote control");
  await app.waitFor("label:Device name");
  await app.waitFor("text:No trusted devices yet.");
  // Headless runs have no app data directory, so sharing cannot start there; `--target linux` runs the whole flow.
  if (app.shows("text:The desktop data directory is unavailable.")) return;
  await app.press("label:Allow remote control");
  await app.waitFor("text:Add device", { timeout: 20000 });
  await app.press("text:Add device");
  // The invitation unfolds on the page: its QR code and a copyable one-use pairing code.
  await app.waitFor("label:Copy pairing code", { timeout: 20000 });
  await app.waitFor("text:Close");
  await app.press("text:Close");
  await app.waitUntil(() => !app.shows("label:Copy pairing code"));
  await app.press("label:Allow remote control");
  await app.waitUntil(() => !app.shows("text:Add device"), { timeout: 20000 });
}, 90000);

test("providers_list_the_real_backends_sign_in_methods", async () => {
  const config = process.env.CLAW_CONFIG ?? "";
  if (!config.endsWith("tests/fixture/config.toml")) {
    throw new Error(`run with CLAW_CONFIG=$PWD/tests/fixture/config.toml (got ${JSON.stringify(config)})`);
  }
  const app = await open("../App.tsx", DESKTOP);
  await app.waitFor("text:New session", { timeout: 20000 });
  await app.press("text:Settings");
  await app.waitUntil(() => app.window("settings").state()?.visible === true);
  await app.press("text:Providers");
  // The backend's own `initialize` reply, not a script: its method shape is what the page reads.
  await app.waitFor("text:ChatGPT", { timeout: 20000 });
  await app.waitFor("text:Not signed in", { timeout: 20000 });
});
