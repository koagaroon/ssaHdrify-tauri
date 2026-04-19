import { useState, useCallback, useEffect, useRef } from "react";
import { pickSubtitleFiles, readText, writeText, fileNameFromPath } from "../../lib/tauri-api";
import { processAssContent, parseAssColor, formatAssColor } from "./ass-processor";
import {
  preprocessSrtColors,
  buildAssDocument,
  isNativeAss,
  isConvertible,
  DEFAULT_STYLE,
  type StyleConfig,
} from "./srt-converter";
import { resolveOutputPath, OUTPUT_PRESETS, DEFAULT_TEMPLATE } from "./output-naming";
import { DEFAULT_BRIGHTNESS, MIN_BRIGHTNESS, MAX_BRIGHTNESS, type Eotf } from "./color-engine";

import { parseSubtitle } from "../../lib/subtitle-parser";
import NumberInput from "../../lib/NumberInput";
import NitViz from "./NitViz";
import { useI18n } from "../../i18n/useI18n";
import { useFileContext } from "../../lib/FileContext";

/** Convert ASS color "&H00BBGGRR" to HTML "#RRGGBB" */
function assColorToHex(assColor: string): string {
  const { r, g, b } = parseAssColor(assColor);
  const hex = (n: number) => n.toString(16).padStart(2, "0");
  return `#${hex(r)}${hex(g)}${hex(b)}`;
}

/** Convert HTML "#RRGGBB" to ASS color "&H00BBGGRR" */
function hexToAssColor(htmlHex: string): string {
  const hex = htmlHex.slice(1);
  const r = parseInt(hex.slice(0, 2), 16);
  const g = parseInt(hex.slice(2, 4), 16);
  const b = parseInt(hex.slice(4, 6), 16);
  return formatAssColor(r, g, b, "00");
}

// Common fonts available on most systems (cross-platform)
const COMMON_FONTS = [
  "Arial",
  "Arial Black",
  "Calibri",
  "Cambria",
  "Comic Sans MS",
  "Consolas",
  "Courier New",
  "Georgia",
  "Impact",
  "Lucida Console",
  "Microsoft YaHei",
  "PingFang SC",
  "Segoe UI",
  "Tahoma",
  "Times New Roman",
  "Trebuchet MS",
  "Verdana",
  "Noto Sans",
  "Noto Sans CJK SC",
  "Noto Serif",
  "Source Han Sans",
  "Source Han Serif",
];

interface LogEntry {
  id: number;
  text: string;
  type: "info" | "error" | "success";
}

