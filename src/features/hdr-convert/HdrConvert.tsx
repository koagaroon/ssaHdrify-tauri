import { useState, useCallback, useRef } from "react";
import {
  pickSubtitleFiles,
  readText,
  writeText,
} from "../../lib/tauri-api";
import {
  processAssContent,
} from "./ass-processor";
import {
  preprocessSrtColors,
  buildAssDocument,
  isNativeAss,
  isConvertible,
  DEFAULT_STYLE,
  type StyleConfig,
} from "./srt-converter";
import {
  resolveOutputPath,
  OUTPUT_PRESETS,
  DEFAULT_TEMPLATE,
} from "./output-naming";
import {
  DEFAULT_BRIGHTNESS,
  MIN_BRIGHTNESS,
  MAX_BRIGHTNESS,
  type Eotf,
} from "./color-engine";

import { parseSubtitle } from "../../lib/subtitle-parser";
import NumberInput from "../../lib/NumberInput";
import { useI18n } from "../../i18n/useI18n";

// Common fonts available on most systems (cross-platform)
const COMMON_FONTS = [
  "Arial", "Arial Black", "Calibri", "Cambria", "Comic Sans MS",
  "Consolas", "Courier New", "Georgia", "Impact", "Lucida Console",
  "Microsoft YaHei", "PingFang SC", "Segoe UI", "Tahoma",
  "Times New Roman", "Trebuchet MS", "Verdana",
  "Noto Sans", "Noto Sans CJK SC", "Noto Serif",
  "Source Han Sans", "Source Han Serif",
];

interface LogEntry {
  id: number;
  text: string;
  type: "info" | "error" | "success";
}

