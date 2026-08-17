import { AcpConnection } from "../acp/connection";
import { AcpMethods, AcpNotifications } from "../acp/extensions";
import type { AcpExtensionMethods, AcpExtensionNotifications } from "../acp/extensions";
import { AcpProtocol } from "../acp/protocol";
import type { InitializeResult, NewSessionResult, SessionId, SessionInfo, SessionListResult, SessionUpdateNotification, TimestampMs } from "../acp/protocol";
import type { UiBootstrap } from "../bootstrap/model";
import type { McpCompletionResult, McpElicitation, McpElicitationSnapshot, McpPromptResult, McpResourceResult, McpSessionSnapshot, PendingMessages, PromptInput, SessionSummary, SessionTree, SkillListResult } from "../domain/model";
import { useWorkspaceStore } from "./store";
import type { WorkspaceAction } from "./state";
import { SessionUpdateRouter } from "./updateRouter";
import type { WorkspaceStoreAccess } from "./updateRouter";

const RECONNECT_DELAYS = [250, 500, 1_000, 2_000, 5_000] as const;

export class WorkspaceController {
  readonly bootstrap: UiBootstrap;
  readonly methods: AcpExtensionMethods;
  readonly notifications: AcpExtensionNotifications;
  private readonly updateRouter: SessionUpdateRouter;
  private connection: AcpConnection | undefined;
  private reconnectAttempt = 0;
  private sessionOpenRevision = 0;
  private sessionDeleteSupported = false;
  private readonly deletingSessionIds = new Set<SessionId>();
  private stopped = false;

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
    await this.connect(false);
  }

  async openSession(sessionId: SessionId): Promise<void> {
    const session = useWorkspaceStore.getState().sessions.find((item) => item.sessionId === sessionId);
    if (session === undefined) throw new Error("Session is not in the current list");
    const revision = this.sessionOpenRevision + 1;
    this.sessionOpenRevision = revision;
    this.dispatch({ type: "transcript/cleared" });
    this.dispatch({ type: "session/activated", sessionId });
    await this.requireConnection().request(AcpProtocol.methods.sessionResume, {
      sessionId,
      cwd: session.cwd,
      replayFrom: { type: "start" }
    });
    if (!this.isCurrentSessionOpen(sessionId, revision)) return;
    const [tree, pending, skills, mcpSnapshot, mcpElicitations] = await Promise.all([
      this.requireConnection().request<SessionTree>(this.methods.tree, { sessionId }),
      this.requireConnection().request<PendingMessages>(this.methods.pendingMessages, { sessionId }),
      this.requireConnection().request<SkillListResult>(this.methods.skillList, { sessionId }),
      this.requireConnection().request<McpSessionSnapshot>(this.methods.mcpStatus, { sessionId }),
      this.requireConnection().request<McpElicitationSnapshot>(this.methods.mcpElicitationList, { sessionId })
    ]);
    if (!this.isCurrentSessionOpen(sessionId, revision)) return;
    this.dispatch({ type: "tree/replaced", tree });
    this.dispatch({ type: "queue/replaced", pending });
    this.dispatch({ type: "skills/replaced", result: skills });
    this.dispatch({ type: "mcp/replaced", snapshot: mcpSnapshot });
    this.dispatch({ type: "mcp/elicitations-replaced", snapshot: mcpElicitations });
  }

  async newSession(cwd: string): Promise<SessionId> {
    const result = await this.requireConnection().request<NewSessionResult>(AcpProtocol.methods.sessionNew, { cwd });
    await this.refreshSessions();
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
    this.deletingSessionIds.add(sessionId);
    try {
      await this.requireConnection().request(AcpProtocol.methods.sessionDelete, { sessionId });
      if (useWorkspaceStore.getState().activeSessionId === sessionId) {
        this.sessionOpenRevision += 1;
        this.dispatch({ type: "session/deactivated" });
      }
      await this.refreshSessions();
    } finally {
      this.deletingSessionIds.delete(sessionId);
    }
  }

  async send(input: PromptInput): Promise<void> {
    const state = useWorkspaceStore.getState();
    if (state.activeSessionId === undefined) throw new Error("No active session");
    const text = input.text;
    if (text.trim().length === 0 && input.resources.length === 0) throw new Error("Prompt is empty");
    this.dispatch({ type: "outcome/unknown", value: false });
    try {
      if (state.running) {
        const resourceText = input.resources.map((resource) => `[${resource.name}](${resource.uri})`).join("\n");
        const followUp = [text, resourceText].filter((part) => part.length > 0).join("\n\n");
        await this.requireConnection().request(this.methods.followUp, { sessionId: state.activeSessionId, input: followUp });
        await this.refreshPending(state.activeSessionId);
      } else {
        await this.requireConnection().request(AcpProtocol.methods.sessionPrompt, {
          sessionId: state.activeSessionId,
          prompt: [
            ...(text.length === 0 ? [] : [{ type: "text", text }]),
            ...input.resources.map((resource) => ({ type: "resource_link", name: resource.name, uri: resource.uri }))
          ]
        });
      }
    } catch (reason: unknown) {
      if (this.connection === undefined) this.dispatch({ type: "outcome/unknown", value: true });
      throw reason;
    }
  }

  async removePending(queueId: string): Promise<void> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    await this.requireConnection().request(this.methods.pendingMessageRemove, { sessionId, queueId });
    await this.refreshPending(sessionId);
  }

  async clearPending(): Promise<void> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    await this.requireConnection().request(this.methods.clearQueue, { sessionId });
    await this.refreshPending(sessionId);
  }

  async navigate(entryId: string | null): Promise<void> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    await this.requireConnection().request(this.methods.navigate, { sessionId, entryId });
    await this.openSession(sessionId);
  }

  async branch(entryId: string | null): Promise<void> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    await this.requireConnection().request(this.methods.branch, { sessionId, entryId });
    await this.openSession(sessionId);
  }

  async fork(entryId: string, cwd: string): Promise<SessionId> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    const result = await this.requireConnection().request<NewSessionResult>(this.methods.fork, { sessionId, entryId, cwd });
    await this.refreshSessions();
    await this.openSession(result.sessionId);
    return result.sessionId;
  }

  cancel(): void {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId !== undefined) {
      this.requireConnection().notify(AcpProtocol.methods.sessionCancel, { sessionId });
    }
  }

  async reconnect(): Promise<void> {
    this.connection?.close();
    this.connection = undefined;
    this.reconnectAttempt = 1;
    await this.connect(true);
  }

  /** Explicitly reconnects one MCP Server and replaces the atomic Session snapshot. */
  async reconnectMcp(serverId: string): Promise<void> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    const snapshot = await this.requireConnection().request<McpSessionSnapshot>(
      this.methods.mcpReconnect,
      { sessionId, serverId }
    );
    this.dispatch({ type: "mcp/replaced", snapshot });
  }

  /** Completes a pending MCP OAuth browser round and replaces the atomic snapshot. */
  async continueMcpOAuth(serverId: string, responseUri: string): Promise<void> {
    const sessionId = useWorkspaceStore.getState().activeSessionId;
    if (sessionId === undefined) throw new Error("No active session");
    const snapshot = await this.requireConnection().request<McpSessionSnapshot>(
      this.methods.mcpOAuthContinue,
      { sessionId, serverId, result: { responseUri } }
    );
    if (useWorkspaceStore.getState().activeSessionId === sessionId) {
      this.dispatch({ type: "mcp/replaced", snapshot });
    }
  }

  /** Resolves one pending MCP elicitation through its Session-scoped ACP route. */
  async respondMcpElicitation(
    request: McpElicitation,
    action: "accept" | "decline" | "cancel",
    content?: unknown
  ): Promise<void> {
    await this.requireConnection().request(this.methods.mcpElicitationRespond, {
      sessionId: request.context.sessionId,
      requestId: request.requestId,
      result: { action, ...(content === undefined ? {} : { content }) }
    });
    this.dispatch({
      type: "mcp/elicitation-resolved",
      sessionId: request.context.sessionId,
      requestId: request.requestId
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
    this.connection?.close();
    this.connection = undefined;
    this.dispatch({ type: "connection/changed", connection: { type: "disconnected" } });
  }

  private async connect(reconnecting: boolean): Promise<void> {
    const attempt = reconnecting ? this.reconnectAttempt : 1;
    this.dispatch({ type: "connection/changed", connection: reconnecting ? { type: "reconnecting", attempt } : { type: "connecting", attempt } });
    const protocol = location.protocol === "https:" ? "wss:" : "ws:";
    const url = new URL(this.bootstrap.acpPath, `${protocol}//${location.host}`);
    const connection = new AcpConnection(url, {
      notification: (method, params) => this.notification(method, params),
      diagnostic: (message) => this.dispatch({ type: "diagnostic/added", message }),
      closed: (reason) => this.disconnected(reason)
    });
    this.connection = connection;
    try {
      await connection.connect();
      this.dispatch({ type: "connection/changed", connection: { type: "initializing" } });
      const initialized = await connection.request<InitializeResult>(AcpProtocol.methods.initialize, {
        protocolVersion: 2,
        info: { name: `${this.bootstrap.product.slug}-web`, version: "0.1.0" },
        capabilities: {}
      });
      this.validateCapabilities(initialized);
      await this.refreshSessions();
      this.reconnectAttempt = 0;
      this.dispatch({ type: "connection/changed", connection: { type: "ready" } });
      const active = useWorkspaceStore.getState().activeSessionId;
      if (reconnecting && active !== undefined) await this.openSession(active);
    } catch (reason: unknown) {
      connection.close();
      this.connection = undefined;
      const message = reason instanceof Error ? reason.message : String(reason);
      this.dispatch({ type: "connection/changed", connection: { type: "failed", message } });
      if (reconnecting && !this.stopped) {
        this.scheduleReconnect(message);
        return;
      }
      throw reason;
    }
  }

  private validateCapabilities(result: InitializeResult): void {
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
  }

  private async refreshSessions(): Promise<void> {
    const result = await this.requireConnection().request<SessionListResult>(AcpProtocol.methods.sessionList, {});
    this.dispatch({ type: "session/listed", sessions: result.sessions.map((session) => this.sessionSummary(session)) });
  }

  private sessionSummary(session: SessionInfo): SessionSummary {
    const meta = session._meta?.[this.bootstrap.product.slug];
    const details = typeof meta === "object" && meta !== null && !Array.isArray(meta) ? meta as Record<string, unknown> : {};
    return {
      sessionId: session.sessionId,
      cwd: session.cwd,
      title: session.title?.trim() || session.sessionId,
      ...(typeof details.createdAtMs === "string" ? { createdAtMs: details.createdAtMs as TimestampMs } : {}),
      ...(typeof details.modifiedAtMs === "string" ? { modifiedAtMs: details.modifiedAtMs as TimestampMs } : {}),
      ...(typeof details.parentSessionId === "string" ? { parentSessionId: details.parentSessionId as SessionId } : {})
    };
  }

  private notification(method: string, params: unknown): void {
    if (method === this.notifications.mcpUpdated) {
      const update = AcpProtocol.decodeRecord(params, "MCP revision notification");
      const activeSessionId = useWorkspaceStore.getState().activeSessionId;
      if (
        typeof update.sessionId === "string"
        && update.sessionId === activeSessionId
        && !this.deletingSessionIds.has(update.sessionId as SessionId)
        && typeof update.revision === "number"
        && update.revision > useWorkspaceStore.getState().mcpSnapshot.revision
      ) {
        void this.refreshMcpSnapshot(update.sessionId as SessionId);
      }
      return;
    }
    if (method !== AcpProtocol.methods.sessionUpdate) {
      this.dispatch({ type: "diagnostic/added", message: `Unknown notification: ${method}` });
      return;
    }
    const value = AcpProtocol.decodeRecord(params, "session/update") as SessionUpdateNotification;
    this.updateRouter.apply(value);
  }

  private async refreshPending(sessionId: SessionId): Promise<void> {
    const pending = await this.requireConnection().request<PendingMessages>(this.methods.pendingMessages, { sessionId });
    if (useWorkspaceStore.getState().activeSessionId === sessionId) {
      this.dispatch({ type: "queue/replaced", pending });
    }
  }

  /** Reads and applies the latest atomic MCP snapshot only to its active Session. */
  private async refreshMcpSnapshot(sessionId: SessionId): Promise<void> {
    try {
      const snapshot = await this.requireConnection().request<McpSessionSnapshot>(
        this.methods.mcpStatus,
        { sessionId }
      );
      if (useWorkspaceStore.getState().activeSessionId === sessionId) {
        this.dispatch({ type: "mcp/replaced", snapshot });
      }
    } catch (reason: unknown) {
      this.dispatch({
        type: "diagnostic/added",
        message: reason instanceof Error ? reason.message : String(reason)
      });
    }
  }

  private async refreshSessionRuntime(sessionId: SessionId): Promise<void> {
    const [tree, pending] = await Promise.all([
      this.requireConnection().request<SessionTree>(this.methods.tree, { sessionId }),
      this.requireConnection().request<PendingMessages>(this.methods.pendingMessages, { sessionId })
    ]);
    if (useWorkspaceStore.getState().activeSessionId === sessionId) {
      this.dispatch({ type: "tree/replaced", tree });
      this.dispatch({ type: "queue/replaced", pending });
    }
  }

  private disconnected(reason: string): void {
    this.connection = undefined;
    if (this.stopped) return;
    this.scheduleReconnect(reason);
  }

  private scheduleReconnect(reason: string): void {
    this.reconnectAttempt += 1;
    const delay = RECONNECT_DELAYS[Math.min(this.reconnectAttempt - 1, RECONNECT_DELAYS.length - 1)] ?? 5_000;
    this.dispatch({ type: "connection/changed", connection: { type: "reconnecting", attempt: this.reconnectAttempt } });
    window.setTimeout(() => {
      if (!this.stopped) void this.connect(true);
    }, delay);
    this.dispatch({ type: "diagnostic/added", message: reason });
  }

  private requireConnection(): AcpConnection {
    if (this.connection === undefined) throw new Error("ACP connection is unavailable");
    return this.connection;
  }

  /** Verifies that an asynchronous session open still owns the active projection. */
  private isCurrentSessionOpen(sessionId: SessionId, revision: number): boolean {
    return this.sessionOpenRevision === revision
      && useWorkspaceStore.getState().activeSessionId === sessionId;
  }

  private dispatch(action: WorkspaceAction): void {
    useWorkspaceStore.getState().dispatch(action);
  }
}