export default function HdrConvert() {
  const { t } = useI18n();
  const { hdrFiles, setHdrFiles, clearFile, filterAvailablePaths } = useFileContext();
  const [eotf, setEotf] = useState<Eotf>("PQ");
  const [brightness, setBrightness] = useState(DEFAULT_BRIGHTNESS);
  const [brightnessText, setBrightnessText] = useState(String(DEFAULT_BRIGHTNESS));
  const [template, setTemplate] = useState<string>(DEFAULT_TEMPLATE);
  const [customTemplate, setCustomTemplate] = useState("");
  const [showStylePanel, setShowStylePanel] = useState(false);
  const [style, setStyle] = useState<StyleConfig>({ ...DEFAULT_STYLE });
  const [processing, setProcessing] = useState(false);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [showFileList, setShowFileList] = useState(false);
  const logIdRef = useRef(0);
  const cancelRef = useRef(false);
  const logEndRef = useRef<HTMLDivElement>(null);
  const fileContainerRef = useRef<HTMLDivElement>(null);

  // File-list dropdown: close on click outside or Escape
  useEffect(() => {
    if (!showFileList) return;
    const onClick = (e: MouseEvent) => {
      if (fileContainerRef.current && !fileContainerRef.current.contains(e.target as Node)) {
        setShowFileList(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setShowFileList(false);
    };
    const id = setTimeout(() => document.addEventListener("mousedown", onClick), 0);
    document.addEventListener("keydown", onKey);
    return () => {
      clearTimeout(id);
      document.removeEventListener("mousedown", onClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [showFileList]);

  const addLog = useCallback((text: string, type: LogEntry["type"] = "info") => {
    const id = logIdRef.current++;
    setLogs((prev) => {
      const next = [...prev, { id, text, type }];
      // Trim to 200 entries max
      return next.length > 200 ? next.slice(-200) : next;
    });
    // Auto-scroll
    setTimeout(() => logEndRef.current?.scrollIntoView({ behavior: "smooth" }), 50);
  }, []);

  const handleBrightnessChange = (value: string) => {
    setBrightnessText(value);
    const num = parseInt(value, 10);
    if (!Number.isNaN(num) && num >= MIN_BRIGHTNESS && num <= MAX_BRIGHTNESS) {
      setBrightness(num);
    }
  };

  // Slider / preset paths emit a validated number — keep both state slots in sync.
  const handleBrightnessFromNits = useCallback((nits: number) => {
    setBrightness(nits);
    setBrightnessText(String(nits));
  }, []);

  const activeTemplate = template === "custom" ? customTemplate : template;
  const convertDisabled = !hdrFiles || processing;

  // ── File selection (separate from conversion) ──────────
  const handleSelectFiles = useCallback(async () => {
    const paths = await pickSubtitleFiles();
    if (!paths || paths.length === 0) return;

    // Cross-tab duplicate guard: skip files already loaded in other tabs
    const { allowed, skippedCount } = filterAvailablePaths(paths, "hdr");
    if (skippedCount > 0) {
      addLog(t("msg_files_skipped_in_use", skippedCount), "error");
    }
    if (allowed.length === 0) return;

    // Silent replace: see FileContext.tsx for design rationale
    const names = allowed.map(fileNameFromPath);
    setHdrFiles({ filePaths: allowed, fileNames: names });
  }, [filterAvailablePaths, setHdrFiles, addLog, t]);

  // ── Conversion (uses already-selected files) ───────────
  const handleConvert = useCallback(async () => {
    if (!hdrFiles) return;

    // Validate brightness
    if (brightness < MIN_BRIGHTNESS || brightness > MAX_BRIGHTNESS) {
      addLog(t("msg_invalid_brightness", MIN_BRIGHTNESS, MAX_BRIGHTNESS), "error");
      return;
    }

    const paths = hdrFiles.filePaths;
    setProcessing(true);
    cancelRef.current = false;

    try {
      addLog(t("msg_start_conversion", paths.length, eotf, brightness));

      const outputPaths = new Set<string>();
      let successCount = 0;

      for (const filePath of paths) {
        if (cancelRef.current) {
          addLog(t("msg_cancelled"), "info");
          break;
        }

        const fileName = fileNameFromPath(filePath);
        addLog(t("msg_processing", fileName));

        try {
          // Resolve output path
          let outputPath: string;
          try {
            outputPath = resolveOutputPath(filePath, activeTemplate, eotf);
          } catch (e) {
            addLog(t("msg_skipped", fileName, e instanceof Error ? e.message : String(e)), "error");
            continue;
          }

          // Check file extension before reading — skip unsupported formats early
          if (!isNativeAss(fileName) && !isConvertible(fileName)) {
            addLog(t("msg_unsupported", fileName), "error");
            continue;
          }

          // Check for duplicate output targets
          const normalizedOut = outputPath.replace(/\\/g, "/").toLowerCase();
          if (outputPaths.has(normalizedOut)) {
            addLog(t("msg_skipped_duplicate", fileName), "error");
            continue;
          }
          outputPaths.add(normalizedOut);

          // Read input file
          let content: string;
          try {
            content = await readText(filePath);
          } catch (e) {
            addLog(
              t("msg_read_error", fileName, e instanceof Error ? e.message : String(e)),
              "error"
            );
            continue;
          }

          // Check cancel after I/O
          if (cancelRef.current) break;

          let assContent: string;

          if (isNativeAss(fileName)) {
            // Direct ASS processing
            assContent = processAssContent(content, brightness, eotf);
          } else if (isConvertible(fileName)) {
            // SRT/SUB → ASS conversion path
            // Strip all curly-brace blocks from raw SRT before color preprocessing.
            // SRT has no curly-brace syntax, so any {...} is either a leaked ASS
            // override tag or content that would be misinterpreted by ASS renderers.
            const sanitized = content.replace(/\{[^}]*\}/g, "");
            // Preprocess SRT colors
            const preprocessed = preprocessSrtColors(sanitized);

            // Parse with our browser-compatible parser
            const { captions } = parseSubtitle(preprocessed, style.fps);

            // Build ASS document from parsed captions
            const entries = captions.map((c) => ({
              start: c.start,
              end: c.end,
              text: c.text,
            }));
            const rawAss = buildAssDocument(entries, style);

            // Now transform the ASS colors to HDR
            assContent = processAssContent(rawAss, brightness, eotf);
          } else {
            // Unreachable — extension was validated above, but satisfies TypeScript
            continue;
          }

          // Check cancel before writing
          if (cancelRef.current) break;

          // Write output
          await writeText(outputPath, assContent);
          const outName = fileNameFromPath(outputPath);
          addLog(t("msg_done", outName), "success");
          successCount++;
        } catch (e) {
          addLog(
            t("msg_convert_error", fileName, e instanceof Error ? e.message : String(e)),
            "error"
          );
        }
      }

      if (!cancelRef.current) {
        addLog(t("msg_complete", successCount, paths.length), "success");
      }
    } finally {
      setProcessing(false);
    }
  }, [hdrFiles, brightness, eotf, activeTemplate, style, addLog, t]);

  const handleClearFiles = useCallback(() => {
    clearFile("hdr");
  }, [clearFile]);

  return (
    <div className="space-y-4">
      {/* ── File strip — always visible; filename + clear + Select button ──
           When >1 file is selected, the filename area becomes a clickable
           dropdown showing all selected files (max ~5 rows, scroll beyond). */}
      <div className="flex items-center gap-2">
        <div
          ref={fileContainerRef}
          className="flex-1 min-w-0"
          style={{ position: "relative" }}
        >
          {hdrFiles && hdrFiles.filePaths.length > 1 ? (
            <button
              type="button"
              onClick={() => setShowFileList((v) => !v)}
              className="w-full flex items-center gap-2 px-3 rounded-lg text-sm"
              style={{
                background: "var(--bg-panel)",
                border: "1px solid var(--border-light)",
                minHeight: "38px",
                color: "var(--text-primary)",
                textAlign: "left",
                cursor: "pointer",
              }}
              aria-expanded={showFileList}
              aria-haspopup="listbox"
            >
              <span className="truncate flex-1">{hdrFiles.fileNames.join(", ")}</span>
              <span className="flex-none text-xs" style={{ color: "var(--text-muted)" }}>
                ({hdrFiles.filePaths.length})
              </span>
              <span className="flex-none text-xs" style={{ color: "var(--text-muted)" }}>
                {showFileList ? "▲" : "▼"}
              </span>
            </button>
          ) : (
            <div
              className="flex items-center gap-2 px-3 rounded-lg text-sm"
              style={{
                background: hdrFiles ? "var(--bg-panel)" : "var(--bg-input)",
                border: "1px solid var(--border-light)",
                minHeight: "38px",
              }}
            >
              {hdrFiles ? (
                <span className="truncate flex-1" style={{ color: "var(--text-primary)" }}>
                  {hdrFiles.fileNames[0]}
                </span>
              ) : (
                <span className="italic" style={{ color: "var(--text-muted)" }}>
                  {t("file_empty")}
                </span>
              )}
            </div>
          )}

          {showFileList && hdrFiles && hdrFiles.filePaths.length > 1 && (
            <div
              className="absolute rounded-lg overflow-hidden flex flex-col"
              style={{
                top: "100%",
                left: 0,
                right: 0,
                marginTop: "4px",
                background: "var(--bg-panel)",
                border: "1px solid var(--border)",
                boxShadow: "var(--shadow-popover)",
                maxHeight: "190px",
                zIndex: 20,
              }}
              role="listbox"
            >
              <div
                className="px-3 py-2 flex-none"
                style={{ borderBottom: "1px solid var(--border)" }}
              >
                <span className="text-xs font-medium" style={{ color: "var(--text-muted)" }}>
                  {t("hdr_files_title", hdrFiles.filePaths.length)}
                </span>
              </div>
              <div className="overflow-y-auto flex-1">
                {hdrFiles.fileNames.map((name, idx) => (
                  <div
                    key={idx}
                    className="px-3 py-2 text-sm truncate"
                    style={{
                      color: "var(--text-primary)",
                      borderBottom:
                        idx < hdrFiles.fileNames.length - 1
                          ? "1px solid color-mix(in srgb, var(--border) 50%, transparent)"
                          : "none",
                    }}
                    title={hdrFiles.filePaths[idx]}
                  >
                    {name}
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
        {hdrFiles && (
          <button
            onClick={handleClearFiles}
            disabled={processing}
            className="flex-none px-3 rounded-lg text-lg font-bold transition-colors"
            style={{
              background: "var(--cancel-bg)",
              color: "var(--cancel-text)",
              opacity: processing ? 0.4 : 1,
              height: "38px",
            }}
            title={t("btn_clear_file")}
          >
            ✕
          </button>
        )}
        <button
          onClick={handleSelectFiles}
          disabled={processing}
          className="flex-none px-5 rounded-lg font-medium text-sm transition-colors"
          style={{
            background: processing ? "var(--bg-input)" : "var(--accent)",
            color: processing ? "var(--text-muted)" : "white",
            height: "38px",
          }}
        >
          {t("btn_select_files")}
        </button>
      </div>

      {/* ── Controls: EOTF + Brightness ─────────────── */}
      <div className="flex items-start gap-6">
        <div className="space-y-1">
          <label
            htmlFor="hdr-eotf-select"
            className="block text-sm font-medium"
            style={{ color: "var(--text-secondary)" }}
          >
            {t("eotf_label")}
          </label>
          <select
            id="hdr-eotf-select"
            name="eotf"
            value={eotf}
            onChange={(e) => setEotf(e.target.value as Eotf)}
            disabled={processing}
            className="px-3 py-2 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
            style={{
              background: "var(--bg-input)",
              border: "1px solid var(--border)",
              color: "var(--text-primary)",
              minWidth: "260px",
            }}
          >
            <option value="PQ">{t("eotf_pq")}</option>
            <option value="HLG">{t("eotf_hlg")}</option>
          </select>
          <p className="text-xs" style={{ color: "var(--text-muted)" }}>
            {eotf === "PQ" ? t("eotf_pq_desc") : t("eotf_hlg_desc")}
          </p>
        </div>

        <div className="space-y-1">
          <label
            htmlFor="hdr-brightness-input"
            className="block text-sm font-medium"
            style={{ color: "var(--text-secondary)" }}
          >
            {t("brightness_label")}
          </label>
          <NumberInput
            id="hdr-brightness-input"
            value={brightnessText}
            onChange={handleBrightnessChange}
            min={MIN_BRIGHTNESS}
            max={MAX_BRIGHTNESS}
            disabled={processing}
            className="w-36"
          />
          <p className="text-xs" style={{ color: "var(--text-muted)" }}>
            {eotf === "PQ" ? t("brightness_hint_pq") : t("brightness_hint_hlg")}
          </p>
        </div>
      </div>

      {/* ── Brightness visualization (rainbow slider + standard presets) ── */}
      <NitViz value={brightness} onChange={handleBrightnessFromNits} disabled={processing} />

      {/* Output Template */}
      <div className="space-y-2">
        <label
          htmlFor="hdr-template-select"
          className="block text-sm font-medium"
          style={{ color: "var(--text-secondary)" }}
        >
          {t("template_label")}
        </label>
        <div className="flex flex-col gap-2">
          <select
            id="hdr-template-select"
            name="output-template"
            value={template}
            onChange={(e) => setTemplate(e.target.value)}
            disabled={processing}
            className="w-64 px-3 py-2 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
            style={{
              background: "var(--bg-input)",
              border: "1px solid var(--border)",
              color: "var(--text-primary)",
            }}
          >
            {OUTPUT_PRESETS.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
            <option value="custom">{t("template_custom")}</option>
          </select>
          {template === "custom" && (
            <input
              type="text"
              value={customTemplate}
              onChange={(e) => setCustomTemplate(e.target.value)}
              placeholder="{name}.hdr.{eotf}.ass"
              maxLength={200}
              disabled={processing}
              className="w-64 px-3 py-2 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
              style={{
                background: "var(--bg-input)",
                border: "1px solid var(--border)",
                color: "var(--text-primary)",
              }}
            />
          )}
        </div>
      </div>

      {/* Collapsible Style Settings */}
      <div className="rounded-lg" style={{ border: "1px solid var(--border)" }}>
        <button
          onClick={() => setShowStylePanel(!showStylePanel)}
          className="w-full flex items-center gap-2 px-4 py-3 text-sm font-medium rounded-lg transition-colors"
          style={{ color: "var(--text-secondary)" }}
        >
          <span className="text-xs">{showStylePanel ? "▼" : "▶"}</span>
          {t("style_settings")}
          <span className="text-xs ml-2" style={{ color: "var(--text-muted)" }}>
            {t("style_hint")}
          </span>
        </button>
        {showStylePanel && (
          <div className="px-4 pb-4 grid grid-cols-2 gap-3">
            <div>
              <label className="block text-xs mb-1" style={{ color: "var(--text-muted)" }}>
                {t("style_font")}
              </label>
              <select
                value={COMMON_FONTS.includes(style.fontName) ? style.fontName : "__custom"}
                onChange={(e) => {
                  if (e.target.value !== "__custom") {
                    setStyle({ ...style, fontName: e.target.value });
                  }
                }}
                disabled={processing}
                className="w-full px-2 py-1.5 rounded text-sm"
                style={{
                  background: "var(--bg-input)",
                  border: "1px solid var(--border)",
                  color: "var(--text-primary)",
                }}
              >
                {COMMON_FONTS.map((f) => (
                  <option key={f} value={f}>
                    {f}
                  </option>
                ))}
                <option value="__custom">{t("style_font_custom")}</option>
              </select>
              {!COMMON_FONTS.includes(style.fontName) && (
                <input
                  type="text"
                  value={style.fontName}
                  onChange={(e) => setStyle({ ...style, fontName: e.target.value })}
                  placeholder="Font family name"
                  disabled={processing}
                  className="w-full mt-1.5 px-2 py-1.5 rounded text-sm"
                  style={{
                    background: "var(--bg-input)",
                    border: "1px solid var(--border)",
                    color: "var(--text-primary)",
                  }}
                />
              )}
            </div>
            <div>
              <label className="block text-xs mb-1" style={{ color: "var(--text-muted)" }}>
                {t("style_size")}
              </label>
              <NumberInput
                value={style.fontSize}
                onChange={(v) => {
                  const n = parseInt(v);
                  setStyle({ ...style, fontSize: Number.isNaN(n) ? 48 : n });
                }}
                min={1}
                max={200}
                disabled={processing}
              />
            </div>
            <div>
              <label className="block text-xs mb-1" style={{ color: "var(--text-muted)" }}>
                {t("style_primary_color")}
              </label>
              <input
                type="color"
                value={assColorToHex(style.primaryColor)}
                onChange={(e) =>
                  setStyle({ ...style, primaryColor: hexToAssColor(e.target.value) })
                }
                disabled={processing}
                className="w-full h-8 rounded cursor-pointer"
                style={{
                  background: "var(--bg-input)",
                  border: "1px solid var(--border)",
                }}
              />
            </div>
            <div>
              <label className="block text-xs mb-1" style={{ color: "var(--text-muted)" }}>
                {t("style_outline_color")}
              </label>
              <input
                type="color"
                value={assColorToHex(style.outlineColor)}
                onChange={(e) =>
                  setStyle({ ...style, outlineColor: hexToAssColor(e.target.value) })
                }
                disabled={processing}
                className="w-full h-8 rounded cursor-pointer"
                style={{
                  background: "var(--bg-input)",
                  border: "1px solid var(--border)",
                }}
              />
            </div>
            <div>
              <label className="block text-xs mb-1" style={{ color: "var(--text-muted)" }}>
                {t("style_outline_width")}
              </label>
              <NumberInput
                value={style.outlineWidth}
                onChange={(v) => {
                  const n = parseFloat(v);
                  setStyle({ ...style, outlineWidth: Number.isNaN(n) ? 2 : n });
                }}
                min={0}
                max={20}
                step="0.5"
                disabled={processing}
              />
            </div>
            <div>
              <label className="block text-xs mb-1" style={{ color: "var(--text-muted)" }}>
                {t("style_shadow_depth")}
              </label>
              <NumberInput
                value={style.shadowDepth}
                onChange={(v) => {
                  const n = parseFloat(v);
                  setStyle({ ...style, shadowDepth: Number.isNaN(n) ? 1 : n });
                }}
                min={0}
                max={20}
                step="0.5"
                disabled={processing}
              />
            </div>
            <div>
              <label className="block text-xs mb-1" style={{ color: "var(--text-muted)" }}>
                {t("style_fps")}
              </label>
              <NumberInput
                value={style.fps}
                onChange={(v) => {
                  const n = parseFloat(v);
                  setStyle({ ...style, fps: Number.isNaN(n) ? 23.976 : n });
                }}
                min={1}
                max={120}
                step="0.001"
                disabled={processing}
              />
            </div>
          </div>
        )}
      </div>

      {/* ── Action row: Convert (+ Cancel while processing) ── */}
      <div className="flex items-center justify-end gap-2">
        {processing && (
          <button
            onClick={() => {
              cancelRef.current = true;
            }}
            className="px-4 rounded-lg text-sm transition-colors"
            style={{
              background: "var(--cancel-bg)",
              color: "var(--cancel-text)",
              height: "38px",
            }}
          >
            {t("btn_cancel")}
          </button>
        )}
        <button
          onClick={handleConvert}
          disabled={convertDisabled}
          className="px-6 rounded-lg font-medium text-sm transition-colors"
          style={{
            background: convertDisabled ? "var(--bg-input)" : "var(--accent)",
            color: convertDisabled ? "var(--text-muted)" : "white",
            opacity: !hdrFiles ? 0.5 : 1,
            height: "38px",
            minWidth: "140px",
          }}
        >
          {processing ? t("btn_converting") : t("btn_convert")}
        </button>
      </div>

      {/* Log Output */}
      {logs.length > 0 && (
        <div
          className="rounded-lg"
          style={{ border: "1px solid var(--border)", background: "var(--bg-panel)" }}
        >
          <div
            className="flex items-center justify-between px-3 py-2"
            style={{ borderBottom: "1px solid var(--border)" }}
          >
            <span className="text-xs font-medium" style={{ color: "var(--text-muted)" }}>
              {t("log_title")}
            </span>
            <button
              onClick={() => setLogs([])}
              className="text-xs"
              style={{ color: "var(--text-muted)" }}
            >
              {t("log_clear")}
            </button>
          </div>
          <div className="max-h-48 overflow-y-auto p-3 font-mono text-xs space-y-0.5">
            {logs.map((log) => (
              <div
                key={log.id}
                style={{
                  color: {
                    error: "var(--error)",
                    success: "var(--success)",
                    info: "var(--text-muted)",
                  }[log.type],
                }}
              >
                {log.text}
              </div>
            ))}
            <div ref={logEndRef} />
          </div>
        </div>
      )}
    </div>
  );
}
