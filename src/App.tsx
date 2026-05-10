import { useCallback, useEffect, useState, useRef, useMemo } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import HdrConvert from "./features/hdr-convert/HdrConvert";
import TimingShift from "./features/timing-shift/TimingShift";
import FontEmbed from "./features/font-embed/FontEmbed";
import BatchRename from "./features/batch-rename/BatchRename";
import FontCacheDriftModal from "./features/font-embed/FontCacheDriftModal";
import { useI18n } from "./i18n/useI18n";
import { useTheme } from "./theme/useTheme";
import type { ThemeMode } from "./theme/useTheme";
import { useStatus, type StatusTab } from "./lib/StatusContext";
import { useClickOutside } from "./lib/useClickOutside";
import {
  openFontCache,
  detectFontCacheDrift,
  type FontCacheStatus,
  type FontCacheDriftReport,
} from "./lib/tauri-api";
import "./shell.css";

// Tab ids also serve as StatusTab keys — single source of truth.
type Tab = StatusTab;

// Source of truth for tab labels — the Record<Tab, ...> type forces
// every Tab variant to have an entry, so adding a new tab to the
// StatusTab union without adding a label here fails at compile time.
// Visual tab-strip order is the declaration order below (JS guarantees
// insertion-order iteration for string keys). Mirrors THEME_LABEL_KEYS
// below and TAB_LABEL_KEYS in lib/tab-labels.ts.
const TAB_LABEL_KEYS: Record<Tab, string> = {
  hdr: "tab_hdr",
  timing: "tab_timing",
  fonts: "tab_fonts",
  rename: "tab_rename",
};

const TAB_IDS: { id: Tab; labelKey: string }[] = (Object.keys(TAB_LABEL_KEYS) as Tab[]).map(
  (id) => ({ id, labelKey: TAB_LABEL_KEYS[id] })
);

const THEME_OPTIONS: { mode: ThemeMode; labelKey: string }[] = [
  { mode: "auto", labelKey: "theme_auto" },
  { mode: "light", labelKey: "theme_light" },
  { mode: "dark", labelKey: "theme_dark" },
];

