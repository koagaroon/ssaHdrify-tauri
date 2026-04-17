import { useState, useRef, useEffect } from "react";
import HdrConvert from "./features/hdr-convert/HdrConvert";
import TimingShift from "./features/timing-shift/TimingShift";
import FontEmbed from "./features/font-embed/FontEmbed";
import { useI18n } from "./i18n/useI18n";
import { useTheme } from "./theme/useTheme";
import type { ThemeMode } from "./theme/useTheme";

type Tab = "hdr" | "timing" | "fonts";

const TAB_IDS: { id: Tab; labelKey: string }[] = [
  { id: "hdr", labelKey: "tab_hdr" },
  { id: "timing", labelKey: "tab_timing" },
  { id: "fonts", labelKey: "tab_fonts" },
];

const THEME_OPTIONS: { mode: ThemeMode; labelKey: string }[] = [
  { mode: "auto", labelKey: "theme_auto" },
  { mode: "light", labelKey: "theme_light" },
  { mode: "dark", labelKey: "theme_dark" },
];

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("hdr");
  const { t, lang, setLang } = useI18n();
  const { mode, appearance, setMode } = useTheme();

  // ── Theme popover state ─────────────────────────────────
  const [themeOpen, setThemeOpen] = useState(false);
  const themeRef = useRef<HTMLDivElement>(null);

  // Close popover on click outside
  useEffect(() => {
    if (!themeOpen) return;
    const handler = (e: MouseEvent) => {
      if (themeRef.current && !themeRef.current.contains(e.target as Node)) {
        setThemeOpen(false);
      }
    };
    // Defer to avoid catching the opening click
    const id = setTimeout(() => document.addEventListener("mousedown", handler), 0);
    return () => {
      clearTimeout(id);
      document.removeEventListener("mousedown", handler);
    };
  }, [themeOpen]);

  // Close on Escape
  useEffect(() => {
    if (!themeOpen) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") setThemeOpen(false);
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [themeOpen]);

  return (
    <div
      className="flex flex-col h-screen"
      style={{ background: "var(--bg-app)", color: "var(--text-primary)" }}
    >
      {/* ── Header ─────────────────────────────────── */}
      <header
        className="flex-none"
        style={{ borderBottom: "1px solid var(--border)", background: "var(--bg-header)" }}
      >
        <div className="px-5 pt-4 pb-0 flex items-start justify-between">
          <div>
            <h1 className="text-lg font-semibold tracking-tight">{t("app_title")}</h1>
            <nav className="flex gap-1 mt-3" role="tablist">
              {TAB_IDS.map((tab) => (
                <button
                  key={tab.id}
                  id={`tab-${tab.id}`}
                  role="tab"
                  aria-selected={activeTab === tab.id}
                  aria-controls={`panel-${tab.id}`}
                  onClick={() => setActiveTab(tab.id)}
                  className="px-4 py-2 text-sm font-medium rounded-t-lg transition-colors"
                  style={
                    activeTab === tab.id
                      ? {
                          background: "var(--bg-app)",
                          color: "var(--text-primary)",
                          borderTop: "1px solid var(--border)",
                          borderLeft: "1px solid var(--border)",
                          borderRight: "1px solid var(--border)",
                        }
                      : { color: "var(--text-muted)" }
                  }
                  onMouseEnter={(e) => {
                    if (activeTab !== tab.id) e.currentTarget.style.color = "var(--text-secondary)";
                  }}
                  onMouseLeave={(e) => {
                    if (activeTab !== tab.id) e.currentTarget.style.color = "var(--text-muted)";
                  }}
                >
                  {t(tab.labelKey)}
                </button>
              ))}
            </nav>
          </div>

          {/* ── Controls: Language + Theme ─────────── */}
          <div className="flex items-center gap-2 mt-1">
            {/* Language toggle */}
            <button
              onClick={() => setLang(lang === "en" ? "zh" : "en")}
              className="w-8 h-8 flex items-center justify-center rounded-md text-xs font-medium transition-colors"
              style={{
                background: "var(--bg-input)",
                border: "1px solid var(--border)",
                color: "var(--text-secondary)",
              }}
              title={lang === "en" ? "切换到中文" : "Switch to English"}
            >
              {lang === "en" ? "中" : "EN"}
            </button>

            {/* Theme menu button + popover */}
            <div className="relative" ref={themeRef}>
              <button
                onClick={() => setThemeOpen(!themeOpen)}
                className="w-8 h-8 flex items-center justify-center rounded-md transition-colors"
                style={{
                  background: "var(--bg-input)",
                  border: "1px solid var(--border)",
                  color: "var(--text-secondary)",
                }}
                title={t("theme_" + mode)}
              >
                {appearance === "light" ? (
                  <svg
                    width="16"
                    height="16"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
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
                    width="16"
                    height="16"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                  >
                    <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
                  </svg>
                )}
              </button>

              {/* Popover menu */}
              {themeOpen && (
                <div
                  className="absolute right-0 mt-1 py-1 rounded-lg shadow-lg z-50"
                  style={{
                    background: "var(--bg-app)",
                    border: "1px solid var(--border)",
                    minWidth: "140px",
                  }}
                >
                  {THEME_OPTIONS.map((opt, idx) => (
                    <div key={opt.mode}>
                      {/* Separator after "Follow System" */}
                      {idx === 1 && (
                        <div
                          className="my-1"
                          style={{ height: "1px", background: "var(--border)" }}
                        />
                      )}
                      <button
                        onClick={() => {
                          setMode(opt.mode);
                          setThemeOpen(false);
                        }}
                        className="w-full text-left px-3 py-1.5 text-sm transition-colors flex items-center justify-between"
                        style={{ color: "var(--text-primary)" }}
                        onMouseEnter={(e) => {
                          e.currentTarget.style.background = "var(--bg-hover)";
                        }}
                        onMouseLeave={(e) => {
                          e.currentTarget.style.background = "transparent";
                        }}
                      >
                        <span>{t(opt.labelKey)}</span>
                        {/* Bullet indicator for active mode */}
                        {mode === opt.mode && (
                          <span
                            style={{ color: "var(--accent)", fontSize: "1.25rem", lineHeight: 1 }}
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
      </header>

      {/* ── Tab content ──────────────────────────────
           All tabs stay mounted (CSS visibility) so local state
           (offset values, style panel, threshold, etc.) survives
           tab switches. File state lives in FileContext above. */}
      <main className="flex-1 overflow-y-auto p-5">
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
      </main>

      {/* ── Footer ─────────────────────────────────── */}
      <footer
        className="flex-none px-5 py-2 text-xs"
        style={{ color: "var(--text-muted)", borderTop: "1px solid var(--border)" }}
      >
        {t("footer_version")}
      </footer>
    </div>
  );
}

export default App;
