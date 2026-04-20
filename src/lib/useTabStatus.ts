/**
 * useTabStatus — publish a feature tab's current workflow status to the
 * shared StatusContext so the footer indicator can reflect it.
 *
 * Each feature tab derives its Status from local state (file loaded?
 * busy? last action?) and passes it here. The effect keys on the status's
 * kind/message primitives rather than the object's identity, so callers
 * can pass a freshly-allocated `{kind, message}` on every render without
 * triggering a re-publish — the effect only fires when the values
 * actually differ. (StatusProvider's setter also no-ops on unchanged
 * values, so even the dup-publish path stays cheap.)
 *
 * Extracted because HdrConvert / TimingShift / FontEmbed each carried a
 * near-identical useEffect with the same context wiring; only the
 * branching logic differs per tab.
 */
import { useEffect } from "react";
import { useStatus, type Status, type StatusTab } from "./StatusContext";

export function useTabStatus(tab: StatusTab, status: Status): void {
  const { setStatus } = useStatus();
  const { kind, message } = status;
  useEffect(() => {
    setStatus(tab, { kind, message });
  }, [tab, kind, message, setStatus]);
}
