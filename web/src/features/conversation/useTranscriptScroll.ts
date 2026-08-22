import { useCallback, useEffect, useLayoutEffect, useRef } from "react";
import type { RefObject, UIEventHandler } from "react";

const BOTTOM_THRESHOLD_PX = 32;

export type TranscriptScrollBinding = Readonly<{
  transcriptRef: RefObject<HTMLDivElement | null>;
  onScroll: UIEventHandler<HTMLDivElement>;
}>;

/** Keeps the transcript pinned only while its viewport remains at the bottom. */
export function useTranscriptScroll(sessionId: string | undefined): TranscriptScrollBinding {
  const transcriptRef = useRef<HTMLDivElement>(null);
  const followsLatest = useRef(true);
  // Start without a followed Session so a transcript that mounts with an
  // already-active Session still performs its initial bottom alignment.
  const followedSessionId = useRef<string | undefined>(undefined);

  const scrollToBottom = useCallback(() => {
    const container = transcriptRef.current;
    if (container !== null && followsLatest.current) {
      // Immediate positioning before paint avoids retargeting a smooth scroll
      // animation for every streaming delta.
      container.scrollTop = container.scrollHeight;
    }
  }, []);

  useLayoutEffect(() => {
    const container = transcriptRef.current;
    if (container === null) return undefined;

    if (followedSessionId.current !== sessionId) {
      // A newly activated transcript starts at its latest persisted event.
      followedSessionId.current = sessionId;
      followsLatest.current = true;
      container.scrollTop = container.scrollHeight;
    }

    // Observing the DOM keeps follow state up to date without requiring this
    // component to subscribe to the streaming projection on every delta.
    const observer = new MutationObserver(() => scrollToBottom());
    observer.observe(container, { childList: true, subtree: true, characterData: true });
    return () => observer.disconnect();
  }, [scrollToBottom, sessionId]);

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
      // A collapse can move a previously detached viewport back to the bottom;
      // preserve that transition even though the MutationObserver is gated.
      followsLatest.current = distanceFromBottom <= BOTTOM_THRESHOLD_PX;
    };

    // Native capture observes the non-bubbling toggle event from nested details.
    container.addEventListener("toggle", handleToggle, true);
    return () => container.removeEventListener("toggle", handleToggle, true);
  }, [sessionId]);

  return { transcriptRef, onScroll };
}
