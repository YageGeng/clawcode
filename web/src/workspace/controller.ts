import { AcpConnection, AcpRequestError } from "../acp/connection";
import type { AcpNotification } from "../acp/connection";
import { AcpMethods, AcpNotifications } from "../acp/extensions";
import type { AcpExtensionMethods, AcpExtensionNotifications } from "../acp/extensions";
import { AcpProtocol } from "../acp/protocol";
import type { InitializeRecovery, InitializeResult, NewSessionResult, PromptContentBlock, SessionId, SessionInfo, SessionListResult, SessionRecoveryPlan, SessionUpdateNotification, TimestampMs } from "../acp/protocol";
import type { UiBootstrap } from "../bootstrap/model";
import type { BranchEditPlan } from "../domain/messageActions";
import type { ElicitationAnswer, ElicitationPrompt, McpCompletionResult, McpPromptResult, McpResourceResult, McpSessionSnapshot, PendingMessages, PromptInput, SessionRuntimeSnapshot, SessionSummary, SessionTree, SkillListResult } from "../domain/model";
import { ElicitationForm, UnrenderableSchema } from "../features/mcp/elicitationForm";
import { useWorkspaceStore } from "./store";
import type { SessionWorkspaceAction } from "./sessionState";
import { sessionWorkspace } from "./state";
import type { WorkspaceAction } from "./state";
import { SessionUpdateRouter } from "./updateRouter";
import type { WorkspaceStoreAccess } from "./updateRouter";

const RECONNECT_DELAYS = [250, 500, 1_000, 2_000, 5_000] as const;
const HEARTBEAT_INTERVAL_MS = 5_000;
const HEARTBEAT_IDLE_MS = 20_000;
const HEARTBEAT_TIMEOUT_MS = 5_000;

type Submission = {
  seq: number;
  submissionId: string;
  input: PromptInput;
  state: "sent" | "queued" | "disconnected" | "orphaned" | "unknown";
};

export class WorkspaceController {
  readonly bootstrap: UiBootstrap;
  readonly methods: AcpExtensionMethods;
  readonly notifications: AcpExtensionNotifications;
  private readonly updateRouter: SessionUpdateRouter;
  private connection: AcpConnection | undefined;
  private submissionSequence = 0;
  private readonly submissions = new Map<SessionId, Map<string, Submission>>();
  private readonly returnedInputs = new Map<SessionId, { items: { submission: Submission; error?: string }[]; timer: number }>();
  private connectionGeneration = 0;
  private connectPromise: Promise<void> | undefined;
  private reconnectTimer: number | undefined;
  private heartbeatTimer: number | undefined;
  private heartbeatInFlight = false;
  private lastActivityAt = 0;
  private reconnectAttempt = 0;
  private sessionDeleteSupported = false;
  private readonly deletingSessionIds = new Set<SessionId>();
  private readonly attachedSessionIds = new Set<SessionId>();
  private readonly attachingSessions = new Map<SessionId, Promise<void>>();
  // Resolvers for queued elicitation dialogs, keyed like their store prompts.
  private readonly elicitationAnswers = new Map<string, (answer: ElicitationAnswer) => void>();
  private elicitationSequence = 0;
  private sessionRefresh: Readonly<{ connection: AcpConnection; promise: Promise<void> }> | undefined;
  private listenersInstalled = false;
  private stopped = false;
  private readonly onlineListener = () => this.wakeConnection();
  /** Applies browser back and forward navigation to the selected Session. */
  private readonly popStateListener = () => {
    void this.restoreSessionFromUrl();
  };
  private readonly visibilityListener = () => {
    if (document.visibilityState === "visible") this.wakeConnection();
  };

  constructor(bootstrap: UiBootstrap) {
    this.bootstrap = bootstrap;
    this.methods = AcpMethods.forNamespace(bootstrap.product.slug);
    this.notifications = AcpNotifications.forNamespace(bootstrap.product.slug);
    const store: WorkspaceStoreAccess = {
      getState: () => useWorkspaceStore.getState(),
      dispatch: (action) => this.dispatch(action)
    };
    this.updateRouter = new SessionUpdateRouter(
      bootstrap.product.slug,
      store,
      (sessionId) => this.refreshSessionRuntime(sessionId)
    );
  }

  async start(): Promise<void> {
    this.stopped = false;
    this.installNetworkListeners();
    await this.ensureConnected(false);
  }

  /** Opens one listed Session and optionally records the selection in browser history. */
  async openSession(sessionId: SessionId, updateUrl = true): Promise<void> {
    await this.attachSession(sessionId);
    this.dispatch({ type: "session/activated", sessionId });
    if (updateUrl) this.pushSessionUrl(sessionId);
  }

  /** Attaches an additional Session projection without changing selection or browser history. */
  async attachSession(sessionId: SessionId): Promise<void> {
    const pending = this.attachingSessions.get(sessionId);
    if (pending !== undefined) return pending;
    const attaching = this.loadAttachedSession(sessionId);
    this.attachingSessions.set(sessionId, attaching);
    try { await attaching; }
    finally { this.attachingSessions.delete(sessionId); }
  }

