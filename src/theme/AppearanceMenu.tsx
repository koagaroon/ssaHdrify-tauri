import { useEffect, useId, useRef, useState } from "react";
import { useI18n } from "../i18n/useI18n";
import { useClickOutside } from "../lib/useClickOutside";
import { useTheme, type ThemeMode, type ThemePalette } from "./useTheme";

const MODE_LABELS: Record<ThemeMode, string> = {
  auto: "theme_auto",
  light: "theme_light",
  dark: "theme_dark",
};
const PALETTE_LABELS: Record<ThemePalette, string> = {
  cool: "palette_cool",
  warm: "palette_warm",
};

export default function AppearanceMenu() {
  const { mode, appearance, palette, setMode, setPalette } = useTheme();
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const menuId = useId();
  useClickOutside(open, containerRef, () => setOpen(false));
  useEffect(() => {
    if (open) containerRef.current?.querySelector<HTMLInputElement>("input:checked")?.focus();
  }, [open]);

  return (
    <div ref={containerRef} style={{ position: "relative" }}>
      <button
        ref={buttonRef}
        type="button"
        className="icon-btn"
        onClick={() => setOpen(!open)}
        title={t("appearance_current", t(PALETTE_LABELS[palette]), t(MODE_LABELS[mode]))}
        aria-label={t("appearance")}
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        aria-haspopup="dialog"
      >
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
          {appearance === "light" ? (
            <>
              <circle cx="12" cy="12" r="5" />
              <path d="M12 1v2M12 21v2M4.22 4.22l1.42 1.42M18.36 18.36l1.42 1.42M1 12h2M21 12h2M4.22 19.78l1.42-1.42M18.36 5.64l1.42-1.42" />
            </>
          ) : (
            <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
          )}
        </svg>
      </button>
      {open && (
        <div
          id={menuId}
          className="appearance-menu"
          role="dialog"
          aria-label={t("appearance")}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              setOpen(false);
              buttonRef.current?.focus();
            }
          }}
        >
          <fieldset className="appearance-section">
            <legend className="appearance-label">{t("appearance_palette")}</legend>
            <div className="palette-options">
              {(Object.keys(PALETTE_LABELS) as ThemePalette[]).map((option) => (
                <label key={option} className="palette-option">
                  <input
                    type="radio"
                    name={`${menuId}-palette`}
                    value={option}
                    checked={palette === option}
                    onChange={() => setPalette(option)}
                  />
                  <span className="palette-swatch" data-palette={option} aria-hidden="true" />
                  <span>{t(PALETTE_LABELS[option])}</span>
                </label>
              ))}
            </div>
          </fieldset>
          <fieldset className="appearance-section">
            <legend className="appearance-label">{t("appearance_mode")}</legend>
            <div className="mode-options">
              {(Object.keys(MODE_LABELS) as ThemeMode[]).map((option) => (
                <label key={option} className="mode-option">
                  <input
                    type="radio"
                    name={`${menuId}-mode`}
                    value={option}
                    checked={mode === option}
                    onChange={() => setMode(option)}
                  />
                  <span>{t(MODE_LABELS[option])}</span>
                </label>
              ))}
            </div>
          </fieldset>
        </div>
      )}
    </div>
  );
}
