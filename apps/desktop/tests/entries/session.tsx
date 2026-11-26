// Test entry: the shell over a scripted host whose Sessions each answer a prompt their own way (composer tests).
import { mount } from "nsh";

import { Shell } from "../../src/shell/Shell";
import { createAppStore } from "../../src/stores/app";
import { createHostStore, LOCAL_HOST } from "../../src/stores/host";
import { createSettingsStore } from "../../src/stores/settings";
import { ScriptedAgent } from "../support/scriptedAgent";

/** The outcome a `session/request_permission` result carries. */
type PermissionResult = Readonly<{ outcome: Readonly<{ outcome: string; optionId?: string }> }>;

/**
 * "Echo" (and any new Session) echoes the prompt and its folder after 500 ms; "Slow" runs until stopped; "Broken"
 * fails the prompt; "Asks" asks for permission and says what it was told; "Crashes" loses the backend mid-turn. Each
 * reply states what the agent heard, so the tests read the agent's side from the screen.
 */
function Entry() {
  const agent = new ScriptedAgent([
    { sessionId: "s1", cwd: "/work/alpha", title: "Echo", modifiedAtMs: "4000" },
    { sessionId: "s2", cwd: "/work/alpha", title: "Slow", modifiedAtMs: "3000" },
    { sessionId: "s3", cwd: "/work/alpha", title: "Broken", modifiedAtMs: "2000" },
    { sessionId: "s4", cwd: "/work/alpha", title: "Asks", modifiedAtMs: "1000" },
    { sessionId: "s5", cwd: "/work/alpha", title: "Crashes", modifiedAtMs: "500" },
  ]);
  agent.onPrompt = (call) => {
    if (call.sessionId === "s3") return agent.fail(call.id, "model unavailable");
    /** Replies with `text` and ends the turn. */
    const reply = (text: string, stopReason = "end_turn") => {
      agent.push(agent.agentMessage(call.sessionId, `a-${call.id}`, text));
      agent.push(agent.state(call.sessionId, "idle"));
      agent.respond(call.id, { stopReason });
    };
    agent.push(agent.state(call.sessionId, "running"));
    agent.push(agent.userMessage(call.sessionId, `u-${call.id}`, call.text));
    if (call.sessionId === "s5") return agent.transport.end("crashed");
    if (call.sessionId === "s2") {
      agent.onCancel = () => reply("Stopped.", "cancelled");
      return;
    }
    if (call.sessionId === "s4") {
      void agent
        .requestPermission({
          sessionId: "s4",
          title: "Run cargo test?",
          options: [
            { optionId: "allow-once", name: "Allow once", kind: "allow_once" },
            { optionId: "reject-once", name: "Reject", kind: "reject_once" },
          ],
        })
        .then((result) => {
          const outcome = (result as PermissionResult).outcome;
          reply(`Chose ${outcome.optionId ?? outcome.outcome}`);
        });
      return;
    }
    const cwd = agent.sessions.find((session) => session.sessionId === call.sessionId)?.cwd;
    setTimeout(() => reply(`Echo ${call.text} from ${cwd}`), 500);
  };
  const store = createAppStore([createHostStore(LOCAL_HOST, agent.transport)]);
  void store.start();
  return <Shell store={store} settings={createSettingsStore()} />;
}

mount(Entry);
