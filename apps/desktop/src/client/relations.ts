import type { SessionId } from "./acp/protocol";
import type { SessionSummary } from "./domain/model";

/** A Session's fork source and its subagents, split by whether they are working now. */
export type SessionRelations = Readonly<{
  /** The agent that spawned this Session, when it is a subagent. */
  owner?: SessionSummary;
  parent?: SessionSummary;
  running: readonly SessionSummary[];
  previous: readonly SessionSummary[];
}>;

/**
 * The relations of `sessionId` among the listed Sessions. Subagents come from the ownership edge, not from fork
 * history, newest first; `active` says whether one is running or waiting on the user.
 */
export function sessionRelations(
  sessions: readonly SessionSummary[], sessionId: SessionId, active: (id: SessionId) => boolean,
): SessionRelations {
  const current = sessions.find((session) => session.sessionId === sessionId);
  const parent = current?.parentSessionId === undefined
    ? undefined : sessions.find((session) => session.sessionId === current.parentSessionId);
  const owner = current?.owner === undefined
    ? undefined : sessions.find((session) => session.sessionId === current.owner!.sessionId);
  const children = sessions
    .filter((session) => session.owner?.sessionId === sessionId)
    // Millisecond timestamps are decimal strings; compare them as numbers. A missing time sorts last.
    .sort((left, right) => Number(right.modifiedAtMs ?? 0) - Number(left.modifiedAtMs ?? 0));
  const running: SessionSummary[] = [];
  const previous: SessionSummary[] = [];
  for (const session of children) (active(session.sessionId) ? running : previous).push(session);
  return { ...(owner ? { owner } : {}), ...(parent ? { parent } : {}), running, previous };
}

/** What `subagentActivity` reads from a host store. */
type ActivitySource = Readonly<{
  running: (sessionId: SessionId) => boolean;
  permissionFor: (sessionId: SessionId) => unknown;
  awaitsAnswer: (sessionId: SessionId) => boolean;
}>;

/**
 * Whether a subagent is working: running on this connection, reported running by the host's runtime query
 * (`queried`, for subagents never opened here), or waiting on an approval or an answer.
 */
export function subagentActivity(host: ActivitySource, queried: ReadonlySet<SessionId>): (sessionId: SessionId) => boolean {
  return (sessionId) => queried.has(sessionId) || host.running(sessionId) || host.permissionFor(sessionId) !== undefined
    || host.awaitsAnswer(sessionId);
}
