/**
 * PreviewTable — generic column-driven table component.
 *
 * Hosts both the simple-tab "input → action → output" 3-column shape used
 * by HDR Convert / Time Shift / Font Embed batch flows and the 5-column
 * pairing grid Tab 4 (Batch Rename) needs (# / video / subtitle / source
 * / remove). The column config drives both render and grid track sizing,
 * so a tab swaps shape just by passing different `columns`.
 *
 * Header strip is rendered above the scroll area (Excel-freeze-row
 * pattern) — sticky-positioning a row inside a CSS Grid container is
 * fragile across browsers, so the header is a sibling that mirrors the
 * row's grid template.
 */
import type { HTMLAttributes, JSX, ReactNode } from "react";

export interface PreviewTableColumn<T> {
  /** Stable key for React reconciliation across columns. */
  key: string;
  /** Header cell content. Plain string, JSX badge, or icon all valid. */
  header: ReactNode;
  /** CSS grid track size for this column (e.g., "32px", "1fr", "minmax(120px, 1fr)").
   *  Defaults to "1fr" when omitted. */
  width?: string;
  /** Cell renderer — receives the row value and its index. */
  render: (row: T, rowIndex: number) => ReactNode;
  /** Optional className applied to every cell in this column (header + body).
   *  Useful for column-wide alignment, font, or color tweaks. */
  className?: string;
  /** Convenience for `text-align`-style alignment. */
  align?: "start" | "center" | "end";
}

export interface PreviewTableProps<T> {
  /** Row data. Empty array shows `emptyMessage`. */
  rows: T[];
  /** Column configuration — drives render + grid sizing + headers. */
  columns: PreviewTableColumn<T>[];
  /** Stable key for each row. Pure index works but is fragile against
   *  reorder / removal — prefer a content-derived key when possible. */
  rowKey: (row: T, rowIndex: number) => string;
  /** Optional title strip above the header (e.g., "Pairing preview · 12 items"). */
  title?: ReactNode;
  /** Rendered when `rows` is empty. Plain string or rich JSX both work. */
  emptyMessage?: ReactNode;
  /** Max height of the scroll area. Default `280px` matches the existing
   *  Time Shift timeline preview. */
  maxHeight?: string;
  /** Optional className appended to the root container. */
  className?: string;
  /** Per-row className computed from data — for state-driven row styling
   *  (e.g., "warning" rows, dimmed unchanged rows). */
  rowClassName?: (row: T, rowIndex: number) => string | undefined;
  /** Per-row HTML attributes — drag/drop handlers, ARIA, dataset etc.
   *  Returned object is spread onto the row's container div. The
   *  `key`, `className`, and `style` slots are owned by PreviewTable
   *  and will not be overridden.
   *
   *  Allocation note: callers typically return a fresh `{}` per row,
   *  which triggers a new spread on every render. Acceptable here
   *  because the cost is one shallow-spread per row (≪ React's own
   *  reconcile work) and callers that need stable identity can
   *  memoize the function with useCallback + a stable lookup map.
   *  Don't try to deep-compare here — caller-provided objects can
   *  carry handler closures that legitimately change identity. */
  rowProps?: (row: T, rowIndex: number) => HTMLAttributes<HTMLDivElement>;
}

export function PreviewTable<T>({
  rows,
  columns,
  rowKey,
  title,
  emptyMessage,
  maxHeight = "280px",
  className,
  rowClassName,
  rowProps,
}: PreviewTableProps<T>): JSX.Element {
  // Inline grid-template-columns so any number of columns / track sizes
  // can be declared at the call site without touching CSS. The body
  // default is "1fr" — explicit fixed widths must be opted into per column.
  const gridTemplate = columns.map((c) => c.width || "1fr").join(" ");

  const rootClass = ["preview-table", className].filter(Boolean).join(" ");

  return (
    <div className={rootClass}>
      {title && <div className="preview-table-title">{title}</div>}
      {/* gridTemplate is built from columns[].width props from call-site components, never user input. */}
      {/* eslint-disable-next-line no-restricted-syntax */}
      <div className="preview-table-head" style={{ gridTemplateColumns: gridTemplate }}>
        {columns.map((col) => {
          const cellClass = ["preview-table-cell", col.className].filter(Boolean).join(" ");
          return (
            <span
              key={col.key}
              className={cellClass}
              style={col.align ? { justifySelf: col.align } : undefined}
            >
              {col.header}
            </span>
          );
        })}
      </div>
      {/* maxHeight is a CSS-shape prop (string/number) controlled by the call-site, never user input. */}
      {/* eslint-disable-next-line no-restricted-syntax */}
      <div className="preview-table-body" style={{ maxHeight }}>
        {rows.length === 0 ? (
          <div className="preview-table-empty">{emptyMessage}</div>
        ) : (
          rows.map((row, rowIndex) => {
            const extraClass = rowClassName?.(row, rowIndex);
            const rowClass = ["preview-table-row", extraClass].filter(Boolean).join(" ");
            const extraProps = rowProps?.(row, rowIndex) ?? {};
            return (
              <div
                {...extraProps}
                key={rowKey(row, rowIndex)}
                className={rowClass}
                style={{
                  // gridTemplate is column-config-derived from the
                  // call-site columns prop, never user input — same
                  // shape as the head row above.
                  // eslint-disable-next-line no-restricted-syntax
                  gridTemplateColumns: gridTemplate,
                }}
              >
                {columns.map((col) => {
                  const cellClass = ["preview-table-cell", col.className].filter(Boolean).join(" ");
                  return (
                    <span
                      key={col.key}
                      className={cellClass}
                      style={col.align ? { justifySelf: col.align } : undefined}
                    >
                      {col.render(row, rowIndex)}
                    </span>
                  );
                })}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
