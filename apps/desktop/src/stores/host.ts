import { computed, signal } from "nsh";
import type { Accessor } from "nsh";
import { t } from "nsh/i18n";

import { decodeCommands } from "../client/acp/commands";
import type { AvailableCommand } from "../client/acp/commands";
import { AcpConnection, AcpRequestError } from "../client/acp/connection";
import type { AcpNotification } from "../client/acp/connection";
import { promptBlocks } from "../client/acp/prompt";
import type { PromptDraft, PromptImage } from "../client/acp/prompt";
import { WorkspaceFiles } from "../client/acp/workspaceFiles";
import { AcpProtocol } from "../client/acp/protocol";
import type { MessageId, NewSessionResult, SessionId, SessionListResult, SessionUpdateNotification } from "../client/acp/protocol";
import { decodeSessionConfig } from "../client/acp/sessionConfig";
import type { SessionConfigSelect } from "../client/acp/sessionConfig";
import type { AuthMethod, PermissionPrompt, ProviderSignIn, SessionNotice, SessionRuntimeSnapshot, SessionSummary, SignInFlow } from "../client/domain/model";
import { messageOf } from "../client/errors";
import type { AcpTransport } from "../client/transport";
import type { SessionWorkspaceState } from "../client/workspace/sessionState";
import { initialWorkspaceState, reduceWorkspace } from "../client/workspace/state";
import type { WorkspaceAction, WorkspaceState } from "../client/workspace/state";
import { SessionUpdateRouter } from "../client/workspace/updateRouter";
import { initialInteractions, openQuestions, questionId, queuedQuestions } from "../client/workspace/interactions";
import type { QuestionKey, SessionQuestion } from "../client/workspace/interactions";

/** How long `initialize` and the first Session list may take before the screen says so. */
const REQUEST_TIMEOUT_MS = 15_000;

/** The product namespace of ACP `_meta` keys and extension methods. */
const NAMESPACE = "clawcode";

/** The product method that reports whether a Session is running a turn. */
const SESSION_RUNTIME = `_${NAMESPACE}/session/runtime`;

/** A stable empty selection for a Session whose host has not advertised settings. */
const EMPTY_CONFIG: readonly SessionConfigSelect[] = [];
const EMPTY_COMMANDS: readonly AvailableCommand[] = [];

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

/** Identifies a recorded edit without retaining patch data across a branch replay. */
export type ChangeSelection = Readonly<{ turnKey: string; path: string; editId: string }>;

