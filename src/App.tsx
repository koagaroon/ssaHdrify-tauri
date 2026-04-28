import { useState, useRef, useEffect, useMemo } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import HdrConvert from "./features/hdr-convert/HdrConvert";
import TimingShift from "./features/timing-shift/TimingShift";
import FontEmbed from "./features/font-embed/FontEmbed";
import BatchRename from "./features/batch-rename/BatchRename";
import { useI18n } from "./i18n/useI18n";
import { useTheme } from "./theme/useTheme";
import type { ThemeMode } from "./theme/useTheme";
import { useStatus, type StatusTab, DEFAULT_STATUS } from "./lib/StatusContext";
import "./shell.css";

// Tab ids also serve as StatusTab keys — single source of truth.
type Tab = StatusTab;

const TAB_IDS: { id: Tab; labelKey: string }[] = [
  { id: "hdr", labelKey: "tab_hdr" },
  { id: "timing", labelKey: "tab_timing" },
  { id: "fonts", labelKey: "tab_fonts" },
  { id: "rename", labelKey: "tab_rename" },
];

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
  const currentStatus = statuses[activeTab] ?? DEFAULT_STATUS;
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

  // Close on click-outside + Escape. Mousedown is armed on the next tick
  // so the initial click that opened the dropdown doesn't immediately
  // close it. Matches the same pattern used by HdrConvert's file list.
  useEffect(() => {
    if (!themeOpen) return;
    const onClick = (e: MouseEvent) => {
      if (themeRef.current && !themeRef.current.contains(e.target as Node)) {
        setThemeOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setThemeOpen(false);
    };
    const id = setTimeout(() => document.addEventListener("mousedown", onClick), 0);
    document.addEventListener("keydown", onKey);
    return () => {
      clearTimeout(id);
      document.removeEventListener("mousedown", onClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [themeOpen]);

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
    </div>
  );
}

export default App;
