// Test support: trajectory records in the shape `_clawcode/session/trajectory` sends them.

/** A usage record as the host writes it. */
export function usage(input: number, output: number, cached = 0) {
  return { format: "rig-core-v1", usage: { input_tokens: input, output_tokens: output, cached_input_tokens: cached } };
}

/** Wire records of one Run: a prompt, two requests (the first with two tool calls), and the answer. */
export function wireRun(run: number, base: number, sequence: number) {
  const ref = { id: `run-${run}`, index: run };
  const at = (offset: number) => String(base + offset);
  const options = { providerId: "p", modelId: "m", thinkingLevel: "low" };
  const snapshot = { systemPrompt: "a".repeat(64), tools: "b".repeat(64), options, messageCount: 1 };
  return [
    { id: `u${run}`, sequence, run: ref, startedAtMs: at(0), endedAtMs: at(0), kind: { type: "user", text: "Fix it", imageCount: 0, fileCount: 0, origin: "input" } },
    { id: `request:e${run}a`, sequence: sequence + 1, run: ref, startedAtMs: at(10), endedAtMs: at(1000),
      kind: { type: "request", index: run * 2 - 1, snapshot, first: run === 1, promptChanged: false, toolsAdded: [], toolsRemoved: [] } },
    { id: `e${run}a`, sequence: sequence + 1, run: ref, startedAtMs: at(10), endedAtMs: at(1000), kind: {
      type: "assistant", requestIndex: run * 2 - 1, providerId: "p", modelId: "m", reasoning: "", text: "", toolCallIds: ["c1", "c2"],
      usage: usage(1000, 50, 800), requestedAtMs: at(10), firstTokenAtMs: at(400), lastTokenAtMs: at(1000), stopReason: "tool_use", cancelled: false } },
    { id: `tool:c1-${run}`, sequence: sequence + 1, run: ref, startedAtMs: at(1000), endedAtMs: at(1500),
      kind: { type: "tool", callId: "c1", name: "read", arguments: { path: "a.rs" }, resultText: "ok", imageCount: 0, isError: false, cancelled: false } },
    { id: `tool:c2-${run}`, sequence: sequence + 1, run: ref, startedAtMs: at(1000), endedAtMs: at(1200),
      kind: { type: "tool", callId: "c2", name: "exec_command", arguments: {}, resultText: "boom", imageCount: 0, isError: true, cancelled: false } },
    { id: `request:e${run}b`, sequence: sequence + 4, run: ref, startedAtMs: at(1600), endedAtMs: at(2000),
      kind: { type: "request", index: run * 2, snapshot, first: false, promptChanged: false, toolsAdded: ["edit"], toolsRemoved: [] } },
    { id: `e${run}b`, sequence: sequence + 4, run: ref, startedAtMs: at(1600), endedAtMs: at(2000), kind: {
      type: "assistant", requestIndex: run * 2, providerId: "p", modelId: "m", reasoning: "", text: "Done", toolCallIds: [],
      usage: usage(1100, 20, 1000), requestedAtMs: at(1600), firstTokenAtMs: at(1700), lastTokenAtMs: at(2000), stopReason: "end_turn", cancelled: false } },
  ];
}
