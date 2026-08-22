import { useEffect, useRef } from "react";

const FOCUSABLE_SELECTOR = "a[href], button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex='-1'])";

/** Traps keyboard focus inside a modal and restores the launching control. */
export function useModalFocus(
  onEscape: () => void,
  escapeDisabled = false,
  initialFocus?: (modal: HTMLElement) => HTMLElement | null
) {
  const modalRef = useRef<HTMLElement>(null);
  const onEscapeRef = useRef(onEscape);
  const escapeDisabledRef = useRef(escapeDisabled);
  const initialFocusRef = useRef(initialFocus);
  const previousFocusRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    // Keep the stable listener and focus resolver synchronized without retrapping focus.
    onEscapeRef.current = onEscape;
    escapeDisabledRef.current = escapeDisabled;
    initialFocusRef.current = initialFocus;
  }, [escapeDisabled, initialFocus, onEscape]);

  useEffect(() => {
    const modal = modalRef.current;
    if (modal === null) return undefined;
    // React StrictMode repeats Effect setup; retain the original launcher
    // rather than replacing it with an element inside the modal on pass two.
    if (previousFocusRef.current === null && document.activeElement instanceof HTMLElement) {
      previousFocusRef.current = document.activeElement;
    }
    const focusable = [...modal.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)]
      .filter((element) => element.getClientRects().length > 0);
    const resolved = initialFocusRef.current?.(modal) ?? (focusable[0] ?? modal);
    resolved.focus();

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !escapeDisabledRef.current) {
        event.preventDefault();
        onEscapeRef.current();
        return;
      }
      if (event.key !== "Tab") return;
      const currentFocusable = [...modal.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)]
        .filter((element) => element.getClientRects().length > 0);
      if (currentFocusable.length === 0) {
        event.preventDefault();
        modal.focus();
        return;
      }
      const first = currentFocusable[0];
      const last = currentFocusable[currentFocusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last?.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first?.focus();
      }
    };

    modal.addEventListener("keydown", handleKeyDown);
    return () => {
      modal.removeEventListener("keydown", handleKeyDown);
      previousFocusRef.current?.focus();
    };
  }, []);

  return modalRef;
}
