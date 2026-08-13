import { ASCII_CONTROL_CHARS, hasUnicodeControls } from "../../lib/unicode-controls";
import { parseFiniteNumberText } from "../../lib/strict-number";

export type StyleSectionKind = "ass-v4-plus" | "ssa-v4";
export type StyleEditChangeKind = "fontFamily" | "fontSize";

export interface FontFamilyEditOperation {
  enabled: boolean;
  targetFamily: string;
  /** When omitted, every style row is eligible for the family change. */
  sourceFamily?: string | undefined;
}

export interface FontSizeEditOperation {
  enabled: boolean;
  targetSize: number;
}

export interface StyleEditOperations {
  fontFamily?: FontFamilyEditOperation | undefined;
  fontSize?: FontSizeEditOperation | undefined;
}

export interface StyleEditRow {
  id: string;
  lineNumber: number;
  section: StyleSectionKind;
  styleName: string;
  fontFamilyBefore: string | null;
  fontFamilyAfter: string | null;
  fontSizeBefore: string | null;
  fontSizeAfter: string | null;
  changes: StyleEditChangeKind[];
  willChange: boolean;
}

export interface StyleEditPlan {
  rows: StyleEditRow[];
  changeableRowIds: string[];
  styleCount: number;
  changedStyleCount: number;
  unchangedStyleCount: number;
  changedFieldCount: number;
}

export interface StyleEditApplyResult {
  content: string;
  plan: StyleEditPlan;
  appliedRowIds: string[];
  changedStyleCount: number;
  changedFieldCount: number;
}

/** Structural metadata safe to use while ingesting a file before edits exist. */
export interface StyleDocumentInspection {
  styleCount: number;
}

export type StyleEditErrorCode =
  | "content-too-large"
  | "line-too-long"
  | "too-many-lines"
  | "too-many-styles"
  | "invalid-operation"
  | "missing-style-section"
  | "duplicate-style-section"
  | "missing-format"
  | "duplicate-format"
  | "invalid-format"
  | "style-before-format"
  | "invalid-style-row"
  | "unknown-row-selection";

export class StyleEditError extends Error {
  readonly code: StyleEditErrorCode;
  readonly lineNumber: number | undefined;

  constructor(code: StyleEditErrorCode, message: string, lineNumber?: number) {
    super(lineNumber === undefined ? message : `Line ${lineNumber}: ${message}`);
    this.name = "StyleEditError";
    this.code = code;
    this.lineNumber = lineNumber;
  }
}

const MAX_CONTENT_CHARS = 100_000_000;
const MAX_LINES = 501_024;
const MAX_LINE_CHARS = 1_000_000;
const MAX_STYLE_FIELDS = 1_024;
const MAX_STYLE_ROWS = 50_000;
const MAX_FORMAT_COLUMN_CHARS = 128;
const MAX_STYLE_NAME_CHARS = 256;
const MAX_FONT_FAMILY_CHARS = 128;
const MAX_FONT_SIZE_FIELD_CHARS = 128;
const CONTROL_RE = new RegExp(`[${ASCII_CONTROL_CHARS}]`, "u");
// ASS/SSA record syntax only needs ordinary horizontal ASCII spacing here.
// JavaScript `\s` also accepts invisible Unicode separators such as U+FEFF,
// which could otherwise hide a structural record prefix from the preview.
const FORMAT_RECORD_RE = /^[ \t]*format[ \t]*:/i;
const STYLE_RECORD_RE = /^[ \t]*style[ \t]*:/i;

interface PhysicalLine {
  body: string;
  lineNumber: number;
  bodyStart: number;
  bodyEnd: number;
}

interface ParsedStyleRow {
  id: string;
  lineNumber: number;
  section: StyleSectionKind;
  styleName: string;
  fontFamilyBefore: string | null;
  fontSizeBefore: string | null;
  bodyStart: number;
  bodyEnd: number;
  prefix: string;
  fields: string[];
  fontFamilyIndex: number | null;
  fontSizeIndex: number | null;
}

interface ProjectedStyleRow {
  source: ParsedStyleRow;
  publicRow: StyleEditRow;
}

interface ParsedDocument {
  content: string;
  rows: ParsedStyleRow[];
  format: string[];
  formatLineNumber: number;
}

interface ActiveStyleSection {
  kind: StyleSectionKind;
  format: string[] | null;
  formatLineNumber: number | null;
  styleRowsSeen: number;
}

