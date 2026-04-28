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
 *
 * `onDismiss` is captured through a ref so consumers may pass a fresh
 * arrow each render without forcing the listener-swap effect to re-run.
 * The orthodox stable-callback technique: a tiny update-ref effect keeps
 * the ref pointed at the latest callback, while the listener-attaching
 * effect's deps stay narrow ([open, ref]). This matches the lifecycle
 * the four pre-extraction call sites had — they didn't list their
 * setState arrows as deps because they were stable, and the
 * consolidation should preserve that, not regress it.
 */
import { useEffect, useRef } from "react";
import type { RefObject } from "react";

export function useClickOutside(
  open: boolean,
  ref: RefObject<HTMLElement | null>,
  onDismiss: () => void
): void {
  const onDismissRef = useRef(onDismiss);
  // Update the ref synchronously after every render so the listener
  // reads the freshest callback when it eventually fires.
  useEffect(() => {
    onDismissRef.current = onDismiss;
  });

  useEffect(() => {
    if (!open) return;
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        onDismissRef.current();
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onDismissRef.current();
    };
    const id = setTimeout(() => document.addEventListener("mousedown", onClick), 0);
    document.addEventListener("keydown", onKey);
    return () => {
      clearTimeout(id);
      document.removeEventListener("mousedown", onClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, ref]);
}
