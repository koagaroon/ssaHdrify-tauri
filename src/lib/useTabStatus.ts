/**
 * useTabStatus — publish a feature tab's current workflow status to the
 * shared StatusContext so the footer indicator can reflect it.
 *
 * Each feature tab derives its Status from local state (file loaded?
 * busy? last action?) and passes it here. StatusProvider's setter already
 * no-ops on unchanged values, so callers are free to recompute the Status
 * object on every render — the effect only fires when the computed kind
 * or message actually differs from what the context holds.
 *
 * Extracted because HdrConvert / TimingShift / FontEmbed each carried a
 * near-identical useEffect with the same context wiring; only the
 * branching logic differs per tab. Callers typically wrap the branching
 * in useMemo so the identity is stable across unrelated re-renders.
 */
import { useEffect } from "react";
import { useStatus, type Status, type StatusTab } from "./StatusContext";

export function useTabStatus(tab: StatusTab, status: Status): void {
  const { setStatus } = useStatus();
  useEffect(() => {
    setStatus(tab, status);
  }, [tab, status, setStatus]);
}