// Theme label keys — explicit map instead of `t("theme_" + mode)` string
// concat. If a ThemeMode value is ever renamed the compiler will flag the
// gap here, where silent string concat would just return the bare key.
const THEME_LABEL_KEYS: Record<ThemeMode, string> = {
  auto: "theme_auto",
  light: "theme_light",
  dark: "theme_dark",
};

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("hdr");
  const { t, lang, setLang } = useI18n();
  const { mode, appearance, setMode } = useTheme();
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

  const [themeOpen, setThemeOpen] = useState(false);
  const themeRef = useRef<HTMLDivElement>(null);
  // Close on click-outside + Escape — see useClickOutside for the
  // armed-on-next-tick rationale.
  useClickOutside(themeOpen, themeRef, () => setThemeOpen(false));

  // Persistent font cache (#5): launch-time drift check. Probe the
  // cache, and if it exists with drift OR with a schema mismatch,
  // surface the FontCacheDriftModal so the user picks rescan / use
  // as-is / clear. The cacheChecked ref guards against StrictMode's
  // intentional double-mount in dev — drift queries are read-only
  // but rescan_drifted writes; no need to double-do that.
  //
  // Single-App-instance assumption: the ref-based guard relies on
  // App rendering exactly once at the root. Multi-instance App
  // rendering (e.g., a future routing refactor) would share the ref
  // across instances and skip launch checks for all but the first.
  const [cacheStatus, setCacheStatus] = useState<FontCacheStatus | null>(null);
  const [cacheDrift, setCacheDrift] = useState<FontCacheDriftReport | null>(null);
  const [showCacheModal, setShowCacheModal] = useState(false);
  const cacheChecked = useRef(false);

  useEffect(() => {
    if (cacheChecked.current) return;
    cacheChecked.current = true;
    let cancelled = false;
    (async () => {
      try {
        const status = await openFontCache();
        if (cancelled) return;
        setCacheStatus(status);
        if (status.schemaMismatch) {
          // Empty drift: cache file is unreadable, modal renders the
          // rebuild-required path (Clear cache button only).
          setCacheDrift({ added: [], modified: [], removed: [] });
          setShowCacheModal(true);
          return;
        }
        if (!status.available) {
          // Init failed for a non-schema reason (logged Rust-side).
          // Nothing actionable to surface; fall back to system fonts.
          return;
        }
        const drift = await detectFontCacheDrift();
        if (cancelled) return;
        setCacheDrift(drift);
        if (drift.modified.length > 0 || drift.removed.length > 0) {
          setShowCacheModal(true);
        }
      } catch (e) {
        // Don't block app launch on cache probe failures — the user
        // can still use the app, embed just falls through to system
        // fonts. Log so devs see it during tauri dev.
        console.warn("font cache launch check failed:", e);
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
    } catch (e) {
      console.warn("openFontCache re-probe failed:", e);
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

  return (
    <div className="stage">
      <div className="stage-bloom" aria-hidden="true" />
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
                title={lang === "en" ? "切换到中文" : "Switch to English"}
              >
                {lang === "en" ? "中" : "EN"}
              </button>

              <div ref={themeRef} style={{ position: "relative" }}>
                <button
                  className="icon-btn"
                  onClick={() => setThemeOpen(!themeOpen)}
                  title={t(THEME_LABEL_KEYS[mode])}
                >
                  {appearance === "light" ? (
                    <svg
                      width="15"
                      height="15"
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke="currentColor"
                      strokeWidth="2"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                      aria-hidden="true"
                    >
                      <circle cx="12" cy="12" r="5" />
                      <line x1="12" y1="1" x2="12" y2="3" />
                      <line x1="12" y1="21" x2="12" y2="23" />
                      <line x1="4.22" y1="4.22" x2="5.64" y2="5.64" />
                      <line x1="18.36" y1="18.36" x2="19.78" y2="19.78" />
                      <line x1="1" y1="12" x2="3" y2="12" />
                      <line x1="21" y1="12" x2="23" y2="12" />
                      <line x1="4.22" y1="19.78" x2="5.64" y2="18.36" />
                      <line x1="18.36" y1="5.64" x2="19.78" y2="4.22" />
                    </svg>
                  ) : (
                    <svg
                      width="15"
                      height="15"
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke="currentColor"
                      strokeWidth="2"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                      aria-hidden="true"
                    >
                      <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
                    </svg>
                  )}
                </button>

                {themeOpen && (
                  <div
                    className="absolute right-0 mt-1 py-1 rounded-lg z-50"
                    style={{
                      background: "var(--bg-app)",
                      border: "1px solid var(--border)",
                      boxShadow: "var(--shadow-popover)",
                      minWidth: "140px",
                    }}
                  >
                    {THEME_OPTIONS.map((opt, idx) => (
                      <div key={opt.mode}>
                        {idx === 1 && (
                          <div
                            className="my-1"
                            style={{ height: "1px", background: "var(--border-light)" }}
                          />
                        )}
                        <button
                          onClick={() => {
                            setMode(opt.mode);
                            setThemeOpen(false);
                          }}
                          className="theme-menu-item"
                        >
                          <span>{t(opt.labelKey)}</span>
                          {mode === opt.mode && (
                            <span
                              style={{
                                color: "var(--accent)",
                                fontSize: "1.25rem",
                                lineHeight: 1,
                              }}
                            >
                              {"\u2022"}
                            </span>
                          )}
                        </button>
                      </div>
                    ))}
                  </div>
                )}
              </div>
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
            style={{ display: activeTab === "hdr" ? "block" : "none" }}
          >
            <HdrConvert />
          </div>
          <div
            id="panel-timing"
            role="tabpanel"
            aria-labelledby="tab-timing"
            style={{ display: activeTab === "timing" ? "block" : "none" }}
          >
            <TimingShift />
          </div>
          <div
            id="panel-fonts"
            role="tabpanel"
            aria-labelledby="tab-fonts"
            style={{ display: activeTab === "fonts" ? "block" : "none" }}
          >
            <FontEmbed />
          </div>
          <div
            id="panel-rename"
            role="tabpanel"
            aria-labelledby="tab-rename"
            style={{ display: activeTab === "rename" ? "block" : "none" }}
          >
            <BatchRename />
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
          <span className="ver">{t("footer_version")}</span>
        </footer>
      </div>

      <FontCacheDriftModal
        open={showCacheModal}
        status={cacheStatus}
        drift={cacheDrift}
        onClose={handleCacheModalClose}
        onRescanComplete={handleCacheActionComplete}
        onClearComplete={handleCacheActionComplete}
      />
    </div>
  );
}

export default App;