function splitPhysicalLines(content: string): PhysicalLine[] {
  if (content.length > MAX_CONTENT_CHARS) {
    throw new StyleEditError(
      "content-too-large",
      `File has ${content.length} characters (max ${MAX_CONTENT_CHARS})`
    );
  }

  const lines: PhysicalLine[] = [];
  let start = 0;
  let lineNumber = 1;

  while (start < content.length) {
    if (lines.length >= MAX_LINES) {
      throw new StyleEditError("too-many-lines", `File has more than ${MAX_LINES} lines`);
    }

    let cursor = start;
    while (cursor < content.length) {
      const code = content.charCodeAt(cursor);
      if (code === 10 || code === 13) break;
      cursor += 1;
    }

    const body = content.slice(start, cursor);
    if (body.length > MAX_LINE_CHARS) {
      throw new StyleEditError(
        "line-too-long",
        `Line has ${body.length} characters (max ${MAX_LINE_CHARS})`,
        lineNumber
      );
    }

    const bodyEnd = cursor;
    if (cursor < content.length) {
      if (content.charCodeAt(cursor) === 13 && content.charCodeAt(cursor + 1) === 10) {
        cursor += 2;
      } else {
        cursor += 1;
      }
    }

    lines.push({ body, lineNumber, bodyStart: start, bodyEnd });
    lineNumber += 1;
    start = cursor;
  }

  if (content.length === 0) {
    lines.push({ body: "", lineNumber: 1, bodyStart: 0, bodyEnd: 0 });
  } else if (content.endsWith("\n") || content.endsWith("\r")) {
    // A trailing terminator starts an empty final physical line. Retain that
    // conceptual line for exact cap accounting; serializing its empty body and
    // empty ending leaves the original bytes unchanged.
    if (lines.length >= MAX_LINES) {
      throw new StyleEditError("too-many-lines", `File has more than ${MAX_LINES} lines`);
    }
    lines.push({
      body: "",
      lineNumber,
      bodyStart: content.length,
      bodyEnd: content.length,
    });
  }
  return lines;
}

function semanticLine(line: PhysicalLine): string {
  return line.lineNumber === 1 && line.body.startsWith("\uFEFF") ? line.body.slice(1) : line.body;
}

function sectionKind(line: string): StyleSectionKind | "other" | null {
  const trimmed = line.trim();
  // Dialogue/style rows dominate large files. Avoid allocating a lowercased
  // copy for every ordinary line when only bracket-led lines can be sections.
  if (!trimmed.startsWith("[")) return null;
  const normalized = trimmed.toLowerCase();
  if (normalized === "[v4+ styles]") return "ass-v4-plus";
  if (normalized === "[v4 styles]") return "ssa-v4";
  // Any bracket-led line terminates the active style section. Unknown or
  // malformed section names are untrusted and need no retained/cased
  // representation. Requiring a closing bracket here would let a malformed
  // `[Events] trailing` line keep the parser in style mode and mutate later
  // unrelated `Style:` text.
  return "other";
}

function recordValue(line: string, recordName: "format" | "style"): string | null {
  const match = (recordName === "format" ? FORMAT_RECORD_RE : STYLE_RECORD_RE).exec(line);
  return match ? line.slice(match[0].length) : null;
}

function parseRecordFields(value: string, lineNumber: number, recordName: string): string[] {
  const fields = value.split(",");
  if (fields.length > MAX_STYLE_FIELDS) {
    throw new StyleEditError(
      recordName === "Format" ? "invalid-format" : "invalid-style-row",
      `${recordName} record has ${fields.length} fields (max ${MAX_STYLE_FIELDS})`,
      lineNumber
    );
  }
  return fields;
}

function normalizeColumnName(value: string): string {
  return value.trim().toLowerCase();
}