  /** Coalesces child panels sharing one Session and installs its replay watcher exactly once. */
  private async loadAttachedSession(sessionId: SessionId): Promise<void> {
    await this.ensureSessionListed(sessionId);
    const state = useWorkspaceStore.getState();
    const session = state.sessions.find((item) => item.sessionId === sessionId);
    if (session === undefined) throw new Error("Session is not in the current list");
    const wasLoaded = this.attachedSessionIds.has(sessionId);
    const connection = this.requireConnection();
    const runtime = await connection.request<SessionRuntimeSnapshot>(
      this.methods.sessionRuntime,
      { sessionId }
    );
    this.dispatchSession(sessionId, { type: "running/changed", running: runtime.running });

    if (!wasLoaded || !runtime.running) {
      // Idle sessions are replayed on every activation because another ACP
      // client may have changed the persisted transcript while this tab slept.
      this.updateRouter.requireFullReplay(sessionId);
      this.dispatchSession(sessionId, { type: "transcript/cleared" });
      await this.resume(connection, sessionId, { sessionId, cwd: session.cwd, replayFrom: { type: "start" } });
      this.dispatchSession(sessionId, { type: "outcome/unknown", value: [...(this.submissions.get(sessionId)?.values() ?? [])].some((record) => record.state === "unknown") });
      this.attachedSessionIds.add(sessionId);
    }

    const [tree, pending, skills, mcpSnapshot] = await Promise.all([
      connection.request<SessionTree>(this.methods.tree, { sessionId }),
      connection.request<PendingMessages>(this.methods.pendingMessages, { sessionId }),
      connection.request<SkillListResult>(this.methods.skillList, { sessionId }),
      connection.request<McpSessionSnapshot>(this.methods.mcpStatus, { sessionId })
    ]);
    this.dispatchSession(sessionId, { type: "tree/replaced", tree });
    this.dispatchSession(sessionId, { type: "queue/replaced", pending });
    this.dispatchSession(sessionId, { type: "skills/replaced", result: skills });
    this.dispatchSession(sessionId, { type: "mcp/replaced", snapshot: mcpSnapshot });
  }

  async newSession(cwd: string): Promise<SessionId> {
    const result = await this.requireConnection().request<NewSessionResult>(AcpProtocol.methods.sessionNew, { cwd });
    await this.refreshSessions(true);
    await this.openSession(result.sessionId);
    return result.sessionId;
  }

  async renameSession(sessionId: SessionId, title: string): Promise<void> {
    const result = await this.requireConnection().request<Readonly<{ title: string }>>(
      this.methods.sessionRename,
      { sessionId, title }
    );
    this.dispatch({ type: "session/title", sessionId, title: result.title });
  }

  /** Permanently removes a session only after ACP v2 advertised native deletion support. */
  async deleteSession(sessionId: SessionId): Promise<void> {
    if (!this.sessionDeleteSupported) throw new Error("Agent does not support session deletion");
    // MCP shutdown publishes final revisions while deletion is in flight. Mark
    // the Session first so those notifications cannot query an already removed runtime.
    const deleting = new Set<SessionId>([sessionId]);
    const sessions = useWorkspaceStore.getState().sessions;
    for (const id of deleting) {
      for (const child of sessions) if (child.ownership?.sessionId === id) deleting.add(child.sessionId);
    }
    for (const id of deleting) this.deletingSessionIds.add(id);
    try {
      await this.requireConnection().request(AcpProtocol.methods.sessionDelete, { sessionId });
      const active = useWorkspaceStore.getState().activeSessionId;
      if (active !== undefined && deleting.has(active)) {
        this.dispatch({ type: "session/deactivated" });
        this.replaceRootUrl();
      }
      for (const id of deleting) {
        this.updateRouter.resetRecovery(id);
        this.attachedSessionIds.delete(id);
        this.submissions.delete(id);
        const returned = this.returnedInputs.get(id);
        if (returned !== undefined) window.clearTimeout(returned.timer);
        this.returnedInputs.delete(id);
      }
      await this.refreshSessions(true);
    } finally {
      for (const id of deleting) this.deletingSessionIds.delete(id);
    }
  }

  /** Sends immediately; this controller owns every eventual response and original input. */
  submit(input: PromptInput, options?: { alternate: boolean }): void {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    if (input.text.trim().length === 0 && input.resources.length === 0 && input.images.length === 0) throw new Error("Prompt is empty");
    const connection = this.requireConnection();
    const prompt: PromptContentBlock[] = [
      ...(input.text.length === 0 ? [] : [{ type: "text" as const, text: input.text }]),
      ...input.resources.map((resource) => ({ type: "resource_link" as const, name: resource.name, uri: resource.uri })),
      ...input.images.map((image) => ({ type: "image" as const, data: image.data, mimeType: image.mimeType }))
    ];
    const submission: Submission = { seq: ++this.submissionSequence, submissionId: crypto.randomUUID(), input, state: "sent" };
    let records = this.submissions.get(sessionId);
    if (records === undefined) { records = new Map(); this.submissions.set(sessionId, records); }
    records.set(submission.submissionId, submission);
    const whenBusy = this.bootstrap.whenBusy === "steer" ? "followUp" : "steer";
    void connection.request(AcpProtocol.methods.sessionPrompt, {
      sessionId, prompt,
      _meta: { [this.bootstrap.product.slug]: { submissionId: submission.submissionId, ...(options?.alternate === true ? { whenBusy } : {}) } }
    }).then(() => {
      records.delete(submission.submissionId);
    }).catch((reason: unknown) => {
      if (!this.submissions.get(sessionId)?.has(submission.submissionId)) return;
      if (!(reason instanceof AcpRequestError)) {
        // Transport loss cannot establish whether the request reached or entered history.
        submission.state = "disconnected";
        return;
      }
      const data = reason.rpc.data;
      if (typeof data === "object" && data !== null && "outcomeUnknown" in data && data.outcomeUnknown === true) {
        this.markUnknown(sessionId, submission, "会话持久化异常，请关闭并重新打开会话");
        return;
      }
      records.delete(submission.submissionId);
      this.returnInput(sessionId, submission, reason.rpc.code === -32800 ? undefined : reason.message);
    });
  }

  /** Reconciles only authoritative queue snapshots, never inferred message history. */
  private reconcileSubmissions(sessionId: SessionId, pending: PendingMessages, reconnected = false): void {
    const records = this.submissions.get(sessionId);
    if (records === undefined) return;
    const queued = new Set([...pending.steering, ...pending.followUp].map((item) => item.submissionId));
    for (const record of records.values()) {
      if (record.state === "unknown") continue;
      if (record.state === "disconnected") {
        if (!reconnected) continue;
        if (queued.has(record.submissionId)) record.state = "orphaned";
        else this.markUnknown(sessionId, record, "请求结果未知，原始输入已保留，系统不会自动重发");
      } else if (record.state === "orphaned") {
        if (!queued.has(record.submissionId)) records.delete(record.submissionId);
      } else if (queued.has(record.submissionId)) record.state = "queued";
    }
  }

