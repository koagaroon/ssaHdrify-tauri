const FINITE_DECIMAL_TEXT_PATTERN = /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/;

export function parseFiniteNumberText(text: string): number | null {
  const trimmed = text.trim();
  if (trimmed === "" || !FINITE_DECIMAL_TEXT_PATTERN.test(trimmed)) return null;

  const value = Number(trimmed);
  return Number.isFinite(value) ? value : null;
}
