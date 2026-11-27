import { open, test } from "nsh/test";

test("escape_returns_to_the_previous_view_before_dismissing_a_controlled_dialog", async () => {
  const app = await open("../entries/remote-dismissal.tsx", { platform: "linux" });
  await app.press("text:Open devices");
  await app.press("text:Open invitation");
  await app.waitFor("text:Invitation");
  await app.keyboard.press("Escape");
  await app.waitFor("text:Open invitation");
  await app.keyboard.press("Escape");
  await app.waitUntil(() => app.count("text:Open invitation") === 0);
});
