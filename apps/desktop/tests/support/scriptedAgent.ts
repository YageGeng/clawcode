// Test support: a scripted ACP agent behind a MemoryTransport. It answers the
// calls a desktop host makes and pushes `session/update` frames in the shapes
// `src/client/workspace/updateDecoder.ts` reads, so tests run without a model.
import { MemoryTransport } from "./memoryTransport";

/** One `session/update` notification's params. */
export type UpdateParams = Readonly<{ sessionId: string; update: Record<string, unknown> }>;

/** A listed Session. */
/** A listed Session; `meta` adds product metadata such as `parentSessionId` or `ownership`. */
export type ScriptedSession = Readonly<{ sessionId: string; cwd: string; title?: string; modifiedAtMs?: string; meta?: Record<string, unknown> }>;

/** One frame the agent received: a request (with `id`) or a notification. */
export type Received = Readonly<{ method: string; params: Record<string, unknown>; id?: number | string }>;

/** What a prompt handler is given: the request and the agent to script the turn with. */
export type PromptCall = Readonly<{ id: number | string; sessionId: string; text: string }>;

/** A scripted agent: answers initialize, session/list, session/new, session/resume and the runtime query; prompts run `onPrompt`. */
export class ScriptedAgent {
  readonly sessions: ScriptedSession[];
  /** Notifications `session/resume` replays per Session before it answers. */
  readonly replays = new Map<string, UpdateParams[]>();
  /** Which Sessions the runtime query reports as running. */
  readonly running = new Set<string>();
  /** The sign-in methods `initialize` advertises, and each one's status for `_clawcode/auth/status`. */
  readonly authMethods: Array<{ id: string; name: string; description?: string; status?: string }> = [];
  /** `auth/login` shows this device code; with `completeAfterMs`, the sign-in then succeeds on its own. */
  signInCode: Readonly<{ userCode: string; completeAfterMs?: number }> | undefined;
  /** Each Session's trajectory records, as `_clawcode/session/trajectory` pages them (one page; `after` filters). */
  readonly trajectories = new Map<string, Array<{ sequence: number }>>();
  /** Request contents by hash, for `_clawcode/session/trajectory/content`. */
  readonly requestContents = new Map<string, string>();
  /** Every request and notification the client sent, in order. */
  readonly received: Received[] = [];
  /** Runs each `session/prompt`; by default replies with one message and ends the turn. */
  onPrompt: (call: PromptCall) => void = (call) => this.defaultTurn(call);
  /** Runs on each `session/cancel` notification, with its Session. */
  onCancel: (sessionId: string) => void = () => {};
  readonly transport: MemoryTransport;
  private sequence = 0;
  private clock = 1_700_000_000_000;
  private turn = 0;
  private requestId = 0;
  private readonly answers = new Map<string, (result: unknown) => void>();

  /** Creates the agent with its listed Sessions. */
  constructor(sessions: ScriptedSession[] = []) {
    this.sessions = [...sessions];
    this.transport = new MemoryTransport((frame) => this.receive(frame));
  }

  /** The params of a `session/update` with the product event meta the decoder needs. */
  update(sessionId: string, update: Record<string, unknown>, turnId = `turn-${this.turn}`): UpdateParams {
    this.sequence += 1;
    this.clock += 1000;
    return { sessionId, update: { ...update, _meta: { clawcode: { turnId, timestampMs: String(this.clock), sequence: this.sequence } } } };
  }

  /** A whole user message. */
  userMessage(sessionId: string, messageId: string, text: string): UpdateParams {
    this.turn += 1;
    return this.update(sessionId, { sessionUpdate: "user_message", messageId, content: [{ type: "text", text }] });
  }

  /** A whole assistant message. */
  agentMessage(sessionId: string, messageId: string, text: string): UpdateParams {
    return this.update(sessionId, { sessionUpdate: "agent_message", messageId, content: [{ type: "text", text }] });
  }

  /** One streamed piece of an assistant message. */
  agentChunk(sessionId: string, messageId: string, text: string): UpdateParams {
    const params = this.update(sessionId, { sessionUpdate: "agent_message_chunk", messageId, content: { type: "text", text } });
    // Chunks carry their meta on the content, as the backend sends them.
    const update = params.update as Record<string, unknown>;
    return { sessionId, update: { ...update, content: { type: "text", text, _meta: update._meta } } };
  }

  /** A tool call's state. */
  tool(sessionId: string, tool: Readonly<{ toolCallId: string; title: string; kind: string; status: string; content?: unknown[] }>): UpdateParams {
    return this.update(sessionId, { sessionUpdate: "tool_call_update", ...tool });
  }

  /** The Session's run state. */
  state(sessionId: string, state: "running" | "idle"): UpdateParams {
    return this.update(sessionId, { sessionUpdate: "state_update", state });
  }