  /** Keeps uncertain input separate from returned drafts until the user retrieves it. */
  private markUnknown(sessionId: SessionId, submission: Submission, message: string): void {
    submission.state = "unknown";
    this.dispatchSession(sessionId, { type: "outcome/unknown", value: true });
    this.dispatch({ type: "notice/added", notice: {
      id: `unknown-${submission.submissionId}`, sessionId, title: message,
      description: [submission.input.text, `${submission.input.images.length} 张图片 · ${submission.input.resources.length} 个资源`].filter(Boolean).join("\n")
    } });
  }

  /** Explicitly retrieves uncertain inputs for inspection without sending them. */
  restoreUnknownInput(sessionId: SessionId): void {
    const records = this.submissions.get(sessionId);
    if (records === undefined) return;
    for (const record of records.values()) {
      if (record.state !== "unknown") continue;
      records.delete(record.submissionId);
      this.returnInput(sessionId, record, "已取回结果未知的输入，请核对历史后再决定是否发送");
    }
    this.dispatchSession(sessionId, { type: "outcome/unknown", value: [...(this.submissions.get(sessionId)?.values() ?? [])].some((record) => record.state === "unknown") });
  }

  /** Coalesces prompt rejections before prepending them to the latest, Session-scoped draft. */
  private returnInput(sessionId: SessionId, submission: Submission, error?: string): void {
    const item = { submission, ...(error === undefined ? {} : { error }) };
    const existing = this.returnedInputs.get(sessionId);
    if (existing !== undefined) { existing.items.push(item); return; }
    const items = [item];
    // shortcut: responses more than 50 ms apart form separate batches; widen only with evidence of reordered delivery.
    const timer = window.setTimeout(() => {
      this.returnedInputs.delete(sessionId);
      items.sort((left, right) => left.submission.seq - right.submission.seq);
      const current = useWorkspaceStore.getState().drafts.get(sessionId) ?? { text: "", resources: [], images: [] };
      const inputs = [...items.map((item) => item.submission.input), current];
      const draft = {
        text: inputs.map((input) => input.text).filter((text) => text.trim().length > 0).join("\n\n"),
        resources: inputs.flatMap((input) => input.resources), images: inputs.flatMap((input) => input.images)
      };
      this.dispatch({ type: "draft/activated", sessionId, draft });
      sessionStorage.setItem(`${this.bootstrap.product.slug}:agent-draft:${sessionId}`, JSON.stringify({ text: draft.text, resources: draft.resources }));
      const errors = items.flatMap((item) => item.error === undefined ? [] : [item.error]);
      this.dispatch({ type: "notice/added", notice: { id: `returned-${submission.submissionId}`, sessionId,
        title: errors.length === 0 ? `已停止，${items.length} 条未执行的消息已放回输入框` : "输入已放回输入框",
        ...(errors.length === 0 ? {} : { description: errors.join("\n") }) } });
    }, 50);
    this.returnedInputs.set(sessionId, { items, timer });
  }

