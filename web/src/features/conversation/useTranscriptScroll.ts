import { useCallback, useEffect, useLayoutEffect, useRef } from "react";
import type { RefObject, UIEventHandler } from "react";

const BOTTOM_THRESHOLD_PX = 32;

export type TranscriptContentRevision = Readonly<{
  sessionId: string | undefined;
  messages: object;
  tools: object;
  bashExecutions: object;
  extensions: object;
  compactions: object;
  compactionStatus: object;
  transcriptLength: number;
}>;

export type TranscriptScrollBinding = Readonly<{
  transcriptRef: RefObject<HTMLDivElement | null>;
  onScroll: UIEventHandler<HTMLDivElement>;
}>;

/** Keeps the transcript pinned only while its viewport remains at the bottom. */
export function useTranscriptScroll(
  revision: TranscriptContentRevision
): TranscriptScrollBinding {
  const transcriptRef = useRef<HTMLDivElement>(null);
  const followsLatest = useRef(true);
  const followedSessionId = useRef<string | undefined>(undefined);

  useLayoutEffect(() => {
    if (followedSessionId.current !== revision.sessionId) {
      // A newly activated transcript starts at its latest persisted event.
      followedSessionId.current = revision.sessionId;
      followsLatest.current = true;
    }

    const container = transcriptRef.current;
    if (container !== null && followsLatest.current) {
      // Immediate positioning before paint avoids retargeting a smooth scroll
      // animation for every streaming delta.
      container.scrollTop = container.scrollHeight;
    }
  }, [
    revision.bashExecutions,
    revision.compactions,
    revision.compactionStatus,
    revision.extensions,
    revision.messages,
    revision.sessionId,
    revision.tools,
    revision.transcriptLength
  ]);

  const onScroll = useCallback<UIEventHandler<HTMLDivElement>>((event) => {
    const container = event.currentTarget;
    const distanceFromBottom = Math.max(
      0,
      container.scrollHeight - container.clientHeight - container.scrollTop
    );
    followsLatest.current = distanceFromBottom <= BOTTOM_THRESHOLD_PX;
  }, []);

  useEffect(() => {
    const container = transcriptRef.current;
    if (container === null) return undefined;

    /** Recomputes follow state after a details element changes the scroll range. */
    const handleToggle = (event: Event) => {
      if (!(event.target instanceof HTMLDetailsElement)) return;

      const distanceFromBottom = Math.max(
        0,
        container.scrollHeight - container.clientHeight - container.scrollTop
      );
      // Expanding or collapsing details changes the scroll range without a
      // transcript update, so recompute follow state from the resulting layout.
      followsLatest.current = distanceFromBottom <= BOTTOM_THRESHOLD_PX;
    };

    // Native capture observes the non-bubbling toggle event from nested details.
    container.addEventListener("toggle", handleToggle, true);
    return () => {
      container.removeEventListener("toggle", handleToggle, true);
    };
  }, [revision.sessionId]);

  return { transcriptRef, onScroll };
}
