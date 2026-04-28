/**
 * useClickOutside — dismiss a transient overlay (dropdown, popover) when
 * the user clicks outside its container or presses Escape.
 *
 * Mousedown is armed on the next tick so the same click that opened the
 * overlay doesn't immediately close it. Without that delay, the click
 * that flipped `open` to true bubbles up to the freshly-attached
 * `mousedown` listener and closes it on the same frame.
 *
 * Used by App's theme menu and the file-strip dropdowns in HDR Convert,
 * Time Shift, and Font Embed — the original sites carried four
 * near-identical useEffects with the same wiring.
 */
import { useEffect } from "react";
import type { RefObject } from "react";

export function useClickOutside(
  open: boolean,
  ref: RefObject<HTMLElement | null>,
  onDismiss: () => void
): void {
  useEffect(() => {
    if (!open) return;
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        onDismiss();
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onDismiss();
    };
    const id = setTimeout(() => document.addEventListener("mousedown", onClick), 0);
    document.addEventListener("keydown", onKey);
    return () => {
      clearTimeout(id);
      document.removeEventListener("mousedown", onClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, ref, onDismiss]);
}
