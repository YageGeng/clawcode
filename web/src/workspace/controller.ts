import { AcpConnection } from "../acp/connection";
import { AcpMethods, AcpNotifications } from "../acp/extensions";
import type { AcpExtensionMethods, AcpExtensionNotifications } from "../acp/extensions";
import { AcpProtocol } from "../acp/protocol";
import type { InitializeResult, NewSessionResult, PromptContentBlock, SessionId, SessionInfo, SessionListResult, SessionUpdateNotification, TimestampMs } from "../acp/protocol";
import type { UiBootstrap } from "../bootstrap/model";
import type { BranchEditPlan } from "../domain/messageActions";
import type { McpCompletionResult, McpElicitation, McpElicitationSnapshot, McpPromptResult, McpResourceResult, McpSessionSnapshot, PendingMessages, PromptInput, SessionRuntimeSnapshot, SessionSummary, SessionTree, SkillListResult } from "../domain/model";
import { useWorkspaceStore } from "./store";
import type { SessionWorkspaceAction } from "./sessionState";
import { sessionWorkspace } from "./state";
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
  private sessionDeleteSupported = false;
  private readonly deletingSessionIds = new Set<SessionId>();
  private readonly runtimePolls = new Map<SessionId, number>();
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
    const state = useWorkspaceStore.getState();
    const session = state.sessions.find((item) => item.sessionId === sessionId);
    if (session === undefined) throw new Error("Session is not in the current list");
    this.dispatch({ type: "session/activated", sessionId });

    const pendingPoll = this.runtimePolls.get(sessionId);
    if (pendingPoll !== undefined) {
      window.clearTimeout(pendingPoll);
      this.runtimePolls.delete(sessionId);
    }
    const runtime = await this.requireConnection().request<SessionRuntimeSnapshot>(
      this.methods.sessionRuntime,
      { sessionId }
    );
    this.dispatchSession(sessionId, { type: "running/changed", running: runtime.running });

    if (!runtime.running) {
      // Idle sessions are replayed on every activation because another ACP
      // client may have changed the persisted transcript while this tab slept.
      this.dispatchSession(sessionId, { type: "transcript/cleared" });
      await this.requireConnection().request(AcpProtocol.methods.sessionResume, {
        sessionId,
        cwd: session.cwd,
        replayFrom: { type: "start" }
      });
      this.dispatchSession(sessionId, { type: "outcome/unknown", value: false });
    }

    const [tree, pending, skills, mcpSnapshot, mcpElicitations] = await Promise.all([
      this.requireConnection().request<SessionTree>(this.methods.tree, { sessionId }),
      this.requireConnection().request<PendingMessages>(this.methods.pendingMessages, { sessionId }),
      this.requireConnection().request<SkillListResult>(this.methods.skillList, { sessionId }),
      this.requireConnection().request<McpSessionSnapshot>(this.methods.mcpStatus, { sessionId }),
      this.requireConnection().request<McpElicitationSnapshot>(this.methods.mcpElicitationList, { sessionId })
    ]);
    this.dispatchSession(sessionId, { type: "tree/replaced", tree });
    this.dispatchSession(sessionId, { type: "queue/replaced", pending });
    this.dispatchSession(sessionId, { type: "skills/replaced", result: skills });
    this.dispatchSession(sessionId, { type: "mcp/replaced", snapshot: mcpSnapshot });
    this.dispatchSession(sessionId, { type: "mcp/elicitations-replaced", snapshot: mcpElicitations });
    if (runtime.running) this.scheduleSessionRuntimePoll(sessionId);
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
    const sessionId = state.activeSessionId;
    const workspace = sessionWorkspace(state, sessionId);
    const text = input.text;
    if (text.trim().length === 0 && input.resources.length === 0 && input.images.length === 0) throw new Error("Prompt is empty");
    const prompt: PromptContentBlock[] = [
      ...(text.length === 0 ? [] : [{ type: "text" as const, text }]),
      ...input.resources.map((resource) => ({ type: "resource_link" as const, name: resource.name, uri: resource.uri })),
      ...input.images.map((image) => ({ type: "image" as const, data: image.data, mimeType: image.mimeType }))
    ];
    this.dispatchSession(sessionId, { type: "outcome/unknown", value: false });
    try {
      if (workspace.running) {
        await this.requireConnection().request(this.methods.followUp, { sessionId, prompt });
        await this.refreshPending(sessionId);
      } else {
        await this.requireConnection().request(AcpProtocol.methods.sessionPrompt, {
          sessionId,
          prompt
        });
      }
    } catch (reason: unknown) {
      if (this.connection === undefined) this.dispatchSession(sessionId, { type: "outcome/unknown", value: true });
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
    await this.refreshSessions();
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
    this.dispatchSession(request.context.sessionId, {
      type: "mcp/elicitation-resolved",
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
    for (const timer of this.runtimePolls.values()) window.clearTimeout(timer);
    this.runtimePolls.clear();
    this.connection?.close();
    this.connection = undefined;
    this.dispatch({ type: "connection/changed", connection: { type: "disconnected" } });
  }

  private async connect(reconnecting: boolean): Promise<void> {
    const attempt = reconnecting ? this.reconnectAttempt : 1;
    if (reconnecting) {
      for (const timer of this.runtimePolls.values()) window.clearTimeout(timer);
      this.runtimePolls.clear();
      this.dispatch({ type: "sessions/invalidated" });
    }
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
      if (reconnecting) {
        // Background Sessions cannot replay through their original connection,
        // but their Run state remains queryable and cancellable after reconnect.
        const backgroundSessionIds = [...useWorkspaceStore.getState().sessionWorkspaces.keys()]
          .filter((sessionId) => sessionId !== active);
        await Promise.all(backgroundSessionIds.map(async (sessionId) => {
          try {
            const runtime = await this.requireConnection().request<SessionRuntimeSnapshot>(
              this.methods.sessionRuntime,
              { sessionId }
            );
            this.dispatchSession(sessionId, { type: "running/changed", running: runtime.running });
            if (runtime.running) this.scheduleSessionRuntimePoll(sessionId);
          } catch (reason: unknown) {
            this.dispatchSession(sessionId, {
              type: "diagnostic/added",
              message: reason instanceof Error ? reason.message : String(reason)
            });
          }
        }));
        if (active !== undefined) await this.openSession(active);
      }
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
    this.dispatchSession(sessionId, { type: "queue/replaced", pending });
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
    const [runtime, tree, pending] = await Promise.all([
      this.requireConnection().request<SessionRuntimeSnapshot>(this.methods.sessionRuntime, { sessionId }),
      this.requireConnection().request<SessionTree>(this.methods.tree, { sessionId }),
      this.requireConnection().request<PendingMessages>(this.methods.pendingMessages, { sessionId })
    ]);
    this.dispatchSession(sessionId, { type: "running/changed", running: runtime.running });
    this.dispatchSession(sessionId, { type: "tree/replaced", tree });
    this.dispatchSession(sessionId, { type: "queue/replaced", pending });
  }

  /** Polls a detached active Run until its durable transcript can be replayed. */
  private scheduleSessionRuntimePoll(sessionId: SessionId): void {
    if (this.stopped || this.runtimePolls.has(sessionId)) return;
    const timer = window.setTimeout(() => {
      this.runtimePolls.delete(sessionId);
      void (async () => {
        if (this.stopped || this.connection === undefined) return;
        try {
          const runtime = await this.requireConnection().request<SessionRuntimeSnapshot>(
            this.methods.sessionRuntime,
            { sessionId }
          );
          this.dispatchSession(sessionId, { type: "running/changed", running: runtime.running });
          if (runtime.running) {
            this.scheduleSessionRuntimePoll(sessionId);
          } else if (useWorkspaceStore.getState().activeSessionId === sessionId) {
            // The old event sink cannot follow a replacement ACP connection;
            // replay once the Run releases the Session operation gate.
            await this.openSession(sessionId);
          }
        } catch (reason: unknown) {
          if (!this.stopped && this.connection !== undefined) {
            this.dispatchSession(sessionId, {
              type: "diagnostic/added",
              message: reason instanceof Error ? reason.message : String(reason)
            });
          }
        }
      })();
    }, 1_000);
    this.runtimePolls.set(sessionId, timer);
  }

  private disconnected(reason: string): void {
    this.connection = undefined;
    for (const timer of this.runtimePolls.values()) window.clearTimeout(timer);
    this.runtimePolls.clear();
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

  /** Dispatches one state transition into the projection owned by a Session. */
  private dispatchSession(sessionId: SessionId, action: SessionWorkspaceAction): void {
    this.dispatch({ type: "session/updated", sessionId, action });
  }

  /** Dispatches one global workspace state transition. */
  private dispatch(action: WorkspaceAction): void {
    useWorkspaceStore.getState().dispatch(action);
  }
}
