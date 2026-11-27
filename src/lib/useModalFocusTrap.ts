import { useEffect, useRef, type RefObject } from "react";

const FOCUSABLE_SELECTOR = [
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "a[href]",
  "summary",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

export function isRenderedFocusable(element: HTMLElement): boolean {
  const closedDetails = element.closest("details:not([open])");
  // Closed disclosures can report child geometry even though only their summary is focusable.
  if (closedDetails && closedDetails.querySelector(":scope > summary") !== element) {
    return false;
  }
  return element.getClientRects().length > 0;
}

export interface UseModalFocusTrapOptions {
  open: boolean;
  dialogRef: RefObject<HTMLElement | null>;
  initialFocusRef: RefObject<HTMLElement | null>;
  onEscape: () => void;
}

/**
 * Shared modal keyboard contract:
 * - Focuses `initialFocusRef` (or `dialogRef`) when `open` transitions to true
 * - Traps Tab / Shift+Tab inside `dialogRef`'s rendered focusable controls
 * - Intercepts Escape in the capture phase and routes to `onEscape`
 * - Restores focus to the previously active element when closed or unmounted
 */
export function useModalFocusTrap({
  open,
  dialogRef,
  initialFocusRef,
  onEscape,
}: UseModalFocusTrapOptions): void {
  const onEscapeRef = useRef(onEscape);
  useEffect(() => {
    onEscapeRef.current = onEscape;
  });

  useEffect(() => {
    if (!open) return;
    if (typeof window === "undefined" || typeof document === "undefined") return;

    const previouslyFocused = document.activeElement;
    if (initialFocusRef.current) {
      initialFocusRef.current.focus();
    } else {
      dialogRef.current?.focus();
    }

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        onEscapeRef.current();
        return;
      }

      if (event.key !== "Tab") return;
      const focusable = Array.from(
        dialogRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR) ?? []
      ).filter(isRenderedFocusable);
      if (focusable.length === 0) {
        event.preventDefault();
        dialogRef.current?.focus();
        return;
      }

      const first = focusable[0]!;
      const last = focusable[focusable.length - 1]!;
      const activeElement = document.activeElement;
      if (!dialogRef.current?.contains(activeElement)) {
        event.preventDefault();
        (event.shiftKey ? last : first).focus();
      } else if (event.shiftKey && activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };

    window.addEventListener("keydown", handleKeyDown, true);
    return () => {
      window.removeEventListener("keydown", handleKeyDown, true);
      if (
        previouslyFocused &&
        typeof (previouslyFocused as HTMLElement).focus === "function" &&
        (!(typeof HTMLElement === "function") || previouslyFocused instanceof HTMLElement)
      ) {
        (previouslyFocused as HTMLElement).focus();
      }
    };
  }, [open, dialogRef, initialFocusRef]);
}
