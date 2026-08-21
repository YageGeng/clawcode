import { AcpConnection, AcpRequestError } from "../acp/connection";
import type { AcpNotification } from "../acp/connection";
import { AcpMethods, AcpNotifications } from "../acp/extensions";
import type { AcpExtensionMethods, AcpExtensionNotifications } from "../acp/extensions";
import { AcpProtocol } from "../acp/protocol";
import type { InitializeRecovery, InitializeResult, NewSessionResult, PromptContentBlock, SessionId, SessionInfo, SessionListResult, SessionRecoveryPlan, SessionUpdateNotification, TimestampMs } from "../acp/protocol";
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
const HEARTBEAT_INTERVAL_MS = 5_000;
const HEARTBEAT_IDLE_MS = 20_000;
const HEARTBEAT_TIMEOUT_MS = 5_000;

export class WorkspaceController {
  readonly bootstrap: UiBootstrap;
  readonly methods: AcpExtensionMethods;
  readonly notifications: AcpExtensionNotifications;
  private readonly updateRouter: SessionUpdateRouter;
  private connection: AcpConnection | undefined;
  private connectionGeneration = 0;
  private connectPromise: Promise<void> | undefined;
  private reconnectTimer: number | undefined;
  private heartbeatTimer: number | undefined;
  private heartbeatInFlight = false;
  private lastActivityAt = 0;
  private reconnectAttempt = 0;
  private sessionDeleteSupported = false;
  private readonly deletingSessionIds = new Set<SessionId>();
  private listenersInstalled = false;
  private stopped = false;
  private readonly onlineListener = () => this.wakeConnection();
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

  async openSession(sessionId: SessionId): Promise<void> {
    const state = useWorkspaceStore.getState();
    const session = state.sessions.find((item) => item.sessionId === sessionId);
    if (session === undefined) throw new Error("Session is not in the current list");
    const wasLoaded = state.sessionWorkspaces.has(sessionId);
    this.dispatch({ type: "session/activated", sessionId });
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
      await connection.request(AcpProtocol.methods.sessionResume, {
        sessionId,
        cwd: session.cwd,
        replayFrom: { type: "start" }
      });
      this.dispatchSession(sessionId, { type: "outcome/unknown", value: false });
    }

    const [tree, pending, skills, mcpSnapshot, mcpElicitations] = await Promise.all([
      connection.request<SessionTree>(this.methods.tree, { sessionId }),
      connection.request<PendingMessages>(this.methods.pendingMessages, { sessionId }),
      connection.request<SkillListResult>(this.methods.skillList, { sessionId }),
      connection.request<McpSessionSnapshot>(this.methods.mcpStatus, { sessionId }),
      connection.request<McpElicitationSnapshot>(this.methods.mcpElicitationList, { sessionId })
    ]);
    this.dispatchSession(sessionId, { type: "tree/replaced", tree });
    this.dispatchSession(sessionId, { type: "queue/replaced", pending });
    this.dispatchSession(sessionId, { type: "skills/replaced", result: skills });
    this.dispatchSession(sessionId, { type: "mcp/replaced", snapshot: mcpSnapshot });
    this.dispatchSession(sessionId, { type: "mcp/elicitations-replaced", snapshot: mcpElicitations });
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
      this.updateRouter.resetRecovery(sessionId);
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
    const connection = this.requireConnection();
    this.dispatchSession(sessionId, { type: "outcome/unknown", value: false });
    try {
      if (workspace.running) {
        await connection.request(this.methods.followUp, { sessionId, prompt });
        await this.refreshPending(sessionId);
      } else {
        await connection.request(AcpProtocol.methods.sessionPrompt, {
          sessionId,
          prompt
        });
      }
    } catch (reason: unknown) {
      if (this.connection !== connection) this.dispatchSession(sessionId, { type: "outcome/unknown", value: true });
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
    this.connectionGeneration += 1;
    this.clearReconnectTimer();
    this.stopHeartbeat();
    this.updateRouter.discardPendingRecovery();
    this.connection?.close();
    this.connection = undefined;
    if (this.listenersInstalled) {
      window.removeEventListener("online", this.onlineListener);
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
    this.connection = connection;
    try {
      await connection.connect();
      this.assertCurrentConnection(generation, connection);
      this.lastActivityAt = Date.now();
      this.dispatch({ type: "connection/changed", connection: { type: "initializing" } });
      const initialized = await connection.request<InitializeResult>(AcpProtocol.methods.initialize, {
        protocolVersion: 2,
        info: { name: `${this.bootstrap.product.slug}-web`, version: "0.1.0" },
        capabilities: {},
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
      const plan = plans.get(sessionId);
      if (plan?.mode === "watch" && plan.runId !== undefined && plan.nextSequence !== undefined) {
        try {
          await connection.request(AcpProtocol.methods.sessionResume, {
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
      await this.refreshRecoveredSession(connection, generation, sessionId);
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
    await connection.request(AcpProtocol.methods.sessionResume, {
      sessionId,
      cwd,
      replayFrom: { type: "start" }
    });
    this.assertCurrentConnection(generation, connection);
    this.dispatchSession(sessionId, { type: "outcome/unknown", value: false });
  }

  /** Refreshes mutable Session snapshots after its watcher is attached. */
  private async refreshRecoveredSession(
    connection: AcpConnection,
    generation: number,
    sessionId: SessionId
  ): Promise<void> {
    const [runtime, tree, pending, skills, mcpSnapshot, mcpElicitations] = await Promise.all([
      connection.request<SessionRuntimeSnapshot>(this.methods.sessionRuntime, { sessionId }),
      connection.request<SessionTree>(this.methods.tree, { sessionId }),
      connection.request<PendingMessages>(this.methods.pendingMessages, { sessionId }),
      connection.request<SkillListResult>(this.methods.skillList, { sessionId }),
      connection.request<McpSessionSnapshot>(this.methods.mcpStatus, { sessionId }),
      connection.request<McpElicitationSnapshot>(this.methods.mcpElicitationList, { sessionId })
    ]);
    this.assertCurrentConnection(generation, connection);
    this.dispatchSession(sessionId, { type: "running/changed", running: runtime.running });
    this.dispatchSession(sessionId, { type: "tree/replaced", tree });
    this.dispatchSession(sessionId, { type: "queue/replaced", pending });
    this.dispatchSession(sessionId, { type: "skills/replaced", result: skills });
    this.dispatchSession(sessionId, { type: "mcp/replaced", snapshot: mcpSnapshot });
    this.dispatchSession(sessionId, { type: "mcp/elicitations-replaced", snapshot: mcpElicitations });
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
    if (sessionUpdates.length > 0) this.updateRouter.applyMany(sessionUpdates);
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
    document.addEventListener("visibilitychange", this.visibilityListener);
    this.listenersInstalled = true;
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