  /** Sends one `session/update` notification. */
  push(params: UpdateParams): void {
    this.transport.deliver(JSON.stringify({ jsonrpc: "2.0", method: "session/update", params }));
  }

  /** Answers the client's request `id` with `result`. */
  respond(id: number | string, result: unknown): void {
    this.transport.deliver(JSON.stringify({ jsonrpc: "2.0", id, result }));
  }

  /** Fails the client's request `id` with `message`. */
  fail(id: number | string, message: string): void {
    this.transport.deliver(JSON.stringify({ jsonrpc: "2.0", id, error: { code: -32603, message } }));
  }

  /** Asks the client for permission; resolves with the client's result. Its request id is returned through `onSent`. */
  requestPermission(params: Record<string, unknown>, onSent?: (id: string) => void): Promise<unknown> {
    const id = `agent-${++this.requestId}`;
    const answer = new Promise<unknown>((resolve) => this.answers.set(id, resolve));
    this.transport.deliver(JSON.stringify({ jsonrpc: "2.0", id, method: "session/request_permission", params }));
    onSent?.(id);
    return answer;
  }

  /** Withdraws the agent's request `id`, as when another client answered it. */
  withdraw(id: string): void {
    this.transport.deliver(JSON.stringify({ jsonrpc: "2.0", method: "$/cancel_request", params: { requestId: id } }));
  }

  /** The requests and notifications the client sent with `method`. */
  calls(method: string): Received[] {
    return this.received.filter((frame) => frame.method === method);
  }

  /** Replies with "Done." in one message, then ends the turn. */
  private defaultTurn(call: PromptCall): void {
    this.push(this.state(call.sessionId, "running"));
    this.push(this.userMessage(call.sessionId, `u-${call.id}`, call.text));
    this.push(this.agentMessage(call.sessionId, `a-${call.id}`, "Done."));
    this.push(this.state(call.sessionId, "idle"));
    this.respond(call.id, { stopReason: "end_turn" });
  }

  /** The replayed messages of `sessionId` as `_clawcode/session/tree` entries, each the child of the one before. */
  treeOf(sessionId: string): Array<{ entryId: string; parentId: string | null; kind: string; payload: Record<string, unknown> }> {
    const entries: Array<{ entryId: string; parentId: string | null; kind: string; payload: Record<string, unknown> }> = [];
    for (const { update } of this.replays.get(sessionId) ?? []) {
      const role = update.sessionUpdate === "user_message" ? "user" : update.sessionUpdate === "agent_message" ? "assistant" : undefined;
      if (role === undefined || entries.some((entry) => entry.payload.message_id === update.messageId)) continue;
      entries.push({ entryId: `entry-${String(update.messageId)}`, parentId: entries.at(-1)?.entryId ?? null, kind: "message",
        payload: { message_id: update.messageId, content: { type: role } } });
    }
    return entries;
  }

  /** The replay of `sessionId` up to and including entry `entryId`'s message; before the first message for the root. */
  private replayUpTo(sessionId: string, entryId: unknown): UpdateParams[] {
    const updates = this.replays.get(sessionId) ?? [];
    const end = updates.findIndex(({ update }) => (entryId === null
      ? update.sessionUpdate === "user_message" || update.sessionUpdate === "agent_message"
      : `entry-${String(update.messageId)}` === entryId));
    return end < 0 ? [...updates] : updates.slice(0, entryId === null ? end : end + 1);
  }

