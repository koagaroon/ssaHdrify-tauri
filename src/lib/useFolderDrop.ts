/**
 * useFolderDrop — subscribe to Tauri's webview drag-drop event and fire a
 * callback when the user drops files / folders onto a designated drop
 * zone (a ref-bound element).
 *
 * Tauri 2 routes drag-drop through the webview, NOT the HTML5 DOM events
 * — `event.dataTransfer.files` is empty in webview content. Instead,
 * `getCurrentWebview().onDragDropEvent` delivers the native paths plus a
 * physical-pixel position. We translate the position to logical pixels
 * (divide by devicePixelRatio) and intersect with the ref's bounding
 * rect so multiple drop zones can coexist on a page without claiming
 * each other's drops.
 *
 * Folder paths are expanded one level deep on the Rust side via
 * `expand_dropped_paths`. Consumers receive a flat list of file paths
 * regardless of whether the user dropped files, folders, or a mix.
 */
import { useEffect, useRef } from "react";
import type { RefObject } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { expandDroppedPaths } from "./tauri-api";

export interface UseFolderDropOptions {
  /** Drop zone element. Drops outside this element's bounding rect are ignored. */
  ref: RefObject<HTMLElement | null>;
  /** Called with expanded file paths when the user drops onto the zone.
   *  Categorization (video vs subtitle) is the consumer's job. */
  onPaths: (paths: string[]) => void;
  /** Optional visibility-state callback for hover styling. Receives `true`
   *  while a drag is over the zone, `false` on leave / drop / drop-outside. */
  onActiveChange?: (active: boolean) => void;
  /** Optional notifier for when `expandDroppedPaths` rejects (Rust IPC
   *  error). Without this, a drop that hits e.g. the `MAX_INPUT_PATHS`
   *  cap silently console.errors and the user sees a drag that "did
   *  nothing." Consumers pass a banner / log writer here. */
  onError?: (error: unknown) => void;
  /** When true, the hook skips subscribing — useful while the consumer is
   *  busy processing a previous drop and wants to ignore further drops. */
  disabled?: boolean;
  /** Translator for hook-emitted error wording (truncated drop,
   *  zero-usable-paths). Round 8 Wave 8.6 — previously the hook's
   *  error messages were hardcoded English (N-R5-FELIB-11). Optional
   *  so consumers in non-i18n contexts (tests, future internal
   *  callers) still work; falls back to English when omitted. */
  t?: (key: string, ...args: (string | number)[]) => string;
}