function validateFormat(value: string, lineNumber: number): string[] {
  const rawFields = parseRecordFields(value, lineNumber, "Format");
  const fields: string[] = [];
  const seen = new Set<string>();

  for (const rawField of rawFields) {
    if (CONTROL_RE.test(rawField) || hasUnicodeControls(rawField)) {
      throw new StyleEditError(
        "invalid-format",
        "Format record contains a column with control or invisible formatting characters",
        lineNumber
      );
    }
    const trimmed = rawField.trim();
    if (!trimmed) {
      throw new StyleEditError(
        "invalid-format",
        "Format record contains an empty column",
        lineNumber
      );
    }
    if (codePointLength(trimmed, MAX_FORMAT_COLUMN_CHARS) > MAX_FORMAT_COLUMN_CHARS) {
      throw new StyleEditError(
        "invalid-format",
        `Format column exceeds ${MAX_FORMAT_COLUMN_CHARS} characters`,
        lineNumber
      );
    }
    const field = normalizeColumnName(trimmed);
    if (seen.has(field)) {
      throw new StyleEditError(
        "invalid-format",
        "Format record contains duplicate column names",
        lineNumber
      );
    }
    seen.add(field);
    fields.push(field);
  }

  if (!seen.has("name")) {
    throw new StyleEditError(
      "invalid-format",
      "Format record is missing the Name column",
      lineNumber
    );
  }
  return fields;
}

function codePointLength(value: string, quickUtf16Limit: number): number {
  if (value.length > quickUtf16Limit * 2) return quickUtf16Limit + 1;
  return Array.from(value).length;
}

function assertSafeSemanticField(
  value: string,
  label: string,
  maxCodePoints: number,
  lineNumber?: number
): void {
  if (!value || value !== value.trim()) {
    throw new StyleEditError(
      lineNumber === undefined ? "invalid-operation" : "invalid-style-row",
      `${label} must be non-empty and have no leading or trailing whitespace`,
      lineNumber
    );
  }
  if (value.includes(",")) {
    throw new StyleEditError(
      lineNumber === undefined ? "invalid-operation" : "invalid-style-row",
      `${label} cannot contain a comma`,
      lineNumber
    );
  }
  if (CONTROL_RE.test(value) || hasUnicodeControls(value)) {
    throw new StyleEditError(
      lineNumber === undefined ? "invalid-operation" : "invalid-style-row",
      `${label} contains control or invisible formatting characters`,
      lineNumber
    );
  }
  if (codePointLength(value, maxCodePoints) > maxCodePoints) {
    throw new StyleEditError(
      lineNumber === undefined ? "invalid-operation" : "invalid-style-row",
      `${label} exceeds ${maxCodePoints} characters`,
      lineNumber
    );
  }
}

function validateOperations(operations: StyleEditOperations): void {
  if (!operations.fontFamily?.enabled && !operations.fontSize?.enabled) {
    throw new StyleEditError("invalid-operation", "Enable at least one style-edit operation");
  }
  if (operations.fontFamily?.enabled) {
    assertSafeSemanticField(
      operations.fontFamily.targetFamily,
      "Target font family",
      MAX_FONT_FAMILY_CHARS
    );
    if (operations.fontFamily.sourceFamily !== undefined) {
      assertSafeSemanticField(
        operations.fontFamily.sourceFamily,
        "Source font family",
        MAX_FONT_FAMILY_CHARS
      );
    }
  }

  if (operations.fontSize?.enabled) {
    const size = operations.fontSize.targetSize;
    if (!Number.isFinite(size) || size < 1 || size > 200) {
      throw new StyleEditError(
        "invalid-operation",
        "Target font size must be a finite number from 1 to 200"
      );
    }
  }
}

function assertSafeExistingFontSize(value: string, lineNumber: number): void {
  // Malformed or empty numeric text is deliberately repairable when the user
  // supplies a valid replacement. Only bound values that would be retained in
  // the preview or passed to Number(), and reject display-spoofing controls.
  if (value.length > MAX_FONT_SIZE_FIELD_CHARS) {
    throw new StyleEditError(
      "invalid-style-row",
      `Existing font size exceeds ${MAX_FONT_SIZE_FIELD_CHARS} characters`,
      lineNumber
    );
  }
  if (CONTROL_RE.test(value) || hasUnicodeControls(value)) {
    throw new StyleEditError(
      "invalid-style-row",
      "Existing font size contains control or invisible formatting characters",
      lineNumber
    );
  }
}

function assertRawStyleFieldSafe(value: string, label: string, lineNumber: number): void {
  // Check BEFORE trim: ASCII spaces are ordinary CSV padding, while a tab or
  // trailing U+FEFF must not disappear during semantic normalization and then
  // reach previews as apparently clean text.
  if (CONTROL_RE.test(value) || hasUnicodeControls(value)) {
    throw new StyleEditError(
      "invalid-style-row",
      `${label} contains control or invisible formatting characters`,
      lineNumber
    );
  }
}

