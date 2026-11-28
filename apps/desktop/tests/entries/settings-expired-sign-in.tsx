// A local host whose ChatGPT credential looks valid until the model answers 401: the Run fails after it started.
import { mountSession } from "./sessionFixture";

mountSession((agent) => {
  agent.authMethods.push({ id: "chatgpt", name: "ChatGPT", status: "signedIn" });
  agent.signInCode = { userCode: "WXYZ-9876", completeAfterMs: 1500 };
  agent.onPrompt = (call) => {
    agent.push(agent.state(call.sessionId, "running"));
    agent.push(agent.userMessage(call.sessionId, `u-${call.id}`, call.text));
    agent.respond(call.id, { stopReason: "end_turn" });
    // The provider rejected the credential mid-stream: the Run settles with an error naming the sign-in.
    agent.push(agent.update(call.sessionId, {
      sessionUpdate: "state_update", state: "idle", stopReason: "error",
      error: { code: -32000, message: "provider request failed: provider_response (HTTP 401)", data: { methodId: "chatgpt" } },
    }));
  };
});
