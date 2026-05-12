/**
 * Theme-aware number input with custom +/- buttons.
 * Uses CSS variables for colors.
 */
interface NumberInputProps {
  value: string | number;
  onChange: (value: string) => void;
  min?: number;
  max?: number;
  step?: number | string;
  disabled?: boolean;
  className?: string;
  /** id forwarded to the inner <input> so a sibling <label htmlFor> works */
  id?: string;
  /**
   * Caller-derived invalid signal (N-R5-FEFEAT-25). When true, the
   * border switches to var(--accent-danger) so the user sees the
   * out-of-range / unparseable input instead of the silent fallback
   * to the prior valid value.
   */
  invalid?: boolean;
}

export default function NumberInput({
  value,
  onChange,
  min,
  max,
  step = 1,
  disabled = false,
  className = "",
  id,
  invalid = false,
}: NumberInputProps) {
  const numStep = typeof step === "string" ? parseFloat(step) : step;
  const inputClass = `num-input w-full pl-3 pr-7 py-2 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-[var(--accent)]${
    disabled ? " is-disabled" : ""
  }`;

  const adjust = (delta: number) => {
    const current = typeof value === "string" ? parseFloat(value) : value;
    if (Number.isNaN(current)) return;
    let next = current + delta;
    if (min !== undefined) next = Math.max(min, next);
    if (max !== undefined) next = Math.min(max, next);
    const decimals = String(numStep).split(".")[1]?.length ?? 0;
    onChange(next.toFixed(decimals));
  };

  return (
    <div className={`relative flex items-stretch ${className}`}>
      <input
        type="number"
        id={id}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        min={min}
        max={max}
        step={step}
        disabled={disabled}
        className={inputClass}
        style={{
          background: "var(--bg-input)",
          // template wraps a literal-branched ternary; all branches are
          // static var(--token) strings — safe by inspection.
          // eslint-disable-next-line no-restricted-syntax
          border: `1px solid ${
            invalid
              ? "var(--accent-danger)"
              : disabled
                ? "var(--border-light)"
                : "var(--border)"
          }`,
          color: "var(--text-primary)",
        }}
      />
      <div
        className="absolute right-0 top-0 bottom-0 flex flex-col w-6 rounded-r-lg overflow-hidden"
        style={{ borderLeft: "1px solid var(--border)" }}
      >
        <button
          type="button"
          tabIndex={-1}
          onClick={() => adjust(numStep)}
          disabled={disabled}
          className="num-spin"
        >
          <svg width="8" height="5" viewBox="0 0 8 5">
            <path d="M4 0L8 5H0L4 0Z" fill="currentColor" />
          </svg>
        </button>
        <div style={{ height: "1px", background: "var(--border)" }} />
        <button
          type="button"
          tabIndex={-1}
          onClick={() => adjust(-numStep)}
          disabled={disabled}
          className="num-spin"
        >
          <svg width="8" height="5" viewBox="0 0 8 5">
            <path d="M4 5L0 0H8L4 5Z" fill="currentColor" />
          </svg>
        </button>
      </div>
    </div>
  );
}
