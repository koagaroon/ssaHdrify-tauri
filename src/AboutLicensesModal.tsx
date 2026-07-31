import { useEffect, useId, useRef } from "react";
import { useI18n } from "./i18n/useI18n";
import { LICENSE_NOTICES, type LicenseNoticeId } from "./lib/license-notices";

interface Props {
  open: boolean;
  onClose: () => void;
}

const SUMMARY_KEYS: Record<LicenseNoticeId, string> = {
  ssahdrify: "licenses_ssahdrify_summary",
  inter: "licenses_inter_summary",
  "smiley-sans": "licenses_smiley_sans_summary",
  feather: "licenses_feather_summary",
};

const FOCUSABLE_SELECTOR = [
  "button:not([disabled])",
  "a[href]",
  "summary",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

function isRenderedFocusable(element: HTMLElement): boolean {
  const closedDetails = element.closest("details:not([open])");
  // Closed disclosures can report child geometry even though only their summary is focusable.
  if (closedDetails && closedDetails.querySelector(":scope > summary") !== element) return false;
  return element.getClientRects().length > 0;
}

export default function AboutLicensesModal({ open, onClose }: Props): React.ReactElement | null {
  const { t } = useI18n();
  const titleId = useId();
  const descriptionId = useId();
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const closeButtonRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    if (!open) return;

    const previouslyFocused = document.activeElement;
    closeButtonRef.current?.focus();

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        onClose();
        return;
      }

      if (event.key !== "Tab") return;
      const focusable = Array.from(
        dialogRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR) ?? []
      ).filter(isRenderedFocusable);
      if (focusable.length === 0) {
        event.preventDefault();
        return;
      }

      const first = focusable[0]!;
      const last = focusable[focusable.length - 1]!;
      const activeElement = document.activeElement;
      if (!dialogRef.current?.contains(activeElement)) {
        event.preventDefault();
        (event.shiftKey ? last : first).focus();
      } else if (event.shiftKey && activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };

    window.addEventListener("keydown", handleKeyDown, true);
    return () => {
      window.removeEventListener("keydown", handleKeyDown, true);
      if (previouslyFocused instanceof HTMLElement) previouslyFocused.focus();
    };
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      className="modal-scrim"
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        ref={dialogRef}
        className="modal licenses-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={descriptionId}
      >
        <div className="modal-head">
          <div className="modal-head-text">
            <div id={titleId} className="modal-title">
              {t("licenses_title")}
            </div>
            <div id={descriptionId} className="modal-sub">
              {t("licenses_intro")}
            </div>
          </div>
          <button
            ref={closeButtonRef}
            type="button"
            onClick={onClose}
            className="modal-close"
            title={t("licenses_close")}
            aria-label={t("licenses_close")}
          >
            <svg
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
            >
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </button>
        </div>

        <div className="modal-body licenses-body">
          <p className="licenses-legal-note" role="note">
            {t("licenses_exact_text_note")}
          </p>

          <div className="license-notice-list">
            {LICENSE_NOTICES.map((notice) => (
              <details key={notice.id} className="license-notice" open={notice.id === "ssahdrify"}>
                <summary className="license-notice-summary">
                  <span className="license-notice-name">{notice.name}</span>
                  <span className="license-notice-id">{notice.licenseId}</span>
                </summary>
                <div className="license-notice-body">
                  <p>{t(SUMMARY_KEYS[notice.id])}</p>
                  <p className="license-notice-source">
                    <span>{t("licenses_source")}</span>
                    <code>{notice.source}</code>
                  </p>
                  <div className="license-full-text-label">{t("licenses_full_text")}</div>
                  <pre className="license-full-text" tabIndex={0}>
                    {notice.text}
                  </pre>
                </div>
              </details>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