/** The visible workspace tab and recorded edit belong to the host and session that opened them. */
export type SessionPanel = Readonly<{ tab: "files" | "changes" | "record"; change?: ChangeSelection }>;

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
  /** Commands available in this Session, ordered by its agent. */
  commandsFor: (sessionId: SessionId) => readonly AvailableCommand[];
  /** The settings that this host last confirmed for one Session. */
  sessionConfig: (sessionId: SessionId) => readonly SessionConfigSelect[];
  /** The sign-in methods the host advertised at `initialize`; only the local host offers any. */
  authMethods: Accessor<readonly AuthMethod[]>;
  /** Asks the host which sign-in methods have a usable credential. */
  signInStatuses: () => Promise<readonly ProviderSignIn[]>;
  /** The sign-in in progress on this host, with its device code once shown; one at a time. */
  signInFlow: Accessor<SignInFlow | null>;
  /** Runs one provider sign-in to its end; rejects when it fails or is cancelled. */
  signIn: (methodId: string) => Promise<void>;
  /** Gives up the sign-in in progress: declines its device code and cancels the login. */
  cancelSignIn: () => void;
  /** The settings a new Session on this host starts with, for a draft that has no Session yet. */
  defaultConfig: Accessor<readonly SessionConfigSelect[]>;
  /** Whether a settings write is awaiting a response for this Session. */
  configPending: (sessionId: SessionId) => boolean;
  /** Applies one advertised choice and rejects if the host could not confirm the change. */
  setSessionConfig: (sessionId: SessionId, configId: string, value: string) => Promise<void>;
  /** Whether a Session is running a turn. */
  running: (sessionId: SessionId) => boolean;
  /** Asks the host whether a Session runs, without attaching it; for Sessions this connection never opened. */
  queryRunning: (sessionId: SessionId) => Promise<boolean>;
  /** The oldest approval a Session is waiting for. */
  permissionFor: (sessionId: SessionId) => PermissionPrompt | undefined;
  /** Asks still accepting an answer, excluding replies already in the host's queue. */
  openQuestions: (sessionId: SessionId) => readonly SessionQuestion[];
  /** Whether the sidebar should mark this Session as awaiting an answer. */
  awaitsAnswer: (sessionId: SessionId) => boolean;
  /** Whether this connection already submitted a reply or skip for the question. */
  questionPending: (sessionId: SessionId, key: QuestionKey) => boolean;
  /** Answers with exactly one text block, including while another turn is running. */
  answerQuestion: (sessionId: SessionId, key: QuestionKey, text: string) => Promise<void>;
  /** Skips one or more open questions in a single kernel transaction. */
  dismissQuestions: (sessionId: SessionId, keys: readonly QuestionKey[]) => Promise<void>;
  /** Notices not yet shown; `dismissNotice` takes one off once it is. */
  notices: Accessor<readonly SessionNotice[]>;
  dismissNotice: (id: string) => void;
  /** Connects, initializes and lists the Sessions, retaining the previous connection's readable cache on retry. */
  start: (previous?: HostStore) => Promise<void>;
  /** Closes the connection. */
  stop: () => void;
  /** Shows a Session: replays its transcript unless it is running and already attached. */
  openSession: (sessionId: SessionId) => Promise<void>;
  /** Retitles a Session on the host; the list shows the host's trimmed title once it confirms. */
  renameSession: (sessionId: SessionId, title: string) => Promise<void>;
  /** Deletes a Session on the host, then lists the Sessions again. */
  deleteSession: (sessionId: SessionId) => Promise<void>;
  /** Creates a Session in `cwd`, refreshes the list and opens it; its id. */
  newSession: (cwd: string) => Promise<SessionId>;
  /** Sends text and selected images; a failed turn returns the complete draft and adds a notice. */
  prompt: (sessionId: SessionId, text: string, images?: readonly PromptImage[]) => Promise<void>;
  /**
   * Asks again from before user message `messageId`: moves the Session's branch to that message's parent, replays it,
   * then starts the edited draft as a prompt and resolves. Rejects, with a notice, when the branch could not move; the
   * draft is then still the caller's.
   */
  editPrompt: (sessionId: SessionId, messageId: MessageId, draft: PromptDraft) => Promise<void>;
  /** Reads one page of a Session's trajectory; rejects with the host's error (`historyUnavailable` when it keeps no history). */
  trajectory: (sessionId: SessionId, query: Readonly<{ before?: number; after?: number }>) => Promise<unknown>;
  /** A request's system prompt or tool list by hash; `undefined` when the host no longer has it. */
  requestContent: (sessionId: SessionId, hash: string) => Promise<string | undefined>;
  /** Forks the Session up to assistant message `messageId` into a new listed Session; its id. Rejects, with a notice, on failure. */
  fork: (sessionId: SessionId, messageId: MessageId) => Promise<SessionId>;
  /** Each Session on this host remembers its own open inspector. */
  panels: Accessor<ReadonlyMap<SessionId, SessionPanel>>;
  files: WorkspaceFiles;
  setPanel: (sessionId: SessionId, panel?: SessionPanel) => void;
  returnedDrafts: Accessor<ReadonlyMap<SessionId, PromptDraft>>;
  /** Consumes both parts of the failed prompt in one operation. */
  takeReturnedDraft: (sessionId: SessionId) => PromptDraft | undefined;
  /** Asks the agent to stop the running turn. */
  cancel: (sessionId: SessionId) => void;
  /** Answers a queued permission request; `undefined` dismisses it. */
  answerPermission: (key: string, optionId: string | undefined) => void;
}>;

/** This computer: the host the in-process backend serves. */
export const LOCAL_HOST: HostInfo = { id: "local", name: "This computer", kind: "local" };

/** A created Session whose directory or attachment must be refreshed before sending its first prompt. */
export class SessionPreparationError extends Error {
  /** Retains the confirmed id while surfacing the failed read to the original draft. */
  constructor(readonly sessionId: SessionId, reason: unknown) {
    super(messageOf(reason));
  }
}

/** A reservation read already exhausted its retries against a complete, unpublished history. */
class QuestionReservationError extends Error {}