function normalizeFamilyForMatch(value: string): string {
  return value.trim().normalize("NFC").toLowerCase();
}

function replaceFieldValue(raw: string, nextValue: string): string {
  const leadingLength = raw.length - raw.trimStart().length;
  const trailingLength = raw.length - raw.trimEnd().length;
  const contentEnd = Math.max(leadingLength, raw.length - trailingLength);
  return `${raw.slice(0, leadingLength)}${nextValue}${raw.slice(contentEnd)}`;
}

function formatTargetSize(size: number): string {
  return String(size);
}

function parseStyleRow(
  line: PhysicalLine,
  section: ActiveStyleSection,
  rowOrdinal: number
): ParsedStyleRow {
  const styleValue = recordValue(semanticLine(line), "style");
  if (styleValue === null) {
    throw new StyleEditError(
      "invalid-style-row",
      "Internal style-record mismatch",
      line.lineNumber
    );
  }
  if (!section.format) {
    throw new StyleEditError(
      "style-before-format",
      "Style record appears before the section's Format record",
      line.lineNumber
    );
  }

  const fields = parseRecordFields(styleValue, line.lineNumber, "Style");
  if (fields.length !== section.format.length) {
    throw new StyleEditError(
      "invalid-style-row",
      `Style record has ${fields.length} fields but Format declares ${section.format.length}`,
      line.lineNumber
    );
  }

  const nameIndex = section.format.indexOf("name");
  const fontFamilyIndexRaw = section.format.indexOf("fontname");
  const fontSizeIndexRaw = section.format.indexOf("fontsize");
  const fontFamilyIndex = fontFamilyIndexRaw < 0 ? null : fontFamilyIndexRaw;
  const fontSizeIndex = fontSizeIndexRaw < 0 ? null : fontSizeIndexRaw;
  assertRawStyleFieldSafe(fields[nameIndex]!, "Style name", line.lineNumber);
  const styleName = fields[nameIndex]!.trim();
  assertSafeSemanticField(styleName, "Style name", MAX_STYLE_NAME_CHARS, line.lineNumber);

  if (fontFamilyIndex !== null) {
    assertRawStyleFieldSafe(fields[fontFamilyIndex]!, "Existing font family", line.lineNumber);
  }
  const fontFamilyBefore = fontFamilyIndex === null ? null : fields[fontFamilyIndex]!.trim();
  if (fontFamilyBefore) {
    assertSafeSemanticField(
      fontFamilyBefore,
      "Existing font family",
      MAX_FONT_FAMILY_CHARS,
      line.lineNumber
    );
  }
  if (fontSizeIndex !== null) {
    assertRawStyleFieldSafe(fields[fontSizeIndex]!, "Existing font size", line.lineNumber);
  }
  const fontSizeBefore = fontSizeIndex === null ? null : fields[fontSizeIndex]!.trim();
  if (fontSizeBefore !== null) {
    assertSafeExistingFontSize(fontSizeBefore, line.lineNumber);
  }

  const prefixLength = semanticLine(line).length - styleValue.length;
  return {
    id: `style:${section.kind}:${rowOrdinal}:${line.lineNumber}`,
    lineNumber: line.lineNumber,
    section: section.kind,
    styleName,
    fontFamilyBefore,
    fontSizeBefore,
    bodyStart: line.bodyStart,
    bodyEnd: line.bodyEnd,
    prefix: semanticLine(line).slice(0, prefixLength),
    fields,
    fontFamilyIndex,
    fontSizeIndex,
  };
}

