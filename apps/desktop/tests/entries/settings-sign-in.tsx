// A local host whose Copilot sign-in shows a device code; "s1" refuses prompts until the user signs in.
import { mountSession } from "./sessionFixture";

mountSession((agent) => {
  agent.authMethods.push({ id: "github-copilot", name: "GitHub Copilot", status: "signInRequired" });
  agent.signInCode = { userCode: "ABCD-1234", completeAfterMs: 1500 };
  agent.onPrompt = (call) => {
    agent.transport.deliver(JSON.stringify({ jsonrpc: "2.0", id: call.id,
      error: { code: -32000, message: "Authentication required", data: { methodId: "github-copilot" } } }));
  };
});
