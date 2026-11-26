// Test entry: 60 ended turns, then new answers at 1 s and 3 s (move the test's manual clock).
import { mountSession } from "./sessionFixture";

mountSession(
  (agent) => {
    const frames = [];
    for (let index = 1; index <= 60; index++) {
      frames.push(agent.userMessage("s1", `u${index}`, `Question ${index}`));
      frames.push(agent.agentMessage("s1", `a${index}`, `Answer ${index}`));
    }
    agent.replays.set("s1", frames);
  },
  (agent) => {
    setTimeout(() => {
      agent.push(agent.userMessage("s1", "u61", "Question 61"));
      agent.push(agent.agentMessage("s1", "a61", "Tail one"));
    }, 1000);
    setTimeout(() => {
      agent.push(agent.userMessage("s1", "u62", "Question 62"));
      agent.push(agent.agentMessage("s1", "a62", "Tail two"));
    }, 3000);
  },
);