function projectRows(parsed: ParsedDocument, operations: StyleEditOperations): ProjectedStyleRow[] {
  validateOperations(operations);
  const format = parsed.format;
  if (operations.fontFamily?.enabled && !format.includes("fontname")) {
    throw new StyleEditError(
      "invalid-format",
      "Font-family editing requires a Fontname column",
      parsed.formatLineNumber
    );
  }
  if (operations.fontSize?.enabled && !format.includes("fontsize")) {
    throw new StyleEditError(
      "invalid-format",
      "Font-size editing requires a Fontsize column",
      parsed.formatLineNumber
    );
  }

  return parsed.rows.map((row) => {
    let fontFamilyAfter = row.fontFamilyBefore;
    let fontSizeAfter = row.fontSizeBefore;
    const changes: StyleEditChangeKind[] = [];
    const familyOperation = operations.fontFamily;
    if (familyOperation?.enabled && row.fontFamilyBefore !== null) {
      const matchesFilter =
        familyOperation.sourceFamily === undefined ||
        normalizeFamilyForMatch(row.fontFamilyBefore) ===
          normalizeFamilyForMatch(familyOperation.sourceFamily);
      if (matchesFilter && row.fontFamilyBefore !== familyOperation.targetFamily) {
        fontFamilyAfter = familyOperation.targetFamily;
        changes.push("fontFamily");
      }
    }

    const sizeOperation = operations.fontSize;
    if (sizeOperation?.enabled && row.fontSizeBefore !== null) {
      const beforeNumber = parseFiniteNumberText(row.fontSizeBefore);
      if (beforeNumber === null || beforeNumber !== sizeOperation.targetSize) {
        fontSizeAfter = formatTargetSize(sizeOperation.targetSize);
        changes.push("fontSize");
      }
    }

    return {
      source: row,
      publicRow: {
        id: row.id,
        lineNumber: row.lineNumber,
        section: row.section,
        styleName: row.styleName,
        fontFamilyBefore: row.fontFamilyBefore,
        fontFamilyAfter,
        fontSizeBefore: row.fontSizeBefore,
        fontSizeAfter,
        changes,
        willChange: changes.length > 0,
      },
    };
  });
}

function buildPlan(rows: ProjectedStyleRow[]): StyleEditPlan {
  const publicRows = rows.map((row) => row.publicRow);
  const changeable = publicRows.filter((row) => row.willChange);
  return {
    rows: publicRows,
    changeableRowIds: changeable.map((row) => row.id),
    styleCount: publicRows.length,
    changedStyleCount: changeable.length,
    unchangedStyleCount: publicRows.length - changeable.length,
    changedFieldCount: changeable.reduce((total, row) => total + row.changes.length, 0),
  };
}

function parseDocument(content: string): ParsedDocument {
  const lines = splitPhysicalLines(content);
  const rows: ParsedStyleRow[] = [];
  let activeStyleSection: ActiveStyleSection | null = null;
  let styleSectionFound = false;
  let documentFormat: string[] | null = null;
  let documentFormatLineNumber: number | null = null;

  const finalizeStyleSection = (lineNumber: number): void => {
    if (activeStyleSection && !activeStyleSection.format) {
      throw new StyleEditError("missing-format", "Style section has no Format record", lineNumber);
    }
    activeStyleSection = null;
  };

  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index]!;
    const semantic = semanticLine(line);
    const nextSection = sectionKind(semantic);

    if (nextSection !== null) {
      finalizeStyleSection(line.lineNumber);
      if (nextSection === "ass-v4-plus" || nextSection === "ssa-v4") {
        if (styleSectionFound) {
          throw new StyleEditError(
            "duplicate-style-section",
            "File contains more than one V4 style section",
            line.lineNumber
          );
        }
        styleSectionFound = true;
        activeStyleSection = {
          kind: nextSection,
          format: null,
          formatLineNumber: null,
          styleRowsSeen: 0,
        };
      }
      continue;
    }

    if (!activeStyleSection) continue;

    const formatValue = recordValue(semantic, "format");
    if (formatValue !== null) {
      if (activeStyleSection.format) {
        throw new StyleEditError(
          "duplicate-format",
          `Style section already has a Format record at line ${activeStyleSection.formatLineNumber}`,
          line.lineNumber
        );
      }
      activeStyleSection.format = validateFormat(formatValue, line.lineNumber);
      activeStyleSection.formatLineNumber = line.lineNumber;
      documentFormat = activeStyleSection.format;
      documentFormatLineNumber = line.lineNumber;
      continue;
    }

    if (recordValue(semantic, "style") !== null) {
      if (rows.length >= MAX_STYLE_ROWS) {
        throw new StyleEditError(
          "too-many-styles",
          `File contains more than ${MAX_STYLE_ROWS} Style records`,
          line.lineNumber
        );
      }
      activeStyleSection.styleRowsSeen += 1;
      rows.push(parseStyleRow(line, activeStyleSection, activeStyleSection.styleRowsSeen));
    }
  }

  finalizeStyleSection(lines.at(-1)!.lineNumber);
  if (!styleSectionFound) {
    throw new StyleEditError(
      "missing-style-section",
      "File has no [V4+ Styles] or [V4 Styles] section"
    );
  }

  return {
    content,
    rows,
    format: documentFormat!,
    formatLineNumber: documentFormatLineNumber!,
  };
}