  /** Moves a pending follow-up into steering; false means the consumer already claimed it. */
  async promotePending(queueId: string): Promise<boolean> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    const result = await this.requireConnection().request<{ promoted: boolean }>(this.methods.pendingMessagePromote, { sessionId, queueId });
    return result.promoted;
  }

  async removePending(queueId: string): Promise<void> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    await this.requireConnection().request(this.methods.pendingMessageRemove, { sessionId, queueId });
  }

  async clearPending(): Promise<void> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    await this.requireConnection().request(this.methods.clearQueue, { sessionId });
  }

  async navigate(entryId: string | null): Promise<void> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    await this.requireConnection().request(this.methods.navigate, { sessionId, entryId });
    this.dispatchSession(sessionId, { type: "transcript/cleared" });
    await this.openSession(sessionId);
  }

  async branch(entryId: string | null): Promise<void> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    await this.requireConnection().request(this.methods.branch, { sessionId, entryId });
    this.dispatchSession(sessionId, { type: "transcript/cleared" });
    await this.openSession(sessionId);
  }

  async fork(entryId: string, cwd: string): Promise<SessionId> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    const result = await this.requireConnection().request<NewSessionResult>(this.methods.fork, { sessionId, entryId, cwd });
    await this.refreshSessions(true);
    await this.openSession(result.sessionId);
    return result.sessionId;
  }

  /** Branches before one user message and seeds its original prompt in the same Session. */
  async branchForEditing(plan: BranchEditPlan): Promise<void> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    await this.requireConnection().request(this.methods.branch, {
      sessionId,
      entryId: plan.branchEntryId
    });
    this.dispatch({ type: "draft/activated", sessionId, draft: plan.draft });
    this.dispatchSession(sessionId, { type: "transcript/cleared" });
    await this.openSession(sessionId);
  }

  /** Keeps one complete in-memory Composer draft across Session switches. */
  updateDraft(sessionId: SessionId, draft: PromptInput): void {
    this.dispatch({ type: "draft/changed", sessionId, draft });
  }

  /** Removes a submitted Composer draft from Session-scoped memory. */
  clearDraft(sessionId: SessionId): void {
    this.dispatch({ type: "draft/cleared", sessionId });
  }

  /** Cancels the active Session or an explicitly selected background Session. */
  cancel(targetSessionId?: SessionId): void {
    const sessionId = targetSessionId ?? useWorkspaceStore.getState().activeSessionId;
    if (sessionId !== undefined) {
      this.requireConnection().notify(AcpProtocol.methods.sessionCancel, { sessionId });
    }
  }

  async reconnect(): Promise<void> {
    const previousAttempt = this.connectPromise;
    this.clearReconnectTimer();
    this.stopHeartbeat();
    // A replacement connection replays any group that was not committed by
    // the current generation, so only its incomplete transport state is stale.
    this.updateRouter.discardPendingRecovery();
    this.connectionGeneration += 1;
    this.connection?.close();
    this.connection = undefined;
    this.reconnectAttempt = 1;
    if (previousAttempt !== undefined) await previousAttempt;
    await this.ensureConnected(true);
  }

  /** Explicitly reconnects one MCP Server and replaces the atomic Session snapshot. */
  async reconnectMcp(serverId: string): Promise<void> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    const snapshot = await this.requireConnection().request<McpSessionSnapshot>(
      this.methods.mcpReconnect,
      { sessionId, serverId }
    );
    this.dispatchSession(sessionId, { type: "mcp/replaced", snapshot });
  }

  /** Completes a pending MCP OAuth browser round and replaces the atomic snapshot. */
  async continueMcpOAuth(serverId: string, responseUri: string): Promise<void> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    const snapshot = await this.requireConnection().request<McpSessionSnapshot>(
      this.methods.mcpOAuthContinue,
      { sessionId, serverId, result: { responseUri } }
    );
    this.dispatchSession(sessionId, { type: "mcp/replaced", snapshot });
  }

  /** Changes one Session config option; the response carries every option's new value. */
  async setConfigOption(sessionId: SessionId, configId: string, value: string): Promise<void> {
    const result = await this.requireConnection().request<Readonly<{ configOptions?: unknown }>>(
      AcpProtocol.methods.sessionSetConfigOption,
      { sessionId, configId, type: "id", value }
    );
    const options = AcpProtocol.configOptions(result.configOptions);
    if (options !== undefined) this.dispatchSession(sessionId, { type: "config/replaced", options });
  }

  /** Attaches through session/resume and keeps the config options its response carries. */
  private async resume(connection: AcpConnection, sessionId: SessionId, params: Readonly<Record<string, unknown>>): Promise<void> {
    const result = await connection.request<Readonly<{ configOptions?: unknown }> | null>(AcpProtocol.methods.sessionResume, params);
    const options = AcpProtocol.configOptions(result?.configOptions);
    if (options !== undefined) this.dispatchSession(sessionId, { type: "config/replaced", options });
  }

  /** Sends the user's answer for one queued elicitation and closes its dialog. */
  answerElicitation(key: string, answer: ElicitationAnswer): void {
    const resolve = this.elicitationAnswers.get(key);
    this.elicitationAnswers.delete(key);
    this.dispatch({ type: "elicitation/settled", key });
    resolve?.(answer);
  }

  /** Queues one agent `elicitation/create` and resolves with the user's answer. */
  private async elicit(params: unknown, signal: AbortSignal): Promise<ElicitationAnswer> {
    const request = AcpProtocol.decodeRecord(params, "elicitation/create params");
    const invalid = (message: string) => new AcpRequestError({ code: -32602, message });
    if (typeof request.sessionId !== "string" || typeof request.message !== "string") {
      throw invalid("elicitation/create needs a Session scope and a message");
    }
    let mode: ElicitationPrompt["mode"];
    if (request.mode === "form") {
      try {
        ElicitationForm.parse(request.requestedSchema);
      } catch (reason: unknown) {
        // The agent learns the form cannot be shown instead of waiting on it.
        if (reason instanceof UnrenderableSchema) throw invalid(reason.message);
        throw reason;
      }
      mode = { type: "form", requestedSchema: request.requestedSchema };
    } else if (request.mode === "url" && typeof request.url === "string" && URL.canParse(request.url)) {
      mode = { type: "url", url: request.url };
    } else {
      throw invalid(`unsupported elicitation mode: ${String(request.mode)}`);
    }
    const product = AcpProtocol.productMeta(request._meta, this.bootstrap.product.slug);
    const key = `elicitation-${++this.elicitationSequence}`;
    const prompt: ElicitationPrompt = {
      key,
      sessionId: request.sessionId as SessionId,
      ...(typeof product?.mcpServerId === "string" ? { serverId: product.mcpServerId } : {}),
      message: request.message,
      mode
    };
    return new Promise<ElicitationAnswer>((resolve, reject) => {
      this.elicitationAnswers.set(key, resolve);
      // Answered elsewhere, or the connection closed: withdraw the dialog.
      signal.addEventListener("abort", () => {
        if (!this.elicitationAnswers.delete(key)) return;
        this.dispatch({ type: "elicitation/settled", key });
        reject(new AcpRequestError({ code: -32800, message: "Request cancelled" }));
      }, { once: true });
      this.dispatch({ type: "elicitation/asked", prompt });
    });
  }

  /** Retrieves one exact MCP Prompt without injecting it automatically. */
  async getMcpPrompt(
    serverId: string,
    remoteName: string,
    arguments_: Readonly<Record<string, string>>
  ): Promise<McpPromptResult> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    return this.requireConnection().request<McpPromptResult>(this.methods.mcpPromptGet, {
      sessionId,
      request: { reference: { serverId, remoteName }, arguments: arguments_ }
    });
  }

  /** Reads one exact MCP Resource while preserving its typed content blocks. */
  async readMcpResource(serverId: string, remoteUri: string): Promise<McpResourceResult> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    return this.requireConnection().request<McpResourceResult>(this.methods.mcpResourceRead, {
      sessionId,
      request: { reference: { serverId, remoteUri } }
    });
  }

  /** Completes one MCP Prompt or Resource Template argument through its owner. */
  async completeMcpArgument(request: Readonly<Record<string, unknown>>): Promise<McpCompletionResult> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    return this.requireConnection().request<McpCompletionResult>(this.methods.mcpComplete, {
      sessionId,
      request
    });
  }

  close(): void {
    this.stopped = true;
    this.connectionGeneration += 1;
    this.clearReconnectTimer();
    this.stopHeartbeat();
    this.updateRouter.discardPendingRecovery();
    this.connection?.close();
    this.connection = undefined;
    if (this.listenersInstalled) {
      window.removeEventListener("online", this.onlineListener);
      window.removeEventListener("popstate", this.popStateListener);
      document.removeEventListener("visibilitychange", this.visibilityListener);
      this.listenersInstalled = false;
    }
    this.dispatch({ type: "connection/changed", connection: { type: "disconnected" } });
  }

  /** Starts at most one complete WebSocket initialization and recovery attempt. */
  private ensureConnected(reconnecting: boolean): Promise<void> {
    if (this.connectPromise !== undefined) return this.connectPromise;
    let failure: string | undefined;
    const promise = this.connect(reconnecting).catch((reason: unknown) => {
      failure = reason instanceof Error ? reason.message : String(reason);
    }).finally(() => {
      if (this.connectPromise === promise) this.connectPromise = undefined;
      if (failure !== undefined && !this.stopped) this.scheduleReconnect(failure);
    });
    this.connectPromise = promise;
    return promise;
  }

  /** Establishes and recovers one generation without consulting a replacement connection. */
  private async connect(reconnecting: boolean): Promise<void> {
    const attempt = reconnecting ? Math.max(this.reconnectAttempt, 1) : 1;
    this.dispatch({ type: "connection/changed", connection: reconnecting ? { type: "reconnecting", attempt } : { type: "connecting", attempt } });
    const generation = ++this.connectionGeneration;
    const protocol = location.protocol === "https:" ? "wss:" : "ws:";
    const url = new URL(this.bootstrap.acpPath, `${protocol}//${location.host}`);
    const connection = new AcpConnection(url, {
      notifications: (notifications) => {
        if (!this.isCurrentConnection(generation, connection)) return;
        try {
          this.applyNotifications(notifications);
        } catch (reason: unknown) {
          const message = reason instanceof Error ? reason.message : String(reason);
          // A projection error invalidates at least one recovery cursor. Close
          // explicitly and enter the same reconnect path used by transport loss.
          connection.close();
          this.disconnected(
            generation,
            connection,
            `Failed to apply ACP notifications: ${message}`
          );
        }
      },
      diagnostic: (message) => {
        if (this.isCurrentConnection(generation, connection)) this.dispatch({ type: "diagnostic/added", message });
      },
      activity: () => {
        if (this.isCurrentConnection(generation, connection)) this.lastActivityAt = Date.now();
      },
      closed: (reason) => this.disconnected(generation, connection, reason)
    });
    connection.handle("elicitation/create", (params, signal) => this.elicit(params, signal));
    this.connection = connection;
    try {
      await connection.connect();
      this.assertCurrentConnection(generation, connection);
      this.lastActivityAt = Date.now();
      this.dispatch({ type: "connection/changed", connection: { type: "initializing" } });
      const initialized = await connection.request<InitializeResult>(AcpProtocol.methods.initialize, {
        protocolVersion: 2,
        info: { name: `${this.bootstrap.product.slug}-web`, version: "0.1.0" },
        capabilities: { elicitation: { form: {}, url: {} } },
        _meta: {
          [this.bootstrap.product.slug]: {
            sessionRecovery: { version: 1, sessions: this.updateRouter.recoveryCursors() }
          }
        }
      });
      this.assertCurrentConnection(generation, connection);
      const recovery = this.validateCapabilities(initialized);
      const listed = await connection.request<SessionListResult>(AcpProtocol.methods.sessionList, {});
      this.assertCurrentConnection(generation, connection);
      this.dispatch({ type: "session/listed", sessions: listed.sessions.map((session) => this.sessionSummary(session)) });
      await this.recoverSessions(connection, generation, recovery);
      this.assertCurrentConnection(generation, connection);
      this.reconnectAttempt = 0;
      this.clearReconnectTimer();
      this.dispatch({ type: "connection/changed", connection: { type: "ready" } });
      this.startHeartbeat(generation, connection);
      // URL restoration must wait for the authoritative Session list and any
      // retained projections before it decides whether the target still exists.
      await this.restoreSessionFromUrl();
    } catch (reason: unknown) {
      connection.close();
      // A close callback may already have cleared this connection while the
      // generation is still current. Preserve that failure so single-flight
      // cleanup can schedule the next attempt.
      if (this.stopped || generation !== this.connectionGeneration) return;
      if (this.connection === connection) this.connection = undefined;
      this.updateRouter.discardPendingRecovery();
      const message = reason instanceof Error ? reason.message : String(reason);
      this.dispatch({ type: "connection/changed", connection: { type: "failed", message } });
      throw reason;
    }
  }

  /** Validates standard capabilities and returns typed recovery plans. */
  private validateCapabilities(result: InitializeResult): InitializeRecovery {
    if (result.protocolVersion !== 2) throw new Error(`Unsupported ACP version ${result.protocolVersion}`);
    const sessionCapabilities = result.capabilities.session;
    this.sessionDeleteSupported = typeof sessionCapabilities === "object"
      && sessionCapabilities !== null
      && typeof sessionCapabilities.delete === "object"
      && sessionCapabilities.delete !== null;
    const meta = result._meta?.[this.bootstrap.product.slug];
    const record = AcpProtocol.decodeRecord(meta, "ACP capability metadata");
    const advertised = Array.isArray(record.methods) ? record.methods.filter((value): value is string => typeof value === "string") : [];
    const missing = AcpMethods.required(this.methods).filter((method) => !advertised.includes(method));
    if (missing.length > 0) throw new Error(`Agent is missing ACP extensions: ${missing.join(", ")}`);
    const recovery = AcpProtocol.initializeRecovery(result._meta, this.bootstrap.product.slug);
    if (recovery === undefined) throw new Error("Agent does not advertise ACP Session recovery version 1");
    return recovery;
  }

  /** Resolves newly discovered children without refreshing already-listed replayed Sessions. */
  async ensureSessionListed(sessionId: SessionId): Promise<void> {
    if (useWorkspaceStore.getState().sessions.some((session) => session.sessionId === sessionId)) return;
    const connection = this.requireConnection();
    const pending = this.sessionRefresh?.connection === connection ? this.sessionRefresh.promise : undefined;
    if (pending !== undefined) {
      await pending;
      if (useWorkspaceStore.getState().sessions.some((session) => session.sessionId === sessionId)) return;
    }
    // A child may have been created after an in-flight list's snapshot was taken.
    await this.refreshSessions();
  }

  /** Coalesces lookups while mutation callers request a new snapshot and supersede older replies. */
  async refreshSessions(afterMutation = false): Promise<void> {
    const connection = this.requireConnection();
    if (!afterMutation && this.sessionRefresh?.connection === connection) return this.sessionRefresh.promise;
    const generation = this.connectionGeneration;
    const promise: Promise<void> = connection.request<SessionListResult>(AcpProtocol.methods.sessionList, {}).then((result) => {
      this.assertCurrentConnection(generation, connection);
      if (this.sessionRefresh?.promise !== promise) return;
      this.dispatch({ type: "session/listed", sessions: result.sessions.map((session) => this.sessionSummary(session)) });
    });
    const refresh = { connection, promise };
    this.sessionRefresh = refresh;
    try { await promise; }
    finally { if (this.sessionRefresh === refresh) this.sessionRefresh = undefined; }
  }

  /** Polls persisted children's active-run flags without resuming idle Sessions. */
  async refreshSessionActivity(sessionIds: readonly SessionId[]): Promise<void> {
    await Promise.all(sessionIds.map(async (sessionId) => {
      const runtime = await this.requireConnection().request<SessionRuntimeSnapshot>(this.methods.sessionRuntime, { sessionId });
      this.dispatchSession(sessionId, { type: "running/changed", running: runtime.running });
    }));
  }

  /** Validates optional product metadata at the ACP boundary. */
  private sessionSummary(session: SessionInfo): SessionSummary {
    const meta = session._meta?.[this.bootstrap.product.slug];
    const details = typeof meta === "object" && meta !== null && !Array.isArray(meta) ? meta as Record<string, unknown> : {};
    const edge = typeof details.ownership === "object" && details.ownership !== null ? details.ownership as Record<string, unknown> : {};
    const ownership = typeof edge.sessionId !== "string" ? undefined
      : edge.kind === "tool" && typeof edge.toolCallId === "string"
        ? { kind: "tool" as const, sessionId: edge.sessionId as SessionId, toolCallId: edge.toolCallId }
        : edge.kind === "background" && typeof edge.name === "string"
          ? { kind: "background" as const, sessionId: edge.sessionId as SessionId, name: edge.name } : undefined;
    return {
      sessionId: session.sessionId,
      cwd: session.cwd,
      title: session.title?.trim() || session.sessionId,
      ...(ownership === undefined ? {} : { ownership }),
      ...(typeof details.createdAtMs === "string" ? { createdAtMs: details.createdAtMs as TimestampMs } : {}),
      ...(typeof details.modifiedAtMs === "string" ? { modifiedAtMs: details.modifiedAtMs as TimestampMs } : {}),
      ...(typeof details.parentSessionId === "string" ? { parentSessionId: details.parentSessionId as SessionId } : {})
    };
  }

  /** Recovers every retained Session projection and reinstalls its connection watcher. */
  private async recoverSessions(
    connection: AcpConnection,
    generation: number,
    recovery: InitializeRecovery
  ): Promise<void> {
    const plans = new Map<SessionId, SessionRecoveryPlan>(
      recovery.sessions.map((plan) => [plan.sessionId, plan])
    );
    const state = useWorkspaceStore.getState();
    const sessions = new Map(state.sessions.map((session) => [session.sessionId, session]));
    await Promise.all([...state.sessionWorkspaces.keys()].map(async (sessionId) => {
      const session = sessions.get(sessionId);
      if (session === undefined) {
        this.updateRouter.resetRecovery(sessionId);
        return;
      }
      this.updateRouter.beginRecovery(sessionId);
      try {
        const plan = plans.get(sessionId);
        if (plan?.mode === "watch" && plan.runId !== undefined && plan.nextSequence !== undefined) {
          try {
            await this.resume(connection, sessionId, {
              sessionId,
              cwd: session.cwd,
              replayFrom: {
                type: "_clawcode/event",
                runId: plan.runId,
                sequence: plan.nextSequence
              }
            });
          } catch (reason: unknown) {
            if (!this.isCursorUnavailable(reason)) throw reason;
            await this.fullResume(connection, generation, sessionId, session.cwd);
          }
        } else {
          await this.fullResume(connection, generation, sessionId, session.cwd);
        }
        this.assertCurrentConnection(generation, connection);
        // ACP guarantees that session/resume resolves only after the replay
        // stream is fully delivered, so this boundary is safe: replay batches
        // are all present above the divider while subsequent live updates keep
        // their higher sequence and stay below it.
        const commit = this.updateRouter.finishRecovery(sessionId);
        if (commit !== undefined) {
          this.dispatchSession(sessionId, {
            type: "recovery/completed",
            notice: commit.notice,
            order: commit.order
          });
        }
        await this.refreshRecoveredSession(connection, generation, sessionId);
      } catch (reason: unknown) {
        this.updateRouter.cancelRecovery(sessionId);
        throw reason;
      }
    }));
  }

  /** Clears one stale projection and attaches using standard full ACP replay. */
  private async fullResume(
    connection: AcpConnection,
    generation: number,
    sessionId: SessionId,
    cwd: string
  ): Promise<void> {
    this.updateRouter.requireFullReplay(sessionId);
    this.dispatchSession(sessionId, { type: "transcript/cleared" });
    await this.resume(connection, sessionId, { sessionId, cwd, replayFrom: { type: "start" } });
    this.assertCurrentConnection(generation, connection);
    this.dispatchSession(sessionId, { type: "outcome/unknown", value: [...(this.submissions.get(sessionId)?.values() ?? [])].some((record) => record.state === "unknown") });
  }

  /** Refreshes mutable Session snapshots after its watcher is attached. */
  private async refreshRecoveredSession(
    connection: AcpConnection,
    generation: number,
    sessionId: SessionId
  ): Promise<void> {
    const [runtime, tree, pending, skills, mcpSnapshot] = await Promise.all([
      connection.request<SessionRuntimeSnapshot>(this.methods.sessionRuntime, { sessionId }),
      connection.request<SessionTree>(this.methods.tree, { sessionId }),
      connection.request<PendingMessages>(this.methods.pendingMessages, { sessionId }),
      connection.request<SkillListResult>(this.methods.skillList, { sessionId }),
      connection.request<McpSessionSnapshot>(this.methods.mcpStatus, { sessionId })
    ]);
    this.assertCurrentConnection(generation, connection);
    this.dispatchSession(sessionId, { type: "running/changed", running: runtime.running });
    this.dispatchSession(sessionId, { type: "tree/replaced", tree });
    this.dispatchSession(sessionId, { type: "queue/replaced", pending });
    this.reconcileSubmissions(sessionId, pending, true);
    this.dispatchSession(sessionId, { type: "skills/replaced", result: skills });
    this.dispatchSession(sessionId, { type: "mcp/replaced", snapshot: mcpSnapshot });
  }

  /** Identifies the stable ACP error that requires a one-time full replay fallback. */
  private isCursorUnavailable(reason: unknown): boolean {
    if (!(reason instanceof AcpRequestError)) return false;
    const data = reason.rpc.data;
    return typeof data === "object"
      && data !== null
      && !Array.isArray(data)
      && (data as Record<string, unknown>).code === "_clawcode/session_recovery_cursor_unavailable";
  }

  /** Applies every notification from one physical JSON-RPC frame as one UI batch. */
  private applyNotifications(notifications: readonly AcpNotification[]): void {
    const sessionUpdates: SessionUpdateNotification[] = [];
    for (const notification of notifications) {
      if (notification.method === this.notifications.mcpUpdated) {
        const update = AcpProtocol.decodeRecord(
          notification.params,
          "MCP revision notification"
        );
        if (
          typeof update.sessionId === "string"
          && !this.deletingSessionIds.has(update.sessionId as SessionId)
          && typeof update.revision === "number"
          && update.revision > sessionWorkspace(
            useWorkspaceStore.getState(),
            update.sessionId as SessionId
          ).mcpSnapshot.revision
        ) {
          void this.refreshMcpSnapshot(update.sessionId as SessionId);
        }
        continue;
      }
      if (notification.method !== AcpProtocol.methods.sessionUpdate) {
        this.dispatch({
          type: "diagnostic/added",
          message: `Unknown notification: ${notification.method}`
        });
        continue;
      }
      sessionUpdates.push(
        AcpProtocol.decodeRecord(
          notification.params,
          "session/update"
        ) as SessionUpdateNotification
      );
    }
    if (sessionUpdates.length > 0) {
      const before = useWorkspaceStore.getState();
      this.updateRouter.applyMany(sessionUpdates);
      const after = useWorkspaceStore.getState();
      for (const sessionId of new Set(sessionUpdates.map((update) => update.sessionId))) {
        const pending = sessionWorkspace(after, sessionId).pending;
        // Streaming and ignored replay groups do not change the authoritative queue view.
        if (pending !== sessionWorkspace(before, sessionId).pending) this.reconcileSubmissions(sessionId, pending);
      }
    }
  }

  /** Reads and applies the latest atomic MCP snapshot to its owning Session. */
  private async refreshMcpSnapshot(sessionId: SessionId): Promise<void> {
    try {
      const snapshot = await this.requireConnection().request<McpSessionSnapshot>(
        this.methods.mcpStatus,
        { sessionId }
      );
      this.dispatchSession(sessionId, { type: "mcp/replaced", snapshot });
    } catch (reason: unknown) {
      this.dispatchSession(sessionId, {
        type: "diagnostic/added",
        message: reason instanceof Error ? reason.message : String(reason)
      });
    }
  }

  /** Refreshes terminal Run state plus mutable Session snapshots after settlement. */
  private async refreshSessionRuntime(sessionId: SessionId): Promise<void> {
    await this.refreshSessions(true);
    const [runtime, tree] = await Promise.all([
      this.requireConnection().request<SessionRuntimeSnapshot>(this.methods.sessionRuntime, { sessionId }),
      this.requireConnection().request<SessionTree>(this.methods.tree, { sessionId })
    ]);
    this.dispatchSession(sessionId, { type: "running/changed", running: runtime.running });
    this.dispatchSession(sessionId, { type: "tree/replaced", tree });
  }

  /** Handles closure only when it belongs to the currently installed generation. */
  private disconnected(generation: number, connection: AcpConnection, reason: string): void {
    if (!this.isCurrentConnection(generation, connection)) return;
    // Physical WebSocket frames are atomic, but a projection group may span
    // frames when the configured batch size is smaller than that group.
    this.updateRouter.discardPendingRecovery();
    this.connection = undefined;
    this.stopHeartbeat();
    if (this.stopped) return;
    this.scheduleReconnect(reason);
  }

  /** Schedules one jittered reconnect without duplicating a timer or active attempt. */
  private scheduleReconnect(reason: string): void {
    if (this.stopped || this.reconnectTimer !== undefined) return;
    if (this.connectPromise !== undefined) {
      const activeAttempt = this.connectPromise;
      // Defer scheduling until single-flight cleanup clears connectPromise;
      // timer deduplication makes concurrent failure signals harmless.
      void activeAttempt.finally(() => this.scheduleReconnect(reason));
      return;
    }
    this.reconnectAttempt += 1;
    const baseDelay = RECONNECT_DELAYS[Math.min(this.reconnectAttempt - 1, RECONNECT_DELAYS.length - 1)] ?? 5_000;
    const delay = baseDelay + Math.floor(Math.random() * Math.min(250, baseDelay));
    this.dispatch({ type: "connection/changed", connection: { type: "reconnecting", attempt: this.reconnectAttempt } });
    this.reconnectTimer = window.setTimeout(() => {
      this.reconnectTimer = undefined;
      if (!this.stopped) void this.ensureConnected(true);
    }, delay);
    this.dispatch({ type: "diagnostic/added", message: reason });
  }

  /** Starts an idle-only standard ACP request heartbeat for one generation. */
  private startHeartbeat(generation: number, connection: AcpConnection): void {
    this.stopHeartbeat();
    this.heartbeatTimer = window.setInterval(() => {
      if (
        !this.isCurrentConnection(generation, connection)
        || this.heartbeatInFlight
        || Date.now() - this.lastActivityAt < HEARTBEAT_IDLE_MS
      ) return;
      this.probeConnection(generation, connection);
    }, HEARTBEAT_INTERVAL_MS);
  }

  /** Sends one standard idle probe and reconnects if its generation stops responding. */
  private probeConnection(generation: number, connection: AcpConnection): void {
    if (!this.isCurrentConnection(generation, connection) || this.heartbeatInFlight) return;
    this.heartbeatInFlight = true;
    void connection.request(AcpProtocol.methods.sessionList, {}, { timeoutMs: HEARTBEAT_TIMEOUT_MS })
      .catch((reason: unknown) => {
        if (!this.isCurrentConnection(generation, connection)) return;
        const message = reason instanceof Error ? reason.message : String(reason);
        connection.close();
        this.disconnected(generation, connection, message);
      })
      .finally(() => {
        if (this.isCurrentConnection(generation, connection)) this.heartbeatInFlight = false;
      });
  }

  /** Clears heartbeat state for a disconnected or replaced connection. */
  private stopHeartbeat(): void {
    if (this.heartbeatTimer !== undefined) window.clearInterval(this.heartbeatTimer);
    this.heartbeatTimer = undefined;
    this.heartbeatInFlight = false;
  }

  /** Cancels the current backoff timer. */
  private clearReconnectTimer(): void {
    if (this.reconnectTimer !== undefined) window.clearTimeout(this.reconnectTimer);
    this.reconnectTimer = undefined;
  }

  /** Rechecks a ready connection or bypasses backoff after browser network activity. */
  private wakeConnection(): void {
    if (this.stopped) return;
    this.clearReconnectTimer();
    const connection = this.connection;
    if (connection === undefined) {
      void this.ensureConnected(true);
      return;
    }
    // The heartbeat timer is installed only after initialization and Session
    // recovery finish, so connecting sockets must complete their current attempt.
    if (this.heartbeatTimer !== undefined) {
      this.probeConnection(this.connectionGeneration, connection);
    }
  }

  /** Installs browser wake listeners exactly once. */
  private installNetworkListeners(): void {
    if (this.listenersInstalled) return;
    window.addEventListener("online", this.onlineListener);
    window.addEventListener("popstate", this.popStateListener);
    document.addEventListener("visibilitychange", this.visibilityListener);
    this.listenersInstalled = true;
  }

  /** Opens the Session encoded in the current URL without creating another history entry. */
  private async restoreSessionFromUrl(): Promise<void> {
    const sessionId = this.sessionIdFromUrl();
    if (sessionId === undefined) {
      if (location.pathname !== "/") this.replaceRootUrl();
      if (useWorkspaceStore.getState().activeSessionId !== undefined) {
        this.dispatch({ type: "session/deactivated" });
      }
      return;
    }
    const state = useWorkspaceStore.getState();
    if (!state.sessions.some((session) => session.sessionId === sessionId)) {
      this.replaceRootUrl();
      if (state.activeSessionId !== undefined) {
        this.dispatch({ type: "session/deactivated" });
      }
      return;
    }
    if (state.activeSessionId === sessionId) return;
    try {
      await this.openSession(sessionId, false);
    } catch (reason: unknown) {
      this.dispatch({
        type: "diagnostic/added",
        message: reason instanceof Error ? reason.message : String(reason)
      });
    }
  }

  /** Decodes an exact `/sessions/:sessionId` route into its branded identifier. */
  private sessionIdFromUrl(): SessionId | undefined {
    const match = /^\/sessions\/([^/]+)$/.exec(location.pathname);
    if (match?.[1] === undefined) return undefined;
    try {
      const sessionId = decodeURIComponent(match[1]);
      return sessionId.length === 0 ? undefined : sessionId as SessionId;
    } catch {
      return undefined;
    }
  }

  /** Pushes one selected Session route unless the browser already shows it. */
  private pushSessionUrl(sessionId: SessionId): void {
    const path = `/sessions/${encodeURIComponent(sessionId)}`;
    if (location.pathname === path && location.search.length === 0 && location.hash.length === 0) return;
    window.history.pushState(null, "", path);
  }

  /** Replaces an invalid or deleted Session route with the application root. */
  private replaceRootUrl(): void {
    if (location.pathname === "/" && location.search.length === 0 && location.hash.length === 0) return;
    window.history.replaceState(null, "", "/");
  }

  /** Returns whether an asynchronous callback still owns the active connection. */
  private isCurrentConnection(generation: number, connection: AcpConnection): boolean {
    return !this.stopped
      && this.connectionGeneration === generation
      && this.connection === connection;
  }

  /** Rejects stale asynchronous work before it can mutate current workspace state. */
  private assertCurrentConnection(generation: number, connection: AcpConnection): void {
    if (!this.isCurrentConnection(generation, connection)) throw new Error("ACP connection generation was replaced");
  }

  private requireConnection(): AcpConnection {
    if (this.connection === undefined) throw new Error("ACP connection is unavailable");
    return this.connection;
  }

  /** Dispatches one state transition into the projection owned by a Session. */
  private dispatchSession(sessionId: SessionId, action: SessionWorkspaceAction): void {
    this.dispatch({ type: "session/updated", sessionId, action });
  }

  /** Dispatches one global workspace state transition. */
  private dispatch(action: WorkspaceAction): void {
    useWorkspaceStore.getState().dispatch(action);
  }
}