/** Creates a host store over `transport`; nothing connects until `start`. */
export function createHostStore(
  info: HostInfo,
  transport: AcpTransport,
  options: Readonly<{ requestTimeoutMs?: number }> = {},
): HostStore {
  const timeoutMs = options.requestTimeoutMs ?? REQUEST_TIMEOUT_MS;
  const phase = signal<HostPhase>({ kind: "starting" });
  const workspace = signal<WorkspaceState>(initialWorkspaceState);
  // Keep the previous wire's read-only Sessions visible until the replacement handshake succeeds.
  // Meanwhile the router applies even the earliest notifications to this connection's own ledger.
  const offlineSessions = signal<Pick<WorkspaceState, "sessions" | "sessionWorkspaces" | "awaitingAnswers"> | undefined>(undefined);
  const readableWorkspace = computed(() => {
    const cached = offlineSessions();
    return cached ? { ...workspace(), ...cached } : workspace();
  });
  const panels = signal<ReadonlyMap<SessionId, SessionPanel>>(new Map());
  const returnedDrafts = signal<ReadonlyMap<SessionId, PromptDraft>>(new Map());
  /** Consume the text and images together so retries and reconnects cannot lose part of a failed submission. */
  const takeReturnedDraft = (sessionId: SessionId) => {
    const draft = returnedDrafts().get(sessionId);
    if (draft) returnedDrafts.update((map) => {
      const next = new Map(map);
      next.delete(sessionId);
      return next;
    });
    return draft;
  };
  const configWrites = signal<ReadonlySet<SessionId>>(new Set());
  // Sessions whose transcript was replayed on this connection.
  const attached = new Set<SessionId>();
  const opening = new Map<SessionId, Promise<void>>();
  const replying = signal<ReadonlyMap<SessionId, ReadonlySet<string>>>(new Map());
  let hasCache = false;
  // Resolvers of queued permission dialogs; `undefined` dismisses without choosing.
  const permissionAnswers = new Map<string, (optionId: string | undefined) => void>();
  let permissionSequence = 0;

  // Each part of the workspace its own value: a streamed token replaces the workspace, and only
  // readers of the part it changed (the Session's projection) run again.
  const sessions = computed(() => readableWorkspace().sessions);
  const sessionWorkspaces = computed(() => readableWorkspace().sessionWorkspaces);
  const awaitingAnswers = computed(() => readableWorkspace().awaitingAnswers);
  const sessionCommands = computed(() => workspace().sessionCommands);
  const sessionConfigs = computed(() => workspace().sessionConfigs);
  const permissions = computed(() => workspace().permissions);
  // Sessions whose `session/prompt` awaits its response. A Run that fails before insertion is also reported by that
  // response (with the draft handed back), so its own notice waits for the response: dropped on a refusal, shown once
  // the prompt was accepted.
  const promptsInFlight = signal<ReadonlyMap<SessionId, number>>(new Map());
  const notices = computed(() => workspace().notices.filter((notice) => !(notice.fromRun && promptsInFlight().has(notice.sessionId))));
  const runningIds = computed<ReadonlySet<SessionId>>(
    () => new Set([...sessionWorkspaces()].flatMap(([sessionId, session]) => (session.running ? [sessionId] : []))),
    { equals: (left, right) => left.size === right.size && [...left].every((sessionId) => right.has(sessionId)) },
  );

  /** Applies one workspace action. */
  const dispatch = (action: WorkspaceAction) => workspace.set(reduceWorkspace(workspace(), action));
  /** Sets whether a Session runs a turn. */
  const setRunning = (sessionId: SessionId, running: boolean) =>
    dispatch({ type: "session/updated", sessionId, action: { type: "running/changed", running } });

  /** Installs a snapshot from a response; only a settings write requires the host to return one. */
  const applyConfigResult = (sessionId: SessionId, result: unknown, required = false) => {
    const options = decodeSessionConfig(AcpProtocol.record(result)?.configOptions);
    if (options !== undefined) dispatch({ type: "config/replaced", sessionId, options });
    else if (required) throw new Error("Invalid configOptions in the host response");
  };

  /** Apply the Session setup response in wire order, interleaved with live capability notifications. */
  const applySessionResult = (sessionId: SessionId, result: unknown) => {
    applyConfigResult(sessionId, result);
    const commands = decodeCommands(AcpProtocol.record(result)?.availableCommands);
    if (commands !== undefined) dispatch({ type: "commands/replaced", sessionId, commands });
  };

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

  /** Applies a valid runtime response before any subsequent live notifications on the same wire. */
  const readRuntime = async (sessionId: SessionId, branch: number): Promise<void> => {
    await connection.request<SessionRuntimeSnapshot>(SESSION_RUNTIME, { sessionId }, { timeoutMs, onResult: (result) => {
      if (router.branchVersion(sessionId) !== branch) return;
      if (typeof result?.running !== "boolean" || (result.sessionId !== undefined && result.sessionId !== sessionId)) {
        throw new Error("Invalid session runtime snapshot");
      }
      setRunning(sessionId, result.running);
    } });
  };

  /** Re-lists the Sessions. */
  const defaultConfig = signal<readonly SessionConfigSelect[]>(EMPTY_CONFIG);
  const authMethods = signal<readonly AuthMethod[]>([]);
  /** Reads what a new Session starts with; a host without the method keeps the generic draft label. */
  const readDefaultConfig = async () => {
    try {
      const reply = await connection.request<unknown>(`_${NAMESPACE}/session/default_config`, {}, { timeoutMs });
      defaultConfig.set(decodeSessionConfig(reply) ?? EMPTY_CONFIG);
    } catch (error) {
      console.warn(`default session settings on host ${info.id.slice(0, 12)}: ${messageOf(error)}`);
    }
  };
  const refreshSessions = async () => {
    const listed = await connection.request<SessionListResult>(AcpProtocol.methods.sessionList, {}, { timeoutMs });
    dispatch({ type: "session/listed", sessions: listed.sessions.map((session) => AcpProtocol.sessionSummary(session, NAMESPACE)) });
  };

  const router: SessionUpdateRouter = new SessionUpdateRouter(NAMESPACE, { getState: () => workspace(), dispatch }, {
    branch: (sessionId) => {
      attached.delete(sessionId);
      return openSession(sessionId);
    },
    runtime: (sessionId) => readRuntime(sessionId, router.branchVersion(sessionId)),
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

  const signInFlow = signal<SignInFlow | null>(null);
  let signInAbort: AbortController | undefined;
  // Every unanswered device code; the newest one is on screen. A host may issue a fresh code before withdrawing the old.
  const pendingCodes = new Set<() => void>();
  let shownCode: (() => void) | undefined;
  /** Declines every device code still waiting for the user. */
  const declineCodes = () => {
    for (const decline of [...pendingCodes]) decline();
  };
  // The `auth/login` request in flight: only an elicitation scoped to it is that sign-in's device code.
  let loginRequestId: number | undefined;
  /**
   * Shows the device code a sign-in asks for (`elicitation/create` in URL mode, scoped to the `auth/login` request)
   * until the user gives up or the host withdraws it once the sign-in ends. Other elicitations, such as an MCP
   * tool's URL authorization in a Session, have no surface here and are cancelled.
   */
  const elicit = (params: unknown, abort: AbortSignal): Promise<unknown> => {
    const request = AcpProtocol.record(params);
    const flow = signInFlow();
    if (request?.mode !== "url" || typeof request.url !== "string" || flow === null
      // Exactly the login's request scope: a Session or tool scope belongs to someone else, whatever its request id.
      || loginRequestId === undefined || request.requestId !== loginRequestId
      || request.sessionId !== undefined || request.toolCallId !== undefined) return Promise.resolve({ action: "cancel" });
    const url = request.url;
    const userCode = AcpProtocol.productMeta(request._meta, NAMESPACE)?.userCode;
    return new Promise((resolve) => {
      let answered = false;
      /** Answers once and, if this code is the one on screen, takes it off; the sign-in itself may still be finishing. */
      const finish = (action: "decline" | "cancel") => {
        if (answered) return;
        answered = true;
        pendingCodes.delete(decline);
        if (shownCode === decline) {
          shownCode = undefined;
          if (signInFlow()?.code !== undefined) signInFlow.set({ methodId: flow.methodId });
        }
        resolve({ action });
      };
      /** The user gave up on this code. */
      const decline = () => finish("decline");
      pendingCodes.add(decline);
      shownCode = decline;
      abort.addEventListener("abort", () => finish("cancel"), { once: true });
      signInFlow.set({ methodId: flow.methodId, code: {
        url, ...(typeof userCode === "string" ? { userCode } : {}), message: typeof request.message === "string" ? request.message : "",
      } });
    });
  };
  connection.handle("elicitation/create", elicit);
  /** The notice for a request the host refused until the user signs in to `methodId`. */
  const signInNotice = (sessionId: SessionId, methodId: string): SessionNotice => ({
    id: `sign-in-${crypto.randomUUID()}`, sessionId, signIn: methodId,
    title: t("auth.required", { name: authMethods().find((method) => method.id === methodId)?.name ?? methodId }),
  });

  /** Connects, initializes and lists the Sessions, once. */
  const connectOnce = async (previous?: HostStore) => {
    if (previous?.info.id === info.id && previous.info.kind === info.kind
      && ["ready", "offline", "reconnecting"].includes(previous.phase().kind)) {
      // Only local projections survive reconnect. ACP request IDs, approvals and replay cursors belong to the old wire.
      const { sessions, sessionWorkspaces, sessionConfigs, awaitingAnswers, notices } = previous.workspace();
      const cached = reduceWorkspace({ sessions, sessionWorkspaces, sessionConfigs, awaitingAnswers, sessionCommands: new Map(), notices, permissions: [] }, { type: "sessions/stopped" });
      offlineSessions.set({ sessions, sessionWorkspaces: cached.sessionWorkspaces, awaitingAnswers: cached.awaitingAnswers });
      workspace.set(reduceWorkspace(cached, { type: "questions/invalidated" }));
      panels.set(previous.panels());
      returnedDrafts.set(previous.returnedDrafts());
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
      const initialized = await connection.request<unknown>(
        AcpProtocol.methods.initialize,
        {
          protocolVersion: 2,
          info: { name: "clawcode-desktop", version: "0.1.0" },
          // Sign-ins show their device code through a URL elicitation.
          capabilities: { elicitation: { url: {} } },
          _meta: { [NAMESPACE]: { sessionUpdateBatch: { version: 1 }, toolCallHistory: true } },
        },
        { timeoutMs },
      );
      authMethods.set(AcpProtocol.authMethods(initialized));
      await refreshSessions();
      void readDefaultConfig();
      // A close can follow the directory response on the wire before this continuation runs.
      if (phase().kind === "offline" || phase().kind === "failed") return;
      offlineSessions.set(undefined);
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
  const replaySession = async (sessionId: SessionId, branch: number) => {
    const session = workspace().sessions.find((item) => item.sessionId === sessionId);
    if (session === undefined) throw new Error(`Session ${sessionId} is not listed`);
    await readRuntime(sessionId, branch);
    if (router.branchVersion(sessionId) !== branch) return;
    if (attached.has(sessionId) && workspace().sessionWorkspaces.get(sessionId)?.running) return;
    // Idle Sessions replay on every open: another client may have changed the transcript.
    attached.delete(sessionId);
    await router.replay(sessionId, async (history) => {
      await connection.request(AcpProtocol.methods.sessionResume, {
        sessionId, cwd: session.cwd, replayFrom: { type: "start" },
      }, { onResult: (result) => {
        if (router.branchVersion(sessionId) === branch) applySessionResult(sessionId, result);
      } });
      if (router.branchVersion(sessionId) !== branch || !history.hasQuestions()) return;
      // Apply the snapshot in wire order; live queue changes after the result update the same draft.
      const method = `_${NAMESPACE}/session/pending_messages`;
      /** Only a complete reservation snapshot can release the replay's questions for answering. */
      const onResult = (result: unknown) => {
        if (router.branchVersion(sessionId) !== branch) return;
        const change = queuedQuestions(result, NAMESPACE);
        if (!change) throw new Error("Invalid question reservation snapshot");
        history.apply({ type: "interactions/changed", change });
      };
      let questionsOnly = true;
      let retried = false;
      for (;;) {
        try {
          await connection.request(method, questionsOnly ? { sessionId, questionsOnly } : { sessionId }, { timeoutMs, onResult });
          return;
        } catch (reason: unknown) {
          if (phase().kind !== "ready") throw reason;
          if (router.branchVersion(sessionId) !== branch || !history.hasQuestions()) return;
          // An older peer may require the full queue format. Keep using that format if the read needs a retry.
          if (questionsOnly && reason instanceof AcpRequestError && reason.rpc.code === -32602) {
            questionsOnly = false;
            continue;
          }
          if (retried) throw new QuestionReservationError(messageOf(reason));
          retried = true;
          console.warn(`Retrying question reservations for Session ${sessionId} on host ${info.id.slice(0, 12)}`);
        }
      }
    });
    if (router.branchVersion(sessionId) === branch) attached.add(sessionId);
  };

  /** Shares an in-flight replay across navigation, reconnect and new-Session callbacks. */
  const openSession = (sessionId: SessionId): Promise<void> => {
    const pending = opening.get(sessionId);
    if (pending) return pending;
    // ACP history frames have no request ID. Keep a single resume on the wire and let all waiters
    // follow it through any intervening navigations until they have the latest complete branch.
    const replay = Promise.resolve().then(async () => {
      try {
        let retriedBranch: number | undefined;
        for (;;) {
          const branch = router.branchVersion(sessionId);
          try {
            await replaySession(sessionId, branch);
            if (router.branchVersion(sessionId) === branch) return;
          } catch (reason: unknown) {
            if (router.branchVersion(sessionId) !== branch) continue;
            const reservationsFailed = reason instanceof QuestionReservationError;
            const branchFailed = branch > 0 && router.needsBranchReplay(sessionId);
            if (phase().kind === "ready" && (reservationsFailed || branchFailed)) {
              if (!reservationsFailed && retriedBranch !== branch) {
                retriedBranch = branch;
                console.warn(`Retrying the active branch for Session ${sessionId} on host ${info.id.slice(0, 12)}`);
                continue;
              }
              const title = workspace().sessions.find((session) => session.sessionId === sessionId)?.title ?? sessionId;
              dispatch({ type: "notice/added", notice: { id: `replay-${crypto.randomUUID()}`, sessionId,
                title: t("notice.replayFailed", { session: title }), description: t("notice.reopenToRetry") } });
            }
            throw reason;
          }
        }
      } finally {
        opening.delete(sessionId);
      }
    });
    opening.set(sessionId, replay);
    return replay;
  };

  /** Shares validation and in-flight ownership between a one-question reply and a batch skip. */
  const resolveQuestions = async (sessionId: SessionId, keys: readonly QuestionKey[], text?: string) => {
    if (phase().kind !== "ready") throw new Error(t("interactions.offline"));
    if (text !== undefined && !text.trim()) throw new Error(t("interactions.emptyAnswer"));
    // A reconnected Session's replay decides which cached questions are still open.
    if (!attached.has(sessionId) || opening.has(sessionId)) await openSession(sessionId);
    const available = new Set(openQuestions(sessionWorkspaces().get(sessionId)?.interactions ?? initialInteractions)
      .map((question) => questionId(question.key)));
    const ids = keys.map(questionId);
    const requested = new Set(ids);
    const pending = replying().get(sessionId) ?? new Set<string>();
    if (!keys.length || requested.size !== ids.length || ids.some((id) => pending.has(id) || !available.has(id))) {
      throw new Error(t("interactions.closed"));
    }
    replying.update((all) => new Map(all).set(sessionId, new Set([...pending, ...ids])));
    const branch = router.branchVersion(sessionId);
    try {
      if (text === undefined) {
        await connection.request(`_${NAMESPACE}/session/question/dismiss`, { sessionId, items: keys }, { timeoutMs, onResult: (result) => {
          if (router.branchVersion(sessionId) !== branch) return;
          const response = AcpProtocol.record(result);
          if (response && "reservedQuestions" in response) {
            // Held skips can still fail to reach history. Only their durable message settles them permanently.
            const reservations = queuedQuestions(response, NAMESPACE);
            if (!reservations) throw new Error("Invalid question reservation snapshot");
            router.applyInteraction(sessionId, reservations);
          } else if (Array.isArray(response?.skipped)) {
            // Earlier acknowledgements only named accepted skips. The oldest peers send durable events alone.
            const skipped = response.skipped.flatMap((item: unknown) => {
              const key = AcpProtocol.record(item);
              if (typeof key?.requestId !== "string" || typeof key.index !== "number") return [];
              const id = questionId({ requestId: key.requestId, index: key.index });
              return requested.has(id) ? [id] : [];
            });
            if (skipped.length) router.applyInteraction(sessionId, { type: "settled", ids: skipped });
          }
        } });
      } else {
        // A successful prompt confirms insertion. The kernel forces steering for a question reply.
        await connection.request(AcpProtocol.methods.sessionPrompt, { sessionId, prompt: [{ type: "text", text }],
          _meta: { [NAMESPACE]: { submissionId: crypto.randomUUID(), questionReply: keys[0] } } }, { onResult: () => {
          if (router.branchVersion(sessionId) === branch) router.applyInteraction(sessionId, { type: "settled", ids });
        } });
      }
    } catch (reason: unknown) {
      console.warn(`Could not settle ${keys.length} question(s) for Session ${sessionId} on host ${info.id.slice(0, 12)}`);
      if (reason instanceof AcpRequestError && AcpProtocol.record(reason.rpc.data)?.reason === "questionClosed") {
        // Another connection may have changed branches or queued a reply before its frames reached this one.
        // A full replay plus its pending queue distinguishes a removed ask from a temporarily queued answer.
        if (phase().kind === "ready" && openQuestions(sessionWorkspaces().get(sessionId)?.interactions ?? initialInteractions)
          .some((question) => requested.has(questionId(question.key)))) {
          attached.delete(sessionId);
          await openSession(sessionId);
        }
        throw new Error(t("interactions.closed"));
      }
      throw reason;
    } finally {
      replying.update((map) => {
        const all = new Map(map);
        const rest = new Set(all.get(sessionId));
        for (const id of ids) rest.delete(id);
        if (rest.size) all.set(sessionId, rest);
        else all.delete(sessionId);
        return all;
      });
    }
  };

  /** Sends text and images; a failure hands the whole draft back and adds a notice. */
  const prompt = async (sessionId: SessionId, text: string, images: readonly PromptImage[] = []) => {
    promptsInFlight.update((map) => new Map(map).set(sessionId, (map.get(sessionId) ?? 0) + 1));
    try {
      await connection.request(AcpProtocol.methods.sessionPrompt, {
        sessionId,
        prompt: promptBlocks({ text, images }),
        _meta: { [NAMESPACE]: { submissionId: crypto.randomUUID() } },
      });
    } catch (error: unknown) {
      // Nothing reached history, or its outcome is unknown: hand the text back so it is not lost.
      returnedDrafts.update((map) => new Map(map).set(sessionId, { text, images }));
      // This refusal speaks for the Run that failed before insertion: drop that Run's own notice.
      for (const notice of workspace().notices) {
        if (notice.fromRun && notice.sessionId === sessionId) dispatch({ type: "notice/dismissed", id: notice.id });
      }
      const method = error instanceof AcpRequestError ? error.authMethod : undefined;
      dispatch({
        type: "notice/added",
        notice: method !== undefined ? signInNotice(sessionId, method)
          : { id: `prompt-${crypto.randomUUID()}`, sessionId, title: t("notice.promptFailed"), description: messageOf(error) },
      });
      setRunning(sessionId, false);
    } finally {
      promptsInFlight.update((map) => {
        const next = new Map(map);
        const left = (next.get(sessionId) ?? 1) - 1;
        if (left > 0) next.set(sessionId, left);
        else next.delete(sessionId);
        return next;
      });
    }
  };

  /** Reads the Session's whole entry tree, which maps a transcript message to the entry Branch and Fork take. */
  const readTree = (sessionId: SessionId) => connection.request<unknown>(`_${NAMESPACE}/session/tree`, { sessionId }, { timeoutMs });

  return {
    info,
    phase,
    workspace: readableWorkspace,
    files: new WorkspaceFiles(connection, timeoutMs),
    panels,
    setPanel: (sessionId, panel) => panels.update((current) => {
      const next = new Map(current);
      if (panel) next.set(sessionId, panel);
      else next.delete(sessionId);
      return next;
    }),
    sessions,
    session: (sessionId) => sessionWorkspaces().get(sessionId),
    defaultConfig,
    authMethods,
    signInFlow,
    signIn: async (methodId) => {
      if (signInFlow() !== null) throw new Error("A sign-in is already in progress");
      const controller = new AbortController();
      signInAbort = controller;
      signInFlow.set({ methodId });
      try {
        // A sign-in waits for the user in a browser: no deadline, only cancellation.
        await connection.request(AcpProtocol.methods.authLogin, { methodId },
          // The response ends ownership in wire order, before this continuation runs.
          { signal: controller.signal, onRequestId: (id) => { loginRequestId = id; }, onResult: () => { loginRequestId = undefined; } });
      } finally {
        loginRequestId = undefined;
        declineCodes();
        signInAbort = undefined;
        signInFlow.set(null);
      }
    },
    cancelSignIn: () => {
      // Cancelling revokes the login's scope at once: a late elicitation from it is someone else's.
      loginRequestId = undefined;
      declineCodes();
      signInAbort?.abort();
    },
    signInStatuses: async () => AcpProtocol.signInStatuses(
      await connection.request<unknown>(`_${NAMESPACE}/auth/status`, {}, { timeoutMs })),
    renameSession: async (sessionId, title) => {
      const reply = AcpProtocol.record(await connection.request<unknown>(
        AcpProtocol.methods.sessionSetTitle, { sessionId, title: title.trim() }, { timeoutMs }));
      // The host trims and bounds titles itself; show what it stored.
      const stored = typeof reply?.title === "string" ? reply.title : title.trim();
      dispatch({ type: "session/title", sessionId, title: stored || sessionId });
    },
    deleteSession: async (sessionId) => {
      await connection.request<unknown>(AcpProtocol.methods.sessionDelete, { sessionId }, { timeoutMs });
      panels.update((current) => {
        if (!current.has(sessionId)) return current;
        const next = new Map(current);
        next.delete(sessionId);
        return next;
      });
      await refreshSessions();
    },
    commandsFor: (sessionId) => sessionCommands().get(sessionId) ?? EMPTY_COMMANDS,
    sessionConfig: (sessionId) => sessionConfigs().get(sessionId) ?? EMPTY_CONFIG,
    configPending: (sessionId) => configWrites().has(sessionId),
    setSessionConfig: async (sessionId, configId, value) => {
      if (phase().kind !== "ready") throw new Error("The host is offline");
      if (configWrites().has(sessionId)) throw new Error("A Session settings change is already pending");
      configWrites.update((pending) => new Set(pending).add(sessionId));
      try {
        // A reconnected host may display cached settings before it has attached this Session on the new wire.
        if (!attached.has(sessionId)) await openSession(sessionId);
        const select = sessionConfigs().get(sessionId)?.find((option) => option.id === configId);
        if (!select?.options.some((option) => option.value === value)) throw new Error("This setting or value is not offered by the host");
        if (select.currentValue === value) return;
        await connection.request(AcpProtocol.methods.sessionSetConfigOption, { sessionId, configId, type: "id", value }, {
          timeoutMs, onResult: (result) => applyConfigResult(sessionId, result, true),
        });
      } catch (error: unknown) {
        // Remote error text can contain peer-supplied data; its RPC code identifies the failing request safely.
        const detail = error instanceof AcpRequestError ? ` (ACP ${error.rpc.code})` : "";
        console.warn(`Could not change settings for Session ${sessionId} on host ${info.id.slice(0, 12)}${detail}`);
        // A model that needs a sign-in refuses the change: offer the sign-in, then the user picks it again.
        const method = error instanceof AcpRequestError ? error.authMethod : undefined;
        if (method !== undefined) dispatch({ type: "notice/added", notice: signInNotice(sessionId, method) });
        throw error;
      } finally {
        configWrites.update((pending) => {
          const next = new Set(pending);
          next.delete(sessionId);
          return next;
        });
      }
    },
    running: (sessionId) => runningIds().has(sessionId),
    queryRunning: async (sessionId) => {
      const result = AcpProtocol.record(await connection.request<unknown>(SESSION_RUNTIME, { sessionId }, { timeoutMs }));
      // Same validation as `readRuntime`: a reply about another Session is not an answer.
      if (typeof result?.running !== "boolean" || (result.sessionId !== undefined && result.sessionId !== sessionId)) {
        throw new Error("Invalid session runtime snapshot");
      }
      return result.running;
    },
    permissionFor: (sessionId) => permissions().find((prompt) => prompt.sessionId === sessionId),
    openQuestions: (sessionId) => openQuestions(sessionWorkspaces().get(sessionId)?.interactions ?? initialInteractions),
    awaitsAnswer: (sessionId) => awaitingAnswers().has(sessionId),
    questionPending: (sessionId, key) => replying().get(sessionId)?.has(questionId(key)) === true,
    answerQuestion: (sessionId, key, text) => resolveQuestions(sessionId, [key], text),
    dismissQuestions: (sessionId, keys) => resolveQuestions(sessionId, keys),
    notices,
    dismissNotice: (id) => dispatch({ type: "notice/dismissed", id }),
    start: (previous) => (started ??= connectOnce(previous)),
    stop: () => connection.close(),
    openSession,
    newSession: async (cwd) => {
      // Creation is non-idempotent. A live peer may finish after the usual read deadline.
      // Connection shutdown still rejects this request and releases the waiting composer.
      const result = await connection.request<NewSessionResult>(AcpProtocol.methods.sessionNew, { cwd }, {
        onResult: (created) => {
          if (typeof created?.sessionId !== "string") throw new Error("session/new did not return a Session id");
          applySessionResult(created.sessionId, created);
        },
      });
      try {
        await refreshSessions();
        await openSession(result.sessionId);
      } catch (error: unknown) {
        // The server already attached this connection and confirmed the id. Retrying creation after a
        // directory or replay failure would duplicate it. Keep a selectable row until the next listing.
        if (!sessions().some((session) => session.sessionId === result.sessionId)) {
          dispatch({ type: "session/listed", sessions: [...sessions(), { sessionId: result.sessionId, cwd, title: result.sessionId }] });
        }
        console.warn(`Could not refresh newly created Session ${result.sessionId} on ${info.id}`);
        throw new SessionPreparationError(result.sessionId, error);
      }
      return result.sessionId;
    },
    prompt,
    trajectory: (sessionId, query) => connection.request<unknown>(`_${NAMESPACE}/session/trajectory`, { sessionId, ...query }, { timeoutMs }),
    requestContent: async (sessionId, hash) => {
      const reply = AcpProtocol.record(await connection.request<unknown>(`_${NAMESPACE}/session/trajectory/content`, { sessionId, hash }, { timeoutMs }));
      return typeof reply?.content === "string" ? reply.content : undefined;
    },
    editPrompt: async (sessionId, messageId, draft) => {
      try {
        const entry = AcpProtocol.treeEntry(await readTree(sessionId), messageId, "user");
        if (entry === undefined) throw new Error(t("edit.notFound"));
        // Branch only moves the cursor; the edited message then goes out as an ordinary prompt on the new branch.
        await connection.request<unknown>(`_${NAMESPACE}/session/branch`, { sessionId, entryId: entry.parentId, summarize: false }, { timeoutMs });
        // Replay before sending: a replay that started after the prompt would clear its live updates.
        attached.delete(sessionId);
        await openSession(sessionId);
      } catch (error: unknown) {
        dispatch({ type: "notice/added", notice: { id: `edit-${crypto.randomUUID()}`, sessionId, title: t("notice.editFailed"), description: messageOf(error) } });
        throw error;
      }
      // The turn runs on after this resolves; like any prompt, a failed send comes back as a returned draft.
      void prompt(sessionId, draft.text, draft.images);
    },
    fork: async (sessionId, messageId) => {
      try {
        const entry = AcpProtocol.treeEntry(await readTree(sessionId), messageId, "assistant");
        if (entry === undefined) throw new Error(t("edit.notFound"));
        const cwd = sessions().find((session) => session.sessionId === sessionId)?.cwd;
        // Copying a long history may outlast the read deadline, as creating a Session may.
        const reply = AcpProtocol.record(await connection.request<unknown>(`_${NAMESPACE}/session/fork`, { sessionId, entryId: entry.entryId, cwd }));
        if (typeof reply?.sessionId !== "string") throw new Error("session/fork did not return a Session id");
        await refreshSessions();
        return reply.sessionId as SessionId;
      } catch (error: unknown) {
        dispatch({ type: "notice/added", notice: { id: `fork-${crypto.randomUUID()}`, sessionId, title: t("notice.forkFailed"), description: messageOf(error) } });
        throw error;
      }
    },
    returnedDrafts,
    takeReturnedDraft,
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