export function planStyleEdit(content: string, operations: StyleEditOperations): StyleEditPlan {
  const parsed = parseDocument(content);
  return buildPlan(projectRows(parsed, operations));
}

/**
 * Validate an ASS/SSA style document before the user enables an edit.
 *
 * This routes through the same section, Format, Style-row, and resource-cap
 * parser as planning, but supplies no edit operations. Consequently a
 * structurally valid `Format: Name` document can be selected, while later
 * planning still fails if the enabled operation's Fontname/Fontsize column is
 * absent. Change projections remain private to the parser and are not exposed
 * as a trusted edit plan from this read-only boundary.
 */
export function inspectStyleDocument(content: string): StyleDocumentInspection {
  const parsed = parseDocument(content);
  return { styleCount: parsed.rows.length };
}

/**
 * Project edits from a document inspected once at file-ingestion time.
 * The returned closure is synchronous but only maps already-parsed style
 * rows, so changing a target does not rescan the full subtitle text.
 */
export function createStyleDocumentPlanner(content: string): {
  inspect: StyleDocumentInspection;
  plan: (operations: StyleEditOperations) => StyleEditPlan;
  apply: (
    operations: StyleEditOperations,
    selectedRowIds?: readonly string[] | ReadonlySet<string>
  ) => StyleEditApplyResult;
} {
  const parsed = parseDocument(content);
  return {
    inspect: { styleCount: parsed.rows.length },
    plan: (operations) => buildPlan(projectRows(parsed, operations)),
    apply: (operations, selectedRowIds) => applyParsedStyleEdit(parsed, operations, selectedRowIds),
  };
}

function selectedIdSet(
  selectedRowIds: readonly string[] | ReadonlySet<string> | undefined,
  plan: StyleEditPlan
): Set<string> {
  if (selectedRowIds === undefined) return new Set(plan.changeableRowIds);
  const selected = new Set(selectedRowIds);
  const valid = new Set(plan.rows.map((row) => row.id));
  for (const id of selected) {
    if (!valid.has(id)) {
      throw new StyleEditError(
        "unknown-row-selection",
        `Selected style row '${id}' does not exist in the current plan`
      );
    }
  }
  return selected;
}

export function applyStyleEdit(
  content: string,
  operations: StyleEditOperations,
  selectedRowIds?: readonly string[] | ReadonlySet<string>
): StyleEditApplyResult {
  return applyParsedStyleEdit(parseDocument(content), operations, selectedRowIds);
}

function applyParsedStyleEdit(
  parsed: ParsedDocument,
  operations: StyleEditOperations,
  selectedRowIds?: readonly string[] | ReadonlySet<string>
): StyleEditApplyResult {
  const projected = projectRows(parsed, operations);
  const plan = buildPlan(projected);
  const selected = selectedIdSet(selectedRowIds, plan);
  const replacements: Array<{ start: number; end: number; body: string }> = [];
  const appliedRowIds: string[] = [];
  let changedFieldCount = 0;

  for (const row of projected) {
    if (!row.publicRow.willChange || !selected.has(row.publicRow.id)) continue;
    const fields = row.source.fields.slice();
    if (row.publicRow.changes.includes("fontFamily") && row.source.fontFamilyIndex !== null) {
      fields[row.source.fontFamilyIndex] = replaceFieldValue(
        fields[row.source.fontFamilyIndex]!,
        row.publicRow.fontFamilyAfter!
      );
      changedFieldCount += 1;
    }
    if (row.publicRow.changes.includes("fontSize") && row.source.fontSizeIndex !== null) {
      fields[row.source.fontSizeIndex] = replaceFieldValue(
        fields[row.source.fontSizeIndex]!,
        row.publicRow.fontSizeAfter!
      );
      changedFieldCount += 1;
    }
    replacements.push({
      start: row.source.bodyStart,
      end: row.source.bodyEnd,
      body: `${row.source.prefix}${fields.join(",")}`,
    });
    appliedRowIds.push(row.publicRow.id);
  }

  let output = "";
  let cursor = 0;
  for (const replacement of replacements) {
    output += parsed.content.slice(cursor, replacement.start);
    output += replacement.body;
    cursor = replacement.end;
  }
  output += parsed.content.slice(cursor);
  return {
    content: output,
    plan,
    appliedRowIds,
    changedStyleCount: appliedRowIds.length,
    changedFieldCount,
  };
}
