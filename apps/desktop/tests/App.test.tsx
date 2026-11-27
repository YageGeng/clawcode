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
