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
        return this.respond(id, { protocolVersion: 2, capabilities: {} });
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