export default function HdrConvert() {
  const { t } = useI18n();
  const [eotf, setEotf] = useState<Eotf>("PQ");
  const [brightness, setBrightness] = useState(DEFAULT_BRIGHTNESS);
  const [brightnessText, setBrightnessText] = useState(String(DEFAULT_BRIGHTNESS));
  const [template, setTemplate] = useState<string>(DEFAULT_TEMPLATE);
  const [customTemplate, setCustomTemplate] = useState("");
  const [showStylePanel, setShowStylePanel] = useState(false);
  const [style, setStyle] = useState<StyleConfig>({ ...DEFAULT_STYLE });
  const [processing, setProcessing] = useState(false);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const logIdRef = useRef(0);
  const cancelRef = useRef(false);
  const logEndRef = useRef<HTMLDivElement>(null);

  const addLog = useCallback(
    (text: string, type: LogEntry["type"] = "info") => {
      const id = logIdRef.current++;
      setLogs((prev) => {
        const next = [...prev, { id, text, type }];
        // Trim to 200 entries max
        return next.length > 200 ? next.slice(-200) : next;
      });
      // Auto-scroll
      setTimeout(() => logEndRef.current?.scrollIntoView({ behavior: "smooth" }), 50);
    },
    []
  );

  const handleBrightnessChange = (value: string) => {
    setBrightnessText(value);
    const num = parseInt(value, 10);
    if (!Number.isNaN(num) && num >= MIN_BRIGHTNESS && num <= MAX_BRIGHTNESS) {
      setBrightness(num);
    }
  };

  const activeTemplate = template === "custom" ? customTemplate : template;

  const handleConvert = useCallback(async () => {
    // Validate brightness
    if (brightness < MIN_BRIGHTNESS || brightness > MAX_BRIGHTNESS) {
      addLog(t("msg_invalid_brightness", MIN_BRIGHTNESS, MAX_BRIGHTNESS), "error");
      return;
    }

    const paths = await pickSubtitleFiles();
    if (!paths || paths.length === 0) return;

    setProcessing(true);
    cancelRef.current = false;
    addLog(t("msg_start_conversion", paths.length, eotf, brightness));

    const outputPaths = new Set<string>();
    let successCount = 0;

    for (const filePath of paths) {
      if (cancelRef.current) {
        addLog(t("msg_cancelled"), "info");
        break;
      }

      const fileName = filePath.replace(/\\/g, "/").split("/").pop() ?? filePath;
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
          addLog(t("msg_read_error", fileName, e instanceof Error ? e.message : String(e)), "error");
          continue;
        }

        let assContent: string;

        if (isNativeAss(fileName)) {
          // Direct ASS processing
          assContent = processAssContent(content, brightness, eotf);
        } else if (isConvertible(fileName)) {
          // SRT/SUB → ASS conversion path
          // Preprocess SRT colors
          const preprocessed = preprocessSrtColors(content);

          // Parse with our browser-compatible parser
          const { captions } = parseSubtitle(preprocessed);

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
          addLog(t("msg_unsupported", fileName), "error");
          continue;
        }

        // Write output
        await writeText(outputPath, assContent);
        const outName = outputPath.replace(/\\/g, "/").split("/").pop() ?? outputPath;
        addLog(t("msg_done", outName), "success");
        successCount++;
      } catch (e) {
        addLog(t("msg_convert_error", fileName, e instanceof Error ? e.message : String(e)), "error");
      }
    }

    addLog(t("msg_complete", successCount, paths.length), "success");
    setProcessing(false);
  }, [brightness, eotf, activeTemplate, style, addLog, t]);

  return (
    <div className="max-w-2xl space-y-5">
      {/* EOTF Selection */}
      <div className="space-y-2">
        <label
          className="block text-sm font-medium"
          style={{ color: "var(--text-secondary)" }}
        >
          {t("eotf_label")}
        </label>
        <select
          value={eotf}
          onChange={(e) => setEotf(e.target.value as Eotf)}
          disabled={processing}
          className="w-48 px-3 py-2 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
          style={{
            background: "var(--bg-input)",
            border: "1px solid var(--border)",
            color: "var(--text-primary)",
          }}
        >
          <option value="PQ">PQ (Perceptual Quantizer)</option>
          <option value="HLG">HLG (Hybrid Log-Gamma)</option>
        </select>
        <p className="text-xs" style={{ color: "var(--text-muted)" }}>
          {eotf === "PQ" ? t("eotf_pq_desc") : t("eotf_hlg_desc")}
        </p>
      </div>

      {/* Brightness */}
      <div className="space-y-2">
        <label
          className="block text-sm font-medium"
          style={{ color: "var(--text-secondary)" }}
        >
          {t("brightness_label")}
        </label>
        <NumberInput
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

      {/* Output Template */}
      <div className="space-y-2">
        <label
          className="block text-sm font-medium"
          style={{ color: "var(--text-secondary)" }}
        >
          {t("template_label")}
        </label>
        <div className="flex flex-col gap-2">
          <select
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
                  <option key={f} value={f}>{f}</option>
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
                onChange={(v) => setStyle({ ...style, fontSize: parseInt(v) || 48 })}
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
                value="#ffffff"
                onChange={(e) => {
                  const hex = e.target.value.slice(1);
                  const r = hex.slice(0, 2);
                  const g = hex.slice(2, 4);
                  const b = hex.slice(4, 6);
                  setStyle({ ...style, primaryColor: `&H00${b}${g}${r}` });
                }}
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
                value="#000000"
                onChange={(e) => {
                  const hex = e.target.value.slice(1);
                  const r = hex.slice(0, 2);
                  const g = hex.slice(2, 4);
                  const b = hex.slice(4, 6);
                  setStyle({ ...style, outlineColor: `&H00${b}${g}${r}` });
                }}
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
                onChange={(v) => setStyle({ ...style, outlineWidth: parseFloat(v) || 2 })}
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
                onChange={(v) => setStyle({ ...style, shadowDepth: parseFloat(v) || 1 })}
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
                onChange={(v) => setStyle({ ...style, fps: parseFloat(v) || 23.976 })}
                min={1}
                max={120}
                step="0.001"
                disabled={processing}
              />
            </div>
          </div>
        )}
      </div>

      {/* Action Button */}
      <div className="flex gap-3">
        <button
          onClick={handleConvert}
          disabled={processing}
          className="px-6 py-2.5 rounded-lg font-medium text-sm transition-colors"
          style={{
            background: processing ? "var(--bg-input)" : "var(--accent)",
            color: processing ? "var(--text-muted)" : "white",
          }}
        >
          {processing ? t("btn_converting") : t("btn_select_convert")}
        </button>
        {processing && (
          <button
            onClick={() => { cancelRef.current = true; }}
            className="px-4 py-2.5 rounded-lg text-sm transition-colors"
            style={{
              background: "var(--cancel-bg)",
              color: "var(--cancel-text)",
            }}
          >
            {t("btn_cancel")}
          </button>
        )}
      </div>

      {/* Log Output */}
      {logs.length > 0 && (
        <div className="rounded-lg" style={{ border: "1px solid var(--border)", background: "var(--bg-panel)" }}>
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
                  color:
                    log.type === "error"
                      ? "var(--error)"
                      : log.type === "success"
                        ? "var(--success)"
                        : "var(--text-muted)",
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
