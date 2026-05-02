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
  /** When true, the hook skips subscribing — useful while the consumer is
   *  busy processing a previous drop and wants to ignore further drops. */
  disabled?: boolean;
}

/** Subscribe to drag-drop events scoped to a ref'd drop zone element. */
export function useFolderDrop({
  ref,
  onPaths,
  onActiveChange,
  disabled,
}: UseFolderDropOptions): void {
  // Stabilize the consumer callbacks so the listener-attaching effect
  // doesn't re-subscribe on every parent render (consumers pass inline
  // arrows in JSX). Each re-subscribe costs an `await onDragDropEvent`
  // round-trip and opens a brief race window where drops can be missed.
  // Same pattern as useClickOutside.
  const onPathsRef = useRef(onPaths);
  const onActiveChangeRef = useRef(onActiveChange);
  // No deps array — refresh refs every render so the listener-attaching
  // effect below can keep its closures stable while still reading the
  // latest callbacks. Not a missing-dep bug.
  useEffect(() => {
    onPathsRef.current = onPaths;
    onActiveChangeRef.current = onActiveChange;
  });

  useEffect(() => {
    if (disabled) return;

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
          const rect = ref.current.getBoundingClientRect();
          const dpr = window.devicePixelRatio || 1;

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
                if (mounted && expanded.length > 0) {
                  onPathsRef.current(expanded);
                }
              } catch (e) {
                // Swallow — the consumer's Status flow is the right place
                // to surface user-facing failure; a drop with zero usable
                // paths is more annoying than informative.
                console.error("expandDroppedPaths failed:", e);
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
  }, [ref, disabled]);
}