/** Subscribe to drag-drop events scoped to a ref'd drop zone element. */
export function useFolderDrop({
  ref,
  onPaths,
  onActiveChange,
  onError,
  disabled,
  t,
}: UseFolderDropOptions): void {
  // Stabilize the consumer callbacks AND the disabled flag so the
  // listener-attaching effect doesn't re-subscribe on every parent
  // render (consumers pass inline arrows in JSX) NOR on every batch
  // boundary (every batch flips disabled false → true → false, and
  // each subscribe costs an `await onDragDropEvent` round-trip plus a
  // brief race window where drops can be missed). Same pattern as
  // useClickOutside.
  const onPathsRef = useRef(onPaths);
  const onActiveChangeRef = useRef(onActiveChange);
  const onErrorRef = useRef(onError);
  const disabledRef = useRef(disabled);
  const tRef = useRef(t);
  // No deps array — refresh refs every render so the listener-attaching
  // effect below can keep its closures stable while still reading the
  // latest values. Not a missing-dep bug.
  useEffect(() => {
    onPathsRef.current = onPaths;
    onActiveChangeRef.current = onActiveChange;
    onErrorRef.current = onError;
    disabledRef.current = disabled;
    tRef.current = t;
  });

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let mounted = true;

    // Tauri's API is async; non-Tauri hosts (e.g., a vitest env that
    // accidentally imports a consumer of this hook) don't expose
    // `getCurrentWebview()`. Wrap the bootstrap in a guard so render
    // doesn't crash in those environments — webview-dependent features
    // simply become no-ops.
    const setup = async () => {
      let webview;
      try {
        webview = getCurrentWebview();
      } catch {
        return;
      }

      let handler: () => void;
      try {
        handler = await webview.onDragDropEvent(async (event) => {
          if (!mounted || !ref.current) return;
          // Read disabled via the ref — toggled at every batch boundary,
          // and we don't want each toggle to tear down + re-subscribe
          // the underlying webview listener (which has an async setup
          // cost and a race window).
          if (disabledRef.current) return;
          const rect = ref.current.getBoundingClientRect();
          // Same symmetric defensiveness this file applies to
          // getCurrentWebview() up-stack: cheap-and-safe guard for
          // non-browser execution contexts (Node-side tests, future
          // SSR snapshot) where `window` may be unbound
          // (N-R5-FELIB-15).
          const dpr = (typeof window !== "undefined" && window.devicePixelRatio) || 1;

          // Tauri reports the cursor position in physical pixels relative
          // to the webview's top-left. getBoundingClientRect is in CSS
          // (logical) pixels, so divide by DPR before comparing.
          const inRect = (pos: { x: number; y: number }): boolean => {
            const x = pos.x / dpr;
            const y = pos.y / dpr;
            return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
          };

          switch (event.payload.type) {
            case "enter":
            case "over":
              onActiveChangeRef.current?.(inRect(event.payload.position));
              break;
            case "leave":
              onActiveChangeRef.current?.(false);
              break;
            case "drop": {
              const inside = inRect(event.payload.position);
              onActiveChangeRef.current?.(false);
              if (!inside) return;
              try {
                const expanded = await expandDroppedPaths(event.payload.paths);
                // Re-check `disabled` AFTER the await (Codex f0cd1143).
                // If the consumer flipped disabled=true while the IPC
                // round-trip was in flight (e.g. another batch just
                // started and the parent tab raised its busy flag), a
                // stale `onPaths` commit here would jam paths into the
                // current operation. Without this guard, the start-of-
                // event check at line 96 only covers the trivial "drop
                // arrives when already busy" case — not "drop arrives
                // when idle, becomes busy mid-await."
                if (mounted && !disabledRef.current) {
                  if (expanded.files.length > 0) {
                    onPathsRef.current(expanded.files);
                    // Surface truncation alongside the partial result so
                    // the consumer banner can prompt the user to retry
                    // with a smaller drop (Round 3 N-R3-19). Routed
                    // through onError because consumers already render
                    // an error banner from that callback; a dedicated
                    // `onTruncated` would force every consumer to wire
                    // a new path.
                    if (expanded.truncated) {
                      // Round 8 Wave 8.6 — closes N-R5-FELIB-11 by
                      // threading the optional `t` translator through
                      // the options surface. Falls back to English
                      // when consumers don't pass `t` (tests, future
                      // internal callers). The "5000" literal here
                      // matches `MAX_RESULT_FILES` on the Rust side
                      // (`dropzone.rs`); kept inline so the wording
                      // doesn't depend on a round-trip lookup.
                      const tr = tRef.current;
                      const msg = tr
                        ? tr("msg_drop_truncated", 5000)
                        : "Drop too large — first 5000 files accepted, the rest were ignored. Retry with a smaller batch.";
                      onErrorRef.current?.(new Error(msg));
                    }
                  } else if (event.payload.paths.length > 0) {
                    // Non-empty input that expanded to zero paths — the
                    // user dropped *something*, the Rust side accepted
                    // the call, and yet we'd silently do nothing without
                    // this signal (Round 1 F1.N-R1-17). Surface as an
                    // error so the consumer banner reads "no usable
                    // files in this drop" instead of nothing.
                    const tr = tRef.current;
                    const msg = tr
                      ? tr("msg_drop_no_usable")
                      : "Drop expanded to zero usable paths";
                    onErrorRef.current?.(new Error(msg));
                  }
                }
              } catch (e) {
                console.error("expandDroppedPaths failed:", e);
                // Notify the consumer so it can surface the failure
                // (banner / log line). Without this, a drop that
                // tripped MAX_INPUT_PATHS or any other Rust-side
                // rejection produces a silent no-op from the user's
                // perspective.
                if (mounted) onErrorRef.current?.(e);
              }
              break;
            }
          }
        });
      } catch (e) {
        if (mounted) {
          console.error("onDragDropEvent subscription failed:", e);
          onActiveChangeRef.current?.(false);
        }
        return;
      }

      // Race window: the component may have unmounted while we awaited
      // `onDragDropEvent`. Tear down the listener immediately if so.
      if (!mounted) {
        handler();
        return;
      }
      unlisten = handler;
    };

    void setup();

    return () => {
      mounted = false;
      unlisten?.();
    };
    // `ref` is a stable RefObject across renders (React guarantees);
    // `disabled` is read via disabledRef inside the listener so it
    // doesn't belong here either. Empty deps would also work but keep
    // ref listed so a future pivot to a new ref triggers a clean
    // re-subscribe.
  }, [ref]);
}
