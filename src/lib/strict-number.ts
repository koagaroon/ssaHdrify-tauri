const FINITE_DECIMAL_TEXT_PATTERN = /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/;
const MAX_FINITE_NUMBER_TEXT_LENGTH = 128;

export function parseFiniteNumberText(text: string): number | null {
  const trimmed = text.trim();
  if (
    trimmed === "" ||
    trimmed.length > MAX_FINITE_NUMBER_TEXT_LENGTH ||
    !FINITE_DECIMAL_TEXT_PATTERN.test(trimmed)
  ) {
    return null;
  }

  const value = Number(trimmed);
  return Number.isFinite(value) ? value : null;
}
