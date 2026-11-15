import type { SessionWorkspaceState } from "./sessionState";
import type { WorkspaceState } from "./state";
import type { SessionId } from "../acp/protocol";
import { Questions } from "../domain/model";
import type { ExtensionEntity, OpenQuestion, QuestionSettlement } from "../domain/model";

/** Resolves the active Session projection, or undefined when none is active. */
export function sessionWorkspaceOf(state: WorkspaceState, sessionId: SessionId | undefined = state.activeSessionId): SessionWorkspaceState | undefined {
  return sessionId === undefined ? undefined : state.sessionWorkspaces.get(sessionId);
}

/**
 * Counts exec_command cards whose terminal is open and has not exited yet; a
 * card cancelled by Esc still counts while its process runs. A failed card
 * never started a process (blocked, invalid, or refused), so its terminal
 * stays open forever and must not count.
 *
 * shortcut: a call cancelled in the moment before its process spawned looks like
 * a cancelled running one and keeps counting, replay included; return exec
 * details from a cancelled wait so cancelled cards can be excluded if that shows up.
 */
export function backgroundTerminalCount(session: SessionWorkspaceState): number {
  let count = 0;
  for (const tool of session.tools.values()) {
    if (tool.title !== "exec_command" || tool.status === "failed") continue;
    const terminal = session.terminals.get(tool.toolCallId);
    if (terminal !== undefined && terminal.exitStatus === undefined) count += 1;
  }
  return count;
}

// Settlements are derived once per extensions snapshot; the map is replaced on
// every change, so identity is a sound cache key.
const settlementCache = new WeakMap<ReadonlyMap<string, ExtensionEntity>, ReadonlyMap<string, QuestionSettlement>>();

/** Indexes every answer or skip a Session's reply records carry, by `Questions.id`. */
export function questionSettlements(extensions: ReadonlyMap<string, ExtensionEntity>): ReadonlyMap<string, QuestionSettlement> {
  const cached = settlementCache.get(extensions);
  if (cached !== undefined) return cached;
  const settlements = new Map<string, QuestionSettlement>();
  for (const extension of extensions.values()) {
    if (extension.customType !== Questions.REPLY_TYPE) continue;
    for (const settlement of Questions.settlements(extension.details)) settlements.set(Questions.id(settlement.key), settlement);
  }
  settlementCache.set(extensions, settlements);
  return settlements;
}

/** The Session fields that decide which questions are open. */
export type QuestionSources = Pick<SessionWorkspaceState, "transcript" | "tools" | "extensions" | "pending" | "settlingQuestions">;

/**
 * Lists questions still waiting for the user, in transcript order: asked by a
 * question card and not settled by a reply record, a queued answer, or this
 * tab's in-flight answer or skip.
 */
export function openQuestions(session: QuestionSources): OpenQuestion[] {
  const settled = questionSettlements(session.extensions);
  const queued = new Set([...session.pending.steering, ...session.pending.followUp]
    .flatMap((item) => Questions.queued(item.message).map((settlement) => Questions.id(settlement.key))));
  return session.transcript.flatMap((entry): OpenQuestion[] => {
    if (entry.type !== "tool") return [];
    const asked = session.tools.get(entry.toolCallId)?.userQuestions;
    if (asked === undefined) return [];
    return asked.questions.flatMap((question, index) => {
      const key = { requestId: asked.requestId, index };
      const id = Questions.id(key);
      return settled.has(id) || queued.has(id) || session.settlingQuestions.has(id) ? [] : [{ key, toolCallId: entry.toolCallId, question }];
    });
  });
}
