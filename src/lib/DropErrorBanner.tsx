/**
 * DropErrorBanner — sticky "selection rejected" alert shown above the
 * file strip when a pick / drop fails the cross-tab dedup or the
 * "no usable files in folder" check.
 *
 * Each batch tab originally inlined the same JSX block verbatim
 * (alert role + cancel-bg + dismiss ✕). Behavior contract is
 * shared: the banner stays visible until the user makes another
 * selection or clicks the dismiss ✕, since the prior selection is
 * preserved (strict-reject pattern).
 */
import { useI18n } from "../i18n/useI18n";

interface DropErrorBannerProps {
  /** Message to display, or null/empty to hide the banner. */
  message: string | null;
  /** Called when the user clicks the dismiss ✕. */
  onDismiss: () => void;
}

export function DropErrorBanner({ message, onDismiss }: DropErrorBannerProps) {
  const { t } = useI18n();
  if (!message) return null;

  return (
    <div
      className="flex items-center justify-between gap-3 px-3 py-2 rounded-lg text-sm"
      role="alert"
      style={{
        background: "var(--cancel-bg)",
        border: "1px solid var(--error)",
        color: "var(--error)",
      }}
    >
      <span>{message}</span>
      <button
        type="button"
        onClick={onDismiss}
        aria-label={t("btn_clear_file")}
        className="flex-none text-base"
        style={{ color: "var(--error)", lineHeight: 1 }}
      >
        ✕
      </button>
    </div>
  );
}
