import { useCallback, useEffect, useLayoutEffect, useRef } from "react";
import type { RefObject, UIEventHandler } from "react";

const BOTTOM_THRESHOLD_PX = 32;
/** Bounds a reveal on engines that never dispatch `scrollend`. */
const REVEAL_SETTLE_FALLBACK_MS = 1_000;
const REVEAL_FLASH_MS = 1_200;

export type TranscriptScrollBinding = Readonly<{
  transcriptRef: RefObject<HTMLDivElement | null>;
  onScroll: UIEventHandler<HTMLDivElement>;
  /** Scrolls one row into view and flashes it, holding the viewport against new deltas. */
  revealRow: (rowKey: string) => void;
}>;

/** Reports whether the viewport sits close enough to the end to follow new output. */
function isAtEnd(container: HTMLElement): boolean {
  return container.scrollHeight - container.clientHeight - container.scrollTop <= BOTTOM_THRESHOLD_PX;
}

/** Keeps the transcript pinned only while its viewport remains at the bottom. */
export function useTranscriptScroll(sessionId: string | undefined): TranscriptScrollBinding {
  const transcriptRef = useRef<HTMLDivElement>(null);
  const followsLatest = useRef(true);
  // Start without a followed Session so a transcript that mounts with an
  // already-active Session still performs its initial bottom alignment.
  const followedSessionId = useRef<string | undefined>(undefined);
  // Set only while a reveal's scroll runs. Its first frames still measure as
  // "at the end", so scroll events must not judge follow state until it lands.
  const cancelReveal = useRef<(() => void) | undefined>(undefined);
  const clearFlash = useRef<(() => void) | undefined>(undefined);

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
    const observer = new MutationObserver(() => {
      // Content that fits has no detached position to keep. Navigate and Branch
      // empty the transcript without moving a viewport already at the top, so no
      // scroll event would otherwise let the replay follow to its latest row.
      if (container.scrollHeight <= container.clientHeight) followsLatest.current = true;
      scrollToBottom();
    });
    observer.observe(container, { childList: true, subtree: true, characterData: true });
    return () => observer.disconnect();
  }, [scrollToBottom, sessionId]);

  const onScroll = useCallback<UIEventHandler<HTMLDivElement>>((event) => {
    if (cancelReveal.current !== undefined) return;
    followsLatest.current = isAtEnd(event.currentTarget);
  }, []);

  const revealRow = useCallback((rowKey: string) => {
    const container = transcriptRef.current;
    const row = container === null
      ? undefined
      : [...container.querySelectorAll<HTMLElement>("[data-transcript-key]")]
        .find((element) => element.dataset["transcriptKey"] === rowKey);
    if (container === null || row === undefined) return;
    cancelReveal.current?.();

    /** Marks the destination, restarting the animation if this row is still flashing. */
    const flash = () => {
      clearFlash.current?.();
      void row.offsetWidth;
      row.dataset["located"] = "true";
      const timer = window.setTimeout(() => clearFlash.current?.(), REVEAL_FLASH_MS);
      clearFlash.current = () => {
        window.clearTimeout(timer);
        delete row.dataset["located"];
        clearFlash.current = undefined;
      };
    };

    const containerBox = container.getBoundingClientRect();
    const rowBox = row.getBoundingClientRect();
    // Center the row, but keep the start of one taller than the viewport visible.
    const centered = container.scrollTop + rowBox.top - containerBox.top - Math.max(0, (container.clientHeight - rowBox.height) / 2);
    const target = Math.min(container.scrollHeight - container.clientHeight, Math.max(0, centered));
    // Already in place: no scroll event will fire, and a viewport at the end keeps following.
    if (Math.abs(target - container.scrollTop) < 1) {
      flash();
      return;
    }

    /** Hands follow state back to the reader's final position once the scroll lands. */
    const settle = () => {
      cancelReveal.current?.();
      followsLatest.current = isAtEnd(container);
      flash();
    };
    const fallback = window.setTimeout(settle, REVEAL_SETTLE_FALLBACK_MS);
    cancelReveal.current = () => {
      window.clearTimeout(fallback);
      container.removeEventListener("scrollend", settle);
      cancelReveal.current = undefined;
    };
    followsLatest.current = false;
    container.addEventListener("scrollend", settle);
    container.scrollTo({
      top: target,
      behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth"
    });
  }, []);

  useEffect(() => {
    const container = transcriptRef.current;
    if (container === null) return undefined;

    /** Recomputes follow state after a details element changes the scroll range. */
    const handleToggle = (event: Event) => {
      if (!(event.target instanceof HTMLDetailsElement) || cancelReveal.current !== undefined) return;

      // A collapse can move a previously detached viewport back to the bottom;
      // preserve that transition even though the MutationObserver is gated.
      followsLatest.current = isAtEnd(container);
    };

    // Native capture observes the non-bubbling toggle event from nested details.
    container.addEventListener("toggle", handleToggle, true);
    return () => {
      container.removeEventListener("toggle", handleToggle, true);
      // A reveal belongs to the transcript it started in.
      cancelReveal.current?.();
      clearFlash.current?.();
    };
  }, [sessionId]);

  return { transcriptRef, onScroll, revealRow };
}