  /** Handles one frame from the client. */
  private receive(frame: string): void {
    const message = JSON.parse(frame) as { id?: number | string; method?: string; params?: Record<string, unknown>; result?: unknown };
    if (message.method === undefined) {
      // A response to one of the agent's own requests.
      if (message.id !== undefined) this.answers.get(String(message.id))?.(message.result);
      return;
    }
    const params = message.params ?? {};
    this.received.push({ method: message.method, params, ...(message.id === undefined ? {} : { id: message.id }) });
    const id = message.id;
    if (id === undefined) {
      if (message.method === "session/cancel") this.onCancel(String(params.sessionId));
      return;
    }
    switch (message.method) {
      case "initialize":
        return this.respond(id, { protocolVersion: 2, capabilities: {},
          authMethods: this.authMethods.map(({ id: method, name, description }) => ({ type: "agent", methodId: method, name, description })) });
      case "auth/login": {
        const code = this.signInCode;
        if (code === undefined) return this.respond(id, {});
        const elicitation = `agent-${++this.requestId}`;
        this.transport.deliver(JSON.stringify({ jsonrpc: "2.0", id: elicitation, method: "elicitation/create", params: {
          mode: "url", requestId: id, elicitationId: elicitation, url: "https://example.com/device",
          message: `Open the page and enter ${code.userCode}`, _meta: { clawcode: { userCode: code.userCode } },
        } }));
        if (code.completeAfterMs !== undefined) {
          setTimeout(() => {
            const method = this.authMethods.find((item) => item.id === params.methodId);
            if (method) method.status = "signedIn";
            this.transport.deliver(JSON.stringify({ jsonrpc: "2.0", method: "$/cancel_request", params: { requestId: elicitation } }));
            this.respond(id, {});
          }, code.completeAfterMs);
        }
        return;
      }
      case "_clawcode/auth/status":
        return this.respond(id, this.authMethods.map(({ id: method, status }) => ({ methodId: method, status: status ?? "unknown" })));
      case "session/list":
        return this.respond(id, {
          sessions: this.sessions.map((session) => ({
            sessionId: session.sessionId,
            cwd: session.cwd,
            title: session.title ?? null,
            ...(session.modifiedAtMs === undefined && session.meta === undefined ? {} : {
              _meta: { clawcode: { ...session.meta, ...(session.modifiedAtMs === undefined ? {} : { modifiedAtMs: session.modifiedAtMs }) } },
            }),
          })),
        });
      case "session/new": {
        const sessionId = `new-${this.sessions.length + 1}`;
        this.sessions.push({ sessionId, cwd: String(params.cwd) });
        return this.respond(id, { sessionId });
      }
      case "session/resume":
        for (const update of this.replays.get(String(params.sessionId)) ?? []) this.push(update);
        return this.respond(id, null);
      case "session/set_title": {
        const index = this.sessions.findIndex((session) => session.sessionId === params.sessionId);
        if (index >= 0) this.sessions[index] = { ...this.sessions[index]!, title: String(params.title) };
        return this.respond(id, { title: params.title });
      }
      case "session/delete": {
        const index = this.sessions.findIndex((session) => session.sessionId === params.sessionId);
        if (index >= 0) this.sessions.splice(index, 1);
        return this.respond(id, {});
      }
      case "_clawcode/session/tree":
        return this.respond(id, { sessionId: params.sessionId, lane: "main", entries: this.treeOf(String(params.sessionId)) });
      case "_clawcode/session/branch": {
        // The new branch ends at the target entry; the backend then tells clients to replay it.
        const sessionId = String(params.sessionId);
        this.replays.set(sessionId, this.replayUpTo(sessionId, params.entryId));
        this.push(this.update(sessionId, { sessionUpdate: "_clawcode/event", payload: { event: "branch_navigated" } }));
        return this.respond(id, { sessionId, lane: "main", entries: this.treeOf(sessionId) });
      }
      case "_clawcode/session/fork": {
        const source = String(params.sessionId);
        const sessionId = `fork-${this.sessions.length + 1}`;
        const title = this.sessions.find((session) => session.sessionId === source)?.title;
        this.sessions.push({ sessionId, cwd: String(params.cwd), ...(title === undefined ? {} : { title: `${title} (fork)` }),
          meta: { parentSessionId: source } });
        this.replays.set(sessionId, this.replayUpTo(source, params.entryId).map(({ update }) => ({ sessionId, update })));
        return this.respond(id, { sessionId });
      }
      case "_clawcode/session/trajectory": {
        const records = this.trajectories.get(String(params.sessionId));
        if (records === undefined) {
          return this.transport.deliver(JSON.stringify({ jsonrpc: "2.0", id, error: { code: -32603, message: "Internal error",
            data: { reason: "historyUnavailable", message: "session history is unavailable" } } }));
        }
        const page = typeof params.after === "number" ? records.filter((record) => record.sequence > (params.after as number)) : records;
        const sequences = records.map((record) => record.sequence);
        return this.respond(id, { records: page, hasMoreBefore: false, hasMoreAfter: false,
          ...(sequences.length ? { firstSequence: Math.min(...sequences), lastSequence: Math.max(...sequences) } : {}) });
      }
      case "_clawcode/session/trajectory/content": {
        const content = this.requestContents.get(String(params.hash));
        return this.respond(id, content === undefined ? null : { hash: params.hash, kind: "system_prompt", content });
      }
      case "_clawcode/session/runtime":
        return this.respond(id, { running: this.running.has(String(params.sessionId)) });
      case "_clawcode/session/pending_messages":
        return this.respond(id, { reservedQuestions: [] });
      case "_clawcode/session/question/dismiss":
        return this.respond(id, { skipped: params.items });
      case "session/prompt": {
        const prompt = Array.isArray(params.prompt) ? (params.prompt as Array<{ text?: string }>) : [];
        return this.onPrompt({ id, sessionId: String(params.sessionId), text: prompt.map((block) => block.text ?? "").join("") });
      }
      default:
        return this.respond(id, null);
    }
  }
}
