import { signal } from "nsh";
import type { Accessor } from "nsh";
import { t } from "nsh/i18n";

import { AcpConnection, AcpRequestError } from "../client/acp/connection";
import type { AcpNotification } from "../client/acp/connection";
import { AcpMethods } from "../client/acp/extensions";
import { AcpProtocol } from "../client/acp/protocol";
import type { NewSessionResult, SessionId, SessionInfo, SessionListResult, SessionUpdateNotification, TimestampMs } from "../client/acp/protocol";
import type { PermissionPrompt, SessionRuntimeSnapshot, SessionSummary } from "../client/domain/model";
import type { AcpTransport } from "../client/transport";
import { initialWorkspaceState, reduceWorkspace } from "../client/workspace/state";
import type { WorkspaceAction, WorkspaceState } from "../client/workspace/state";
import { SessionUpdateRouter } from "../client/workspace/updateRouter";
import type { WorkspaceStoreAccess } from "../client/workspace/updateRouter";

/** How long `initialize` and the first Session list may take before the screen says so. */
const REQUEST_TIMEOUT_MS = 15_000;

/** The product namespace of ACP `_meta` keys and extension methods. */
const NAMESPACE = "clawcode";

/** Whether a host is this computer or a paired remote one. */
export type HostKind = "local" | "remote";

/** A host's identity: `id` is stable ("local" for this computer, a device key later). */
export type HostInfo = Readonly<{ id: string; name: string; kind: HostKind }>;

/** Where the connection to a host stands. */
export type HostPhase =
  | Readonly<{ kind: "starting" }>
  | Readonly<{ kind: "ready" }>
  | Readonly<{ kind: "failed"; reason: string }>
  // Was ready, then lost its backend: its Sessions stay on screen, read-only in effect (requests fail).
  | Readonly<{ kind: "offline"; reason: string }>;

/** One host's connection and the Sessions it serves; its methods are the only writers. */
export type HostStore = Readonly<{
  info: HostInfo;
  phase: Accessor<HostPhase>;
  /** Everything the host's Sessions show, reduced from ACP updates. */
  workspace: Accessor<WorkspaceState>;
  /** The listed Sessions; titles fall back to the Session id. */
  sessions: Accessor<readonly SessionSummary[]>;
  /** Connects, initializes and lists the Sessions; failures land in `phase`, never throw. */
  start: () => Promise<void>;
  /** Closes the connection. */
  stop: () => void;
  /** Shows a Session: replays its transcript unless it is running and already attached. */
  openSession: (sessionId: SessionId) => Promise<void>;
  /** Creates a Session in `cwd`, refreshes the list and opens it; its id. */
  newSession: (cwd: string) => Promise<SessionId>;
  /** Sends `text`; resolves when the turn ends. On failure the text comes back in `returnedText` and a notice is added. */
  prompt: (sessionId: SessionId, text: string) => Promise<void>;
  /** Text a failed prompt handed back, by Session, until `takeReturnedText` reads it. */
  returnedText: Accessor<ReadonlyMap<SessionId, string>>;
  /** Reads and forgets the text a failed prompt handed back. */
  takeReturnedText: (sessionId: SessionId) => string | undefined;
  /** Asks the agent to stop the running turn. */
  cancel: (sessionId: SessionId) => void;
  /** Answers a queued permission request; `undefined` dismisses it. */
  answerPermission: (key: string, optionId: string | undefined) => void;
}>;

/** This computer: the host the in-process backend serves. */
export const LOCAL_HOST: HostInfo = { id: "local", name: "This computer", kind: "local" };

/** A listed Session as the app shows it; ported from the WebUI controller's `sessionSummary`. */
function sessionSummary(session: SessionInfo): SessionSummary {
  const meta = session._meta?.[NAMESPACE];
  const details = typeof meta === "object" && meta !== null && !Array.isArray(meta) ? (meta as Record<string, unknown>) : {};
  const edge = typeof details.ownership === "object" && details.ownership !== null ? (details.ownership as Record<string, unknown>) : {};
  const ownership =
    typeof edge.sessionId === "string" && typeof edge.name === "string" && typeof edge.nickname === "string"
      ? { sessionId: edge.sessionId as SessionId, name: edge.name, nickname: edge.nickname }
      : undefined;
  return {
    sessionId: session.sessionId,
    cwd: session.cwd,
    title: session.title?.trim() || session.sessionId,
    ...(ownership === undefined ? {} : { ownership }),
    ...(typeof details.createdAtMs === "string" ? { createdAtMs: details.createdAtMs as TimestampMs } : {}),
    ...(typeof details.modifiedAtMs === "string" ? { modifiedAtMs: details.modifiedAtMs as TimestampMs } : {}),
    ...(typeof details.parentSessionId === "string" ? { parentSessionId: details.parentSessionId as SessionId } : {}),
  };
}

