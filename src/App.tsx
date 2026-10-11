import { useCallback, useEffect, useState, useRef, useMemo } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import HdrConvert from "./features/hdr-convert/HdrConvert";
import TimingShift from "./features/timing-shift/TimingShift";
import FontEmbed from "./features/font-embed/FontEmbed";
import BatchRename from "./features/batch-rename/BatchRename";
import StyleEdit from "./features/style-edit/StyleEdit";
import FontCacheDriftModal from "./features/font-embed/FontCacheDriftModal";
import AboutLicensesModal from "./AboutLicensesModal";
import { useI18n } from "./i18n/useI18n";
import AppearanceMenu from "./theme/AppearanceMenu";
import { useStatus, type StatusTab } from "./lib/StatusContext";
import { resolveAppModalVisibility } from "./lib/modal-coordination";
import { TAB_LABEL_KEYS } from "./lib/tab-labels";
import { completeFontCacheProbe } from "./lib/font-cache-ui-state";
import {
  openFontCache,
  detectFontCacheDrift,
  type FontCacheStatus,
  type FontCacheDriftReport,
} from "./lib/tauri-api";
import "./shell.css";

// Tab ids also serve as StatusTab keys — single source of truth.
type Tab = StatusTab;

// TAB_LABEL_KEYS is imported from lib/tab-labels.ts
// as the project-wide single source. Visual tab-strip order is the
// declaration order in that map (JS guarantees insertion-order
// iteration for string keys).
const TAB_IDS: { id: Tab; labelKey: string }[] = (Object.keys(TAB_LABEL_KEYS) as Tab[]).map(
  (id) => ({ id, labelKey: TAB_LABEL_KEYS[id] })
);

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("hdr");
  const { t, lang, setLang } = useI18n();
  const { statuses } = useStatus();
  const currentStatus = statuses[activeTab];
  // Resolve the Tauri window handle once — doing this at module scope would
  // crash a non-Tauri host (e.g. a vitest env that accidentally imports
  // App.tsx), even though only main.tsx imports it today. Wrapped in
  // try/catch so render-time failure inside a non-Tauri host produces a
  // null handle rather than crashing the whole tree; the three titlebar
  // buttons are null-guarded below.
  const appWindow = useMemo(() => {
    try {
      return getCurrentWindow();
    } catch {
      return null;
    }
  }, []);

  const [licensesOpen, setLicensesOpen] = useState(false);

  // Persistent font cache (#5): launch-time drift check. Probe the
  // cache, and if it exists with drift OR with a schema mismatch,
  // surface the FontCacheDriftModal so the user picks rescan / use
  // as-is / clear. cacheChecked is latched only by a live completed
  // effect instance: React StrictMode cancels its first development
  // setup, and that cancelled setup must not suppress the second one.
  const [cacheStatus, setCacheStatus] = useState<FontCacheStatus | null>(null);
  const [cacheProbeFailed, setCacheProbeFailed] = useState(false);
  const [cacheDrift, setCacheDrift] = useState<FontCacheDriftReport | null>(null);
  const [showCacheModal, setShowCacheModal] = useState(false);
  const cacheChecked = useRef(false);

  useEffect(() => {
    if (cacheChecked.current) return;
    let cancelled = false;
    void (async () => {
      try {
        const status = await openFontCache();
        if (cancelled) return;
        setCacheStatus(status);
        setCacheProbeFailed(false);
        if (status.schemaMismatch) {
          // Empty drift: cache file is unreadable, modal renders the
          // rebuild-required path (Clear cache button only).
          // dropped a redundant `if (cancelled)
          // return` here — JS is single-threaded and no await fires
          // between the initial post-`openFontCache` cancel check
          // (line above) and these setters. React's setState is
          // synchronous-schedule; it doesn't yield mid-call.
          setCacheDrift({ added: [], modified: [], removed: [] });
          setShowCacheModal(true);
          return;
        }
        if (!status.available) {
          // Init failed for a non-schema reason (logged Rust-side).
          // Nothing actionable to surface; fall back to system fonts.
          return;
        }
        try {
          const drift = await detectFontCacheDrift();
          if (cancelled) return;
          setCacheDrift(drift);
          if (drift.modified.length > 0 || drift.removed.length > 0) {
            setShowCacheModal(true);
          }
        } catch (e) {
          if (!cancelled) {
            console.warn("Font cache drift check failed; cached fonts remain available:", e);
          }
        }
      } catch (e) {
        if (!cancelled) {
          setCacheStatus(null);
          setCacheProbeFailed(true);
          console.warn("Font cache unavailable at launch; embed will use system fonts only:", e);
        }
      } finally {
        completeFontCacheProbe(cacheChecked, cancelled);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const handleCacheModalClose = useCallback(() => {
    setShowCacheModal(false);
  }, []);

  // Re-probe the cache instead of synthesizing the post-op state from
  // assumptions about what the Rust commands did. The probe is cheap
  // (one SQL query) and the rebuilt state is authoritative — avoids
  // tightly coupling the UI to internal command behavior, and closes
  // tiny race windows where another command mutated state in parallel.
  const refreshCacheStatus = useCallback(async () => {
    try {
      const status = await openFontCache();
      setCacheStatus(status);
      setCacheProbeFailed(false);
    } catch (e) {
      setCacheStatus(null);
      setCacheProbeFailed(true);
      console.warn("Font cache status re-probe failed; embed will use system fonts only:", e);
    }
  }, []);

  // Rescan and Clear converge to the same post-op state from the
  // parent's perspective (drift cleared, status re-probed); the modal
  // distinguishes them via its own working / doneMessage state. One
  // shared callback wired to both modal props.
  const handleCacheActionComplete = useCallback(() => {
    setCacheDrift({ added: [], modified: [], removed: [] });
    void refreshCacheStatus();
  }, [refreshCacheStatus]);

  const handleLicensesClose = useCallback(() => {
    setLicensesOpen(false);
  }, []);

  const modalVisibility = resolveAppModalVisibility(showCacheModal, licensesOpen);

  return (
    <div className="stage">
      <div className="window">
        {/* ── Titlebar ────────────────────────────── */}
        <div className="titlebar" data-tauri-drag-region>
          <svg
            className="titlebar-logo"
            viewBox="0 0 48 46"
            xmlns="http://www.w3.org/2000/svg"
            aria-hidden="true"
          >
            <defs>
              <linearGradient id="tl-hdrV" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0" stopColor="#fbbf24" />
                <stop offset="0.5" stopColor="#ec4899" />
                <stop offset="1" stopColor="#6d28d9" />
              </linearGradient>
              <linearGradient id="tl-hdrH" x1="0" y1="0" x2="1" y2="0">
                <stop offset="0" stopColor="#6b7280" />
                <stop offset="0.38" stopColor="#7c3aed" />
                <stop offset="0.72" stopColor="#ec4899" />
                <stop offset="1" stopColor="#fbbf24" />
              </linearGradient>
            </defs>
            <rect x="11" y="9" width="7" height="30" rx="1.2" fill="#64748b" />
            <rect x="30" y="9" width="7" height="30" rx="1.2" fill="url(#tl-hdrV)" />
            <rect x="11" y="20" width="26" height="7" fill="url(#tl-hdrH)" />
          </svg>
          <span className="titlebar-title">{t("app_title")}</span>
          <div className="titlebar-spacer" />
          <button
            className="titlebar-btn"
            onClick={() => appWindow?.minimize()}
            aria-label={t("titlebar_minimize")}
            title={t("titlebar_minimize")}
          >
            <svg width="10" height="10" viewBox="0 0 12 12" aria-hidden="true">
              <path stroke="currentColor" d="M2 6h8" />
            </svg>
          </button>
          <button
            className="titlebar-btn"
            onClick={() => appWindow?.toggleMaximize()}
            aria-label={t("titlebar_maximize")}
            title={t("titlebar_maximize")}
          >
            <svg width="10" height="10" viewBox="0 0 12 12" aria-hidden="true">
              <rect x="2" y="2" width="8" height="8" fill="none" stroke="currentColor" />
            </svg>
          </button>
          <button
            className="titlebar-btn close"
            onClick={() => appWindow?.close()}
            aria-label={t("titlebar_close")}
            title={t("titlebar_close")}
          >
            <svg width="10" height="10" viewBox="0 0 12 12" aria-hidden="true">
              <path stroke="currentColor" d="M3 3l6 6M9 3l-6 6" />
            </svg>
          </button>
        </div>

        {/* ── App Header ──────────────────────────── */}
        <div className="app-header">
          <div className="app-title-row">
            <div className="app-lockup">
              <div className="app-logo">
                <svg viewBox="0 0 512 512" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
                  <defs>
                    <linearGradient id="hl-bg" x1="0.2" y1="0" x2="1" y2="1">
                      <stop offset="0" stopColor="#1e1628" />
                      <stop offset="1" stopColor="#0a0710" />
                    </linearGradient>
                    <radialGradient id="hl-glow" cx="0.72" cy="0.38" r="0.75">
                      <stop offset="0" stopColor="#a78bfa" stopOpacity="0.24" />
                      <stop offset="0.5" stopColor="#a78bfa" stopOpacity="0.08" />
                      <stop offset="1" stopColor="#a78bfa" stopOpacity="0" />
                    </radialGradient>
                    <linearGradient id="hl-hdrV" x1="0" y1="0" x2="0" y2="1">
                      <stop offset="0" stopColor="#fbbf24" />
                      <stop offset="0.5" stopColor="#ec4899" />
                      <stop offset="1" stopColor="#6d28d9" />
                    </linearGradient>
                    <linearGradient id="hl-hdrH" x1="0" y1="0" x2="1" y2="0">
                      <stop offset="0" stopColor="#6b7280" />
                      <stop offset="0.38" stopColor="#7c3aed" />
                      <stop offset="0.72" stopColor="#ec4899" />
                      <stop offset="1" stopColor="#fbbf24" />
                    </linearGradient>
                  </defs>
                  <rect width="512" height="512" rx="108" ry="108" fill="url(#hl-bg)" />
                  <rect width="512" height="512" rx="108" ry="108" fill="url(#hl-glow)" />
                  <rect x="120" y="96" width="72" height="320" rx="12" ry="12" fill="#64748b" />
                  <rect
                    x="320"
                    y="96"
                    width="72"
                    height="320"
                    rx="12"
                    ry="12"
                    fill="url(#hl-hdrV)"
                  />
                  <rect x="120" y="220" width="272" height="72" fill="url(#hl-hdrH)" />
                </svg>
              </div>
              <div className="app-title-group">
                <div className="app-title">{t("app_title")}</div>
                <div className="app-tagline">{t("app_tagline")}</div>
              </div>
            </div>

            <div className="header-controls">
              <button
                className="icon-btn lang"
                onClick={() => setLang(lang === "en" ? "zh" : "en")}
                title={lang === "en" ? t("lang_switch_to_zh") : t("lang_switch_to_en")}
              >
                {lang === "en" ? "中" : "EN"}
              </button>

              <AppearanceMenu />
            </div>
          </div>

          {/* Tabs */}
          <nav className="tabs" role="tablist">
            {TAB_IDS.map((tab, i) => (
              <button
                key={tab.id}
                id={`tab-${tab.id}`}
                role="tab"
                aria-selected={activeTab === tab.id}
                aria-controls={`panel-${tab.id}`}
                onClick={() => setActiveTab(tab.id)}
                className="tab"
                data-active={activeTab === tab.id}
              >
                <span className="tab-index">{String(i + 1).padStart(2, "0")}</span>
                <span>{t(tab.labelKey)}</span>
              </button>
            ))}
          </nav>
        </div>

        {/* ── Main ────────────────────────────────── */}
        <main className="app-main">
          <div
            id="panel-hdr"
            role="tabpanel"
            aria-labelledby="tab-hdr"
            aria-hidden={activeTab !== "hdr"}
            style={{ display: activeTab === "hdr" ? "block" : "none" }}
          >
            <HdrConvert />
          </div>
          <div
            id="panel-timing"
            role="tabpanel"
            aria-labelledby="tab-timing"
            aria-hidden={activeTab !== "timing"}
            style={{ display: activeTab === "timing" ? "block" : "none" }}
          >
            <TimingShift />
          </div>
          <div
            id="panel-fonts"
            role="tabpanel"
            aria-labelledby="tab-fonts"
            aria-hidden={activeTab !== "fonts"}
            style={{ display: activeTab === "fonts" ? "block" : "none" }}
          >
            <FontEmbed cacheStatus={cacheStatus} cacheProbeFailed={cacheProbeFailed} />
          </div>
          <div
            id="panel-rename"
            role="tabpanel"
            aria-labelledby="tab-rename"
            aria-hidden={activeTab !== "rename"}
            style={{ display: activeTab === "rename" ? "block" : "none" }}
          >
            <BatchRename />
          </div>
          <div
            id="panel-style"
            role="tabpanel"
            aria-labelledby="tab-style"
            aria-hidden={activeTab !== "style"}
            style={{ display: activeTab === "style" ? "block" : "none" }}
          >
            <StyleEdit />
          </div>
        </main>

        {/* ── Footer ──────────────────────────────── */}
        <footer className="app-footer">
          <span className={`dot ${currentStatus.kind}`} aria-hidden="true" />
          <span>{currentStatus.message || t("footer_ready")}</span>
          {currentStatus.progress && currentStatus.progress.total > 0 && (
            <span className="footer-progress" aria-live="polite">
              {currentStatus.progress.processed}/{currentStatus.progress.total}
            </span>
          )}
          <span className="spacer" />
          <button
            type="button"
            className="footer-license-button"
            onClick={() => {
              if (!showCacheModal) setLicensesOpen(true);
            }}
            disabled={showCacheModal}
            aria-haspopup="dialog"
          >
            <span>{t("footer_version")}</span>
            <span aria-hidden="true">·</span>
            <span>{t("licenses_open")}</span>
          </button>
        </footer>
      </div>

      <FontCacheDriftModal
        open={modalVisibility.cache}
        status={cacheStatus}
        drift={cacheDrift}
        onClose={handleCacheModalClose}
        onRescanComplete={handleCacheActionComplete}
        onClearComplete={handleCacheActionComplete}
      />
      <AboutLicensesModal open={modalVisibility.licenses} onClose={handleLicensesClose} />
    </div>
  );
}

export default App;
