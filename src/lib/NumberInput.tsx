/**
 * Dark-themed number input with custom +/- buttons.
 * Replaces the jarring native spinner arrows with subtle controls.
 */
interface NumberInputProps {
  value: string | number;
  onChange: (value: string) => void;
  min?: number;
  max?: number;
  step?: number | string;
  disabled?: boolean;
  className?: string;
}

export default function NumberInput({
  value,
  onChange,
  min,
  max,
  step = 1,
  disabled = false,
  className = "",
}: NumberInputProps) {
  const numStep = typeof step === "string" ? parseFloat(step) : step;

  const adjust = (delta: number) => {
    const current = typeof value === "string" ? parseFloat(value) : value;
    if (Number.isNaN(current)) return;
    let next = current + delta;
    if (min !== undefined) next = Math.max(min, next);
    if (max !== undefined) next = Math.min(max, next);
    // Preserve decimal precision from step
    const decimals = String(numStep).split(".")[1]?.length ?? 0;
    onChange(next.toFixed(decimals));
  };

  return (
    <div className={`relative flex items-stretch ${className}`}>
      <input
        type="number"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        min={min}
        max={max}
        step={step}
        disabled={disabled}
        className="w-full pl-3 pr-7 py-2 rounded-lg bg-neutral-800 border border-neutral-700 text-neutral-100 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500 disabled:opacity-50"
      />
      <div className="absolute right-0 top-0 bottom-0 flex flex-col w-6 border-l border-neutral-700 rounded-r-lg overflow-hidden">
        <button
          type="button"
          tabIndex={-1}
          onClick={() => adjust(numStep)}
          disabled={disabled}
          className="flex-1 flex items-center justify-center bg-neutral-800 hover:bg-neutral-700 active:bg-neutral-600 disabled:opacity-30 transition-colors"
        >
          <svg width="8" height="5" viewBox="0 0 8 5" className="text-neutral-400">
            <path d="M4 0L8 5H0L4 0Z" fill="currentColor" />
          </svg>
        </button>
        <div className="h-px bg-neutral-700" />
        <button
          type="button"
          tabIndex={-1}
          onClick={() => adjust(-numStep)}
          disabled={disabled}
          className="flex-1 flex items-center justify-center bg-neutral-800 hover:bg-neutral-700 active:bg-neutral-600 disabled:opacity-30 transition-colors"
        >
          <svg width="8" height="5" viewBox="0 0 8 5" className="text-neutral-400">
            <path d="M4 5L0 0H8L4 5Z" fill="currentColor" />
          </svg>
        </button>
      </div>
    </div>
  );
}