/** Creates a host store over `transport`; nothing connects until `start`. */
export function createHostStore(
  info: HostInfo,
  transport: AcpTransport,
  options: Readonly<{ requestTimeoutMs?: number }> = {},
): HostStore {
  const timeoutMs = options.requestTimeoutMs ?? REQUEST_TIMEOUT_MS;
  const methods = AcpMethods.forNamespace(NAMESPACE);
  const phase = signal<HostPhase>({ kind: "starting" });
  const workspace = signal<WorkspaceState>(initialWorkspaceState);
  const returnedText = signal<ReadonlyMap<SessionId, string>>(new Map());
  // Sessions whose transcript was replayed on this connection.
  const attached = new Set<SessionId>();
  // Resolvers of queued permission dialogs; `undefined` dismisses without choosing.
  const permissionAnswers = new Map<string, (optionId: string | undefined) => void>();
  let permissionSequence = 0;

  /** Applies one workspace action. */
  const dispatch = (action: WorkspaceAction) => workspace.set(reduceWorkspace(workspace(), action));
  const store: WorkspaceStoreAccess = { getState: () => workspace(), dispatch };

  const connection = new AcpConnection(transport, {
    notifications: (notifications) => applyNotifications(notifications),
    diagnostic: (message) => console.warn(`ACP (${info.name}): ${message}`),
    closed: (reason) => {
      // After it was ready, its Sessions stay readable offline; before, the host never started.
      const wasReady = phase().kind === "ready";
      phase.set({ kind: wasReady ? "offline" : "failed", reason: `Backend stopped: ${reason}` });
      // Nothing finishes a turn on a closed connection: no spinner may keep turning for one.
      for (const sessionId of workspace().runningSessionIds) {
        dispatch({ type: "session/updated", sessionId, action: { type: "running/changed", running: false } });
      }
    },
    activity: () => {},
  });

  /** Reads and applies one Session's run state. */
  const refreshRuntime = async (sessionId: SessionId) => {
    const runtime = await connection.request<SessionRuntimeSnapshot>(methods.sessionRuntime, { sessionId });
    dispatch({ type: "session/updated", sessionId, action: { type: "running/changed", running: runtime.running } });
  };

  /** Re-lists the Sessions. */
  const refreshSessions = async () => {
    const listed = await connection.request<SessionListResult>(AcpProtocol.methods.sessionList, {}, { timeoutMs });
    dispatch({ type: "session/listed", sessions: listed.sessions.map(sessionSummary) });
  };

  const router = new SessionUpdateRouter(NAMESPACE, store, refreshRuntime, async () => {}, refreshSessions);

  /** Routes `session/update` notifications (batches expanded) into the workspace. */
  const applyNotifications = (notifications: readonly AcpNotification[]) => {
    const updates: SessionUpdateNotification[] = [];
    for (const notification of notifications) {
      if (notification.method !== AcpProtocol.methods.sessionUpdate) continue;
      const update = AcpProtocol.decodeRecord(notification.params, "session/update") as SessionUpdateNotification;
      updates.push(...AcpProtocol.expandSessionUpdateBatch(update, NAMESPACE));
    }
    if (updates.length > 0) router.applyMany(updates);
  };

  /** Queues one agent `session/request_permission` and resolves with the user's outcome; ported from the WebUI controller. */
  const requestPermission = (params: unknown, abort: AbortSignal): Promise<unknown> => {
    const request = AcpProtocol.decodeRecord(params, "session/request_permission params");
    const options = Array.isArray(request.options)
      ? request.options.flatMap((option: unknown) => {
          if (typeof option !== "object" || option === null) return [];
          const { optionId, name, kind } = option as Readonly<Record<string, unknown>>;
          return typeof optionId === "string" && typeof name === "string" && typeof kind === "string" ? [{ optionId, name, kind }] : [];
        })
      : [];
    if (typeof request.sessionId !== "string" || typeof request.title !== "string" || options.length === 0) {
      throw new AcpRequestError({ code: -32602, message: "session/request_permission needs a Session, a title, and options" });
    }
    const key = `permission-${++permissionSequence}`;
    const prompt: PermissionPrompt = {
      key,
      sessionId: request.sessionId as SessionId,
      title: request.title,
      ...(typeof request.description === "string" ? { description: request.description } : {}),
      options: options as PermissionPrompt["options"],
    };
    const outcome = (optionId: string | undefined) => ({
      outcome: optionId === undefined ? { outcome: "cancelled" } : { outcome: "selected", optionId },
    });
    return new Promise((resolve) => {
      permissionAnswers.set(key, (optionId) => resolve(outcome(optionId)));
      // Answered elsewhere, or the work stopped: withdraw the card and report it dismissed.
      abort.addEventListener(
        "abort",
        () => {
          if (!permissionAnswers.delete(key)) return;
          dispatch({ type: "permission/settled", key });
          resolve(outcome(undefined));
        },
        { once: true },
      );
      dispatch({ type: "permission/asked", prompt });
    });
  };
  connection.handle(AcpProtocol.methods.sessionRequestPermission, requestPermission);

  /** Connects, initializes and lists the Sessions, once. */
  const connectOnce = async () => {
    try {
      await connection.connect();
    } catch (error: unknown) {
      phase.set({ kind: "failed", reason: `Backend failed to start: ${messageOf(error)}` });
      return;
    }
    try {
      await connection.request(
        AcpProtocol.methods.initialize,
        {
          protocolVersion: 2,
          info: { name: "clawcode-desktop", version: "0.1.0" },
          capabilities: {},
          _meta: { [NAMESPACE]: { sessionUpdateBatch: { version: 1 } } },
        },
        { timeoutMs },
      );
      await refreshSessions();
      phase.set({ kind: "ready" });
    } catch (error: unknown) {
      phase.set({ kind: "failed", reason: `Backend did not answer: ${messageOf(error)}` });
    }
  };
  // A connection connects once: repeated `start` calls share the first attempt
  // (R1's retry will make a fresh connection per attempt).
  let started: Promise<void> | undefined;

  /** Replays a Session unless it is running and already attached; ported from the WebUI controller's `loadAttachedSession`. */
  const openSession = async (sessionId: SessionId) => {
    const session = workspace().sessions.find((item) => item.sessionId === sessionId);
    if (session === undefined) throw new Error(`Session ${sessionId} is not listed`);
    const runtime = await connection.request<SessionRuntimeSnapshot>(methods.sessionRuntime, { sessionId });
    /** Applies the run state; after a replay, because clearing the transcript resets it. */
    const applyRunning = () => dispatch({ type: "session/updated", sessionId, action: { type: "running/changed", running: runtime.running } });
    if (attached.has(sessionId) && runtime.running) {
      applyRunning();
      return;
    }
    // Idle Sessions replay on every open: another client may have changed the transcript.
    router.requireFullReplay(sessionId);
    dispatch({ type: "session/updated", sessionId, action: { type: "transcript/cleared" } });
    const result = await connection.request<Readonly<{ configOptions?: unknown }> | null>(AcpProtocol.methods.sessionResume, {
      sessionId,
      cwd: session.cwd,
      replayFrom: { type: "start" },
    });
    const options = AcpProtocol.configOptions(result?.configOptions);
    if (options !== undefined) dispatch({ type: "session/updated", sessionId, action: { type: "config/replaced", options } });
    applyRunning();
    attached.add(sessionId);
  };

  return {
    info,
    phase,
    workspace,
    sessions: () => workspace().sessions,
    start: () => (started ??= connectOnce()),
    stop: () => connection.close(),
    openSession,
    newSession: async (cwd) => {
      const result = await connection.request<NewSessionResult>(AcpProtocol.methods.sessionNew, { cwd });
      await refreshSessions();
      await openSession(result.sessionId);
      return result.sessionId;
    },
    prompt: async (sessionId, text) => {
      try {
        await connection.request(AcpProtocol.methods.sessionPrompt, {
          sessionId,
          prompt: [{ type: "text", text }],
          _meta: { [NAMESPACE]: { submissionId: crypto.randomUUID() } },
        });
      } catch (error: unknown) {
        // Nothing reached history, or its outcome is unknown: hand the text back so it is not lost.
        returnedText.update((map) => new Map(map).set(sessionId, text));
        dispatch({
          type: "notice/added",
          notice: { id: `prompt-${crypto.randomUUID()}`, sessionId, title: t("notice.promptFailed"), description: messageOf(error) },
        });
        if (workspace().sessionWorkspaces.get(sessionId)?.running === true) {
          dispatch({ type: "session/updated", sessionId, action: { type: "running/changed", running: false } });
        }
      }
    },
    returnedText,
    takeReturnedText: (sessionId) => {
      const text = returnedText().get(sessionId);
      if (text !== undefined) {
        returnedText.update((map) => {
          const next = new Map(map);
          next.delete(sessionId);
          return next;
        });
      }
      return text;
    },
    cancel: (sessionId) => {
      try {
        connection.notify(AcpProtocol.methods.sessionCancel, { sessionId });
      } catch (error: unknown) {
        console.warn(`cancel ${sessionId}: ${messageOf(error)}`);
      }
    },
    answerPermission: (key, optionId) => {
      const resolve = permissionAnswers.get(key);
      permissionAnswers.delete(key);
      dispatch({ type: "permission/settled", key });
      resolve?.(optionId);
    },
  };
}

/** The message of a thrown value. */
function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
