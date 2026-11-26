import { computed, signal } from "nsh";
import type { Accessor } from "nsh";
import { t } from "nsh/i18n";

import { AcpConnection, AcpRequestError } from "../client/acp/connection";
import type { AcpNotification } from "../client/acp/connection";
import { AcpProtocol } from "../client/acp/protocol";
import type { NewSessionResult, SessionId, SessionListResult, SessionUpdateNotification } from "../client/acp/protocol";
import type { PermissionPrompt, SessionNotice, SessionRuntimeSnapshot, SessionSummary } from "../client/domain/model";
import { messageOf } from "../client/errors";
import type { AcpTransport } from "../client/transport";
import type { SessionWorkspaceState } from "../client/workspace/sessionState";
import { initialWorkspaceState, reduceWorkspace } from "../client/workspace/state";
import type { WorkspaceAction, WorkspaceState } from "../client/workspace/state";
import { SessionUpdateRouter } from "../client/workspace/updateRouter";

/** How long `initialize` and the first Session list may take before the screen says so. */
const REQUEST_TIMEOUT_MS = 15_000;

/** The product namespace of ACP `_meta` keys and extension methods. */
const NAMESPACE = "clawcode";

/** The product method that reports whether a Session is running a turn. */
const SESSION_RUNTIME = `_${NAMESPACE}/session/runtime`;

/** Whether a host is this computer or a paired remote one. */
export type HostKind = "local" | "remote";

/** A host's identity: `id` is stable ("local" for this computer, a device key later). */
export type HostInfo = Readonly<{ id: string; name: string; kind: HostKind }>;

/**
 * Why a host is not ready: it could not start, its first requests were not answered, or it stopped
 * after it was ready. `detail` is the underlying error; the view words the cause.
 */
export type HostFailure = Readonly<{ cause: "start" | "unanswered" | "stopped"; detail: string }>;

/** Where the connection to a host stands. */
export type HostPhase =
  | Readonly<{ kind: "starting" }>
  | Readonly<{ kind: "reconnecting" }>
  | Readonly<{ kind: "ready" }>
  | Readonly<{ kind: "failed"; failure: HostFailure }>
  // Was ready, then lost its backend: its Sessions stay on screen, read-only in effect (requests fail).
  | Readonly<{ kind: "offline"; failure: HostFailure }>;

