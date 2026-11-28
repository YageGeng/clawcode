// A local host advertising two sign-in methods, one already signed in.
import { mountSession } from "./sessionFixture";

mountSession((agent) => {
  agent.authMethods.push(
    { id: "openai-codex", name: "ChatGPT", description: "Use your ChatGPT plan", status: "signedIn" },
    { id: "github-copilot", name: "GitHub Copilot", status: "signInRequired" },
  );
});
