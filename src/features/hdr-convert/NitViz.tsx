import { useCallback, useEffect, useRef } from "react";
import { useI18n } from "../../i18n/useI18n";
import { MIN_BRIGHTNESS, MAX_BRIGHTNESS } from "./color-engine";

interface NitVizProps {
  value: number;
  onChange: (nits: number) => void;
  disabled?: boolean;
}

interface Preset {
  key: "sdr" | "bt2408" | "hdr10" | "dv";
  label: string;
  v: number;
  descKey: string;
}

const PRESETS: Preset[] = [
  { key: "sdr", label: "SDR", v: 100, descKey: "preset_sdr_desc" },
  { key: "bt2408", label: "BT.2408", v: 203, descKey: "preset_bt2408_desc" },
  { key: "hdr10", label: "HDR10", v: 1000, descKey: "preset_hdr10_desc" },
  { key: "dv", label: "DV", v: 4000, descKey: "preset_dv_desc" },
];

/**
 * Maps [MIN_BRIGHTNESS, MAX_BRIGHTNESS] onto 0-100% using a log10 curve —
 * low values (100, 203) get more screen real estate than high ones (4000,
 * 10000), matching how human brightness perception actually scales.
 *
 * The formula generalizes for any MIN_BRIGHTNESS (not just 1) so bumping
 * the constant won't silently desynchronize the marker position from the
 * value returned on click. log10(MIN) == 0 only when MIN == 1, so the
 * previous form was a special case that would drift if the constant moved.
 */
const LOG_MIN = Math.log10(MIN_BRIGHTNESS);
const LOG_MAX = Math.log10(MAX_BRIGHTNESS);
const LOG_RANGE = LOG_MAX - LOG_MIN;
function nitsToPct(nits: number): number {
  const clamped = Math.min(MAX_BRIGHTNESS, Math.max(MIN_BRIGHTNESS, nits));
  return ((Math.log10(clamped) - LOG_MIN) / LOG_RANGE) * 100;
}

function pctToNits(pct: number): number {
  const p = Math.min(1, Math.max(0, pct));
  return Math.round(Math.pow(10, LOG_MIN + p * LOG_RANGE));
}

export default function NitViz({ value, onChange, disabled = false }: NitVizProps) {
  const { t } = useI18n();
  const trackRef = useRef<HTMLDivElement>(null);
  const dragCleanupRef = useRef<(() => void) | null>(null);
  const markerPct = nitsToPct(value);

  const setFromClientX = useCallback(
    (clientX: number) => {
      const el = trackRef.current;
      if (!el) return;
      const rect = el.getBoundingClientRect();
      if (rect.width <= 0) return; // pre-layout or collapsed — ignore
      const pct = (clientX - rect.left) / rect.width;
      onChange(pctToNits(pct));
    },
    [onChange]
  );

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (disabled) return;
    // Only respond to primary (left) button; right-click and middle-click
    // should not drag the marker.
    if (e.button !== 0) return;
    e.preventDefault();
    const el = trackRef.current;
    if (!el) return;
    dragCleanupRef.current?.();
    const pointerId = e.pointerId;
    // setPointerCapture is still useful when WebView2 supports it, but the
    // load-bearing listeners live on window below. That fallback keeps drag
    // working if capture fails and lets us filter by the initiating pointer.
    let captured = false;
    try {
      el.setPointerCapture(pointerId);
      captured = true;
    } catch {
      // not supported (very old webview) — window listeners below still fire
    }
    setFromClientX(e.clientX);
    const onMove = (ev: PointerEvent) => {
      if (ev.pointerId !== pointerId) return;
      setFromClientX(ev.clientX);
    };
    let cleanup = () => {};
    const onUp = (ev: PointerEvent) => {
      if (ev.pointerId !== pointerId) return;
      cleanup();
    };
    cleanup = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onUp);
      try {
        if (captured) el.releasePointerCapture(pointerId);
      } catch {
        // ignore — capture may have already been released
      }
      if (dragCleanupRef.current === cleanup) dragCleanupRef.current = null;
    };
    dragCleanupRef.current = cleanup;
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onUp);
  };

  useEffect(() => {
    return () => dragCleanupRef.current?.();
  }, []);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (disabled) return;
    const step = e.shiftKey ? 100 : 10;
    if (e.key === "ArrowLeft" || e.key === "ArrowDown") {
      e.preventDefault();
      onChange(Math.max(MIN_BRIGHTNESS, value - step));
    } else if (e.key === "ArrowRight" || e.key === "ArrowUp") {
      e.preventDefault();
      onChange(Math.min(MAX_BRIGHTNESS, value + step));
    } else if (e.key === "Home") {
      e.preventDefault();
      onChange(MIN_BRIGHTNESS);
    } else if (e.key === "End") {
      e.preventDefault();
      onChange(MAX_BRIGHTNESS);
    }
  };

  return (
    <div className={`nit-viz${disabled ? " is-disabled" : ""}`}>
      <div className="nit-head">
        <span className="nit-head-label">{t("nit_target")}</span>
        <span className="nit-readout">
          <span className="v">{value}</span>
          <span className="u">{t("nit_unit")}</span>
        </span>
      </div>

      <div
        ref={trackRef}
        className="nit-track"
        onPointerDown={handlePointerDown}
        onKeyDown={handleKeyDown}
        role="slider"
        tabIndex={disabled ? -1 : 0}
        aria-valuemin={MIN_BRIGHTNESS}
        aria-valuemax={MAX_BRIGHTNESS}
        aria-valuenow={value}
        aria-valuetext={`${value} ${t("nit_unit")}`}
        aria-disabled={disabled}
        aria-label={t("nit_target")}
      >
        <div className="nit-ticks">
          {Array.from({ length: 11 }).map((_, i) => (
            <span key={i} />
          ))}
        </div>
        {/* markerPct is a numeric percentage clamped from internal slider state — safe by inspection. */}
        {/* eslint-disable-next-line no-restricted-syntax */}
        <div className="nit-fill" style={{ width: `${markerPct}%` }} />
        {/* eslint-disable-next-line no-restricted-syntax */}
        <div className="nit-marker" style={{ left: `calc(${markerPct}% - 8px)` }}>
          <div className="nit-marker-dot" />
        </div>
      </div>

      <div className="nit-scale">
        <span>1</span>
        <span>10</span>
        <span>100</span>
        <span>1k</span>
        <span>10k</span>
      </div>

      <div className="nit-presets-label">
        <span>{t("nit_presets_label")}</span>
        <span className="nit-presets-hint">{t("nit_presets_hint")}</span>
      </div>
      <div className="nit-presets">
        {PRESETS.map((p) => (
          <button
            key={p.key}
            type="button"
            className="nit-preset"
            data-key={p.key}
            aria-pressed={value === p.v}
            onClick={() => onChange(p.v)}
            disabled={disabled}
            title={`${t(p.descKey)} · ${p.v} ${t("nit_unit")}`}
          >
            <span className="nit-preset-label">{p.label}</span>
            <span className="nit-preset-v">{p.v}</span>
            <span className="nit-preset-desc">{t(p.descKey)}</span>
          </button>
        ))}
      </div>

      <div className="nit-interaction-hint">{t("nit_interaction_hint")}</div>
    </div>
  );
}
