import type { SessionSummary } from "../client/domain/model";

/** A workspace's display name: the last segment of its path. */
export function workspaceName(cwd: string): string {
  return cwd.split(/[\\/]/).filter((part) => part.length > 0).at(-1) ?? cwd;
}

/** Newest first by modification time; Sessions without one go last. */
function newestFirst(left: SessionSummary, right: SessionSummary): number {
  return Number(right.modifiedAtMs ?? 0) - Number(left.modifiedAtMs ?? 0);
}

/** One workspace and its Sessions, newest first. */
export type WorkspaceGroup = Readonly<{ cwd: string; name: string; sessions: readonly SessionSummary[] }>;

/** Sessions grouped by working directory; groups ordered by their newest Session. */
export function groupByWorkspace(sessions: readonly SessionSummary[]): readonly WorkspaceGroup[] {
  const groups = new Map<string, SessionSummary[]>();
  for (const session of [...sessions].sort(newestFirst)) {
    const list = groups.get(session.cwd) ?? [];
    list.push(session);
    groups.set(session.cwd, list);
  }
  return [...groups].map(([cwd, list]) => ({ cwd, name: workspaceName(cwd), sessions: list }));
}

/** The `count` most recently modified Sessions. */
export function recentSessions(sessions: readonly SessionSummary[], count: number): readonly SessionSummary[] {
  return [...sessions].sort(newestFirst).slice(0, count);
}