/** One host's connection and the Sessions it serves; its methods are the only writers. */
export type HostStore = Readonly<{
  info: HostInfo;
  phase: Accessor<HostPhase>;
  /** Everything the host's Sessions show, reduced from ACP updates. */
  workspace: Accessor<WorkspaceState>;
  /** The listed Sessions; titles fall back to the Session id. */
  sessions: Accessor<readonly SessionSummary[]>;
  /** One opened Session's projection; `undefined` until it is opened. */
  session: (sessionId: SessionId) => SessionWorkspaceState | undefined;
  /** Whether a Session is running a turn. */
  running: (sessionId: SessionId) => boolean;
  /** The oldest approval a Session is waiting for. */
  permissionFor: (sessionId: SessionId) => PermissionPrompt | undefined;
  /** Notices not yet shown; `dismissNotice` takes one off once it is. */
  notices: Accessor<readonly SessionNotice[]>;
  dismissNotice: (id: string) => void;
  /** Connects, initializes and lists the Sessions, retaining the previous connection's readable cache on retry. */
  start: (previous?: HostStore) => Promise<void>;
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

/** Creates a host store over `transport`; nothing connects until `start`. */
export function createHostStore(
  info: HostInfo,
  transport: AcpTransport,
  options: Readonly<{ requestTimeoutMs?: number }> = {},
): HostStore {
  const timeoutMs = options.requestTimeoutMs ?? REQUEST_TIMEOUT_MS;
  const phase = signal<HostPhase>({ kind: "starting" });
  const workspace = signal<WorkspaceState>(initialWorkspaceState);
  const returnedText = signal<ReadonlyMap<SessionId, string>>(new Map());
  // Sessions whose transcript was replayed on this connection.
  const attached = new Set<SessionId>();
  const opening = new Map<SessionId, Promise<void>>();
  let hasCache = false;
  // Resolvers of queued permission dialogs; `undefined` dismisses without choosing.
  const permissionAnswers = new Map<string, (optionId: string | undefined) => void>();
  let permissionSequence = 0;

  // Each part of the workspace its own value: a streamed token replaces the workspace, and only
  // readers of the part it changed (the Session's projection) run again.
  const sessions = computed(() => workspace().sessions);
  const sessionWorkspaces = computed(() => workspace().sessionWorkspaces);
  const permissions = computed(() => workspace().permissions);
  const notices = computed(() => workspace().notices);
  const runningIds = computed<ReadonlySet<SessionId>>(
    () => new Set([...sessionWorkspaces()].flatMap(([sessionId, session]) => (session.running ? [sessionId] : []))),
    { equals: (left, right) => left.size === right.size && [...left].every((sessionId) => right.has(sessionId)) },
  );

  /** Applies one workspace action. */
  const dispatch = (action: WorkspaceAction) => workspace.set(reduceWorkspace(workspace(), action));
  /** Sets whether a Session runs a turn. */
  const setRunning = (sessionId: SessionId, running: boolean) =>
    dispatch({ type: "session/updated", sessionId, action: { type: "running/changed", running } });

  const connection = new AcpConnection(transport, {
    notification: (notification) => applyNotification(notification),
    // Remote frame parse errors can include peer-provided text. Log only the affected connection.
    diagnostic: (message) => console.warn(info.kind === "remote"
      ? `ACP protocol diagnostic for remote host ${info.id.slice(0, 12)}`
      : `ACP (${info.name}): ${message}`),
    closed: (reason) => {
      // After it was ready, its Sessions stay readable offline; before, the host never started.
      phase.set({ kind: hasCache ? "offline" : "failed", failure: { cause: "stopped", detail: reason } });
      dispatch({ type: "sessions/stopped" });
    },
  });

  /** Reads one Session's run state. */
  const readRuntime = (sessionId: SessionId) => connection.request<SessionRuntimeSnapshot>(SESSION_RUNTIME, { sessionId });

  /** Re-lists the Sessions. */
  const refreshSessions = async () => {
    const listed = await connection.request<SessionListResult>(AcpProtocol.methods.sessionList, {}, { timeoutMs });
    dispatch({ type: "session/listed", sessions: listed.sessions.map((session) => AcpProtocol.sessionSummary(session, NAMESPACE)) });
  };

  const router = new SessionUpdateRouter(NAMESPACE, { getState: () => workspace(), dispatch }, {
    runtime: async (sessionId) => setRunning(sessionId, (await readRuntime(sessionId)).running),
    sessions: refreshSessions,
  });

  /** Routes a `session/update` notification (a batch expanded) into the workspace. */
  const applyNotification = (notification: AcpNotification) => {
    if (notification.method !== AcpProtocol.methods.sessionUpdate) return;
    const update = AcpProtocol.decodeRecord(notification.params, "session/update") as SessionUpdateNotification;
    router.applyMany(AcpProtocol.expandSessionUpdateBatch(update, NAMESPACE));
  };

  /** Queues one agent `session/request_permission` and resolves with the user's outcome. */
  const requestPermission = (params: unknown, abort: AbortSignal): Promise<unknown> => {
    const request = AcpProtocol.permissionRequest(params);
    if (request === undefined) {
      throw new AcpRequestError({ code: -32602, message: "session/request_permission needs a Session, a title, and options" });
    }
    const key = `permission-${++permissionSequence}`;
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
      dispatch({ type: "permission/asked", prompt: { key, ...request } });
    });
  };
  connection.handle(AcpProtocol.methods.sessionRequestPermission, requestPermission);

  /** Connects, initializes and lists the Sessions, once. */
  const connectOnce = async (previous?: HostStore) => {
    if (previous && ["ready", "offline", "reconnecting"].includes(previous.phase().kind)) {
      // Only local projections survive reconnect. ACP request IDs, approvals and replay cursors belong to the old wire.
      const { sessions, sessionWorkspaces, notices } = previous.workspace();
      workspace.set(reduceWorkspace({ sessions, sessionWorkspaces, notices, permissions: [] }, { type: "sessions/stopped" }));
      returnedText.set(previous.returnedText());
      hasCache = true;
      phase.set({ kind: "reconnecting" });
    }
    try {
      // iroh may use up to 20 s for discovery/dial and another 100 s for the native handshake.
      // Allow that layer to report the actual failure instead of truncating it at the local boot timeout.
      await connection.connect(info.kind === "remote" ? 125_000 : undefined);
    } catch (error: unknown) {
      phase.set({ kind: hasCache ? "offline" : "failed", failure: { cause: "start", detail: messageOf(error) } });
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
      hasCache = true;
      phase.set({ kind: "ready" });
    } catch (error: unknown) {
      // A peer that could not initialize ACP must not keep a live remote-control slot on the host.
      if (info.kind === "remote") connection.close();
      phase.set({ kind: hasCache ? "offline" : "failed", failure: { cause: "unanswered", detail: messageOf(error) } });
    }
  };
  // Repeated starts share this attempt. A retry owns a new connection and request numbering.
  let started: Promise<void> | undefined;

  /** Replays a Session unless it is running and already attached; ported from the WebUI controller's `loadAttachedSession`. */
  const replaySession = async (sessionId: SessionId) => {
    const session = workspace().sessions.find((item) => item.sessionId === sessionId);
    if (session === undefined) throw new Error(`Session ${sessionId} is not listed`);
    const { running } = await readRuntime(sessionId);
    // Before the replay, so live run-state updates arriving during it win.
    setRunning(sessionId, running);
    if (attached.has(sessionId) && running) return;
    // Idle Sessions replay on every open: another client may have changed the transcript.
    attached.delete(sessionId);
    await router.replay(sessionId, () => connection.request(AcpProtocol.methods.sessionResume, {
      sessionId, cwd: session.cwd, replayFrom: { type: "start" },
    }));
    attached.add(sessionId);
  };

  /** Shares an in-flight replay across navigation, reconnect and new-Session callbacks. */
  const openSession = (sessionId: SessionId) => {
    const pending = opening.get(sessionId);
    if (pending) return pending;
    const replay = replaySession(sessionId).finally(() => opening.delete(sessionId));
    opening.set(sessionId, replay);
    return replay;
  };

  return {
    info,
    phase,
    workspace,
    sessions,
    session: (sessionId) => sessionWorkspaces().get(sessionId),
    running: (sessionId) => runningIds().has(sessionId),
    permissionFor: (sessionId) => permissions().find((prompt) => prompt.sessionId === sessionId),
    notices,
    dismissNotice: (id) => dispatch({ type: "notice/dismissed", id }),
    start: (previous) => (started ??= connectOnce(previous)),
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
        setRunning(sessionId, false);
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
