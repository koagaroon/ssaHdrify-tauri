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

// Common fonts available on most systems (cross-platform)
const COMMON_FONTS = [
  "Arial", "Arial Black", "Calibri", "Cambria", "Comic Sans MS",
  "Consolas", "Courier New", "Georgia", "Impact", "Lucida Console",
  "Microsoft YaHei", "PingFang SC", "Segoe UI", "Tahoma",
  "Times New Roman", "Trebuchet MS", "Verdana",
  "Noto Sans", "Noto Sans CJK SC", "Noto Serif",
  "Source Han Sans", "Source Han Serif",
];

// ── EOTF descriptions ──────────────────────────────────────
const EOTF_INFO: Record<Eotf, { desc: string; brightnessHint: string }> = {
  PQ: {
    desc: "Absolute brightness, up to 10,000 nits. For HDR10 / Dolby Vision streaming and disc content.",
    brightnessHint: "Recommended: 100–300 nits (BT.2408 standard: 203)",
  },
  HLG: {
    desc: "Relative brightness, adapts to display. For broadcast HDR and SDR-compatible content.",
    brightnessHint: "Recommended: 100–400 nits (display-adaptive)",
  },
};

interface LogEntry {
  id: number;
  text: string;
  type: "info" | "error" | "success";
}

export default function HdrConvert() {
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
      addLog(`Invalid brightness: must be ${MIN_BRIGHTNESS}–${MAX_BRIGHTNESS} nits`, "error");
      return;
    }

    const paths = await pickSubtitleFiles();
    if (!paths || paths.length === 0) return;

    setProcessing(true);
    cancelRef.current = false;
    addLog(`Starting conversion: ${paths.length} file(s), ${eotf} @ ${brightness} nits`);

    const outputPaths = new Set<string>();
    let successCount = 0;

    for (const filePath of paths) {
      if (cancelRef.current) {
        addLog("Conversion cancelled.", "info");
        break;
      }

      const fileName = filePath.replace(/\\/g, "/").split("/").pop() ?? filePath;
      addLog(`Processing: ${fileName}`);

      try {
        // Resolve output path
        let outputPath: string;
        try {
          outputPath = resolveOutputPath(filePath, activeTemplate, eotf);
        } catch (e) {
          addLog(`Skipped ${fileName}: ${e instanceof Error ? e.message : e}`, "error");
          continue;
        }

        // Check for duplicate output targets
        const normalizedOut = outputPath.replace(/\\/g, "/").toLowerCase();
        if (outputPaths.has(normalizedOut)) {
          addLog(`Skipped ${fileName}: duplicate output path`, "error");
          continue;
        }
        outputPaths.add(normalizedOut);

        // Read input file
        let content: string;
        try {
          content = await readText(filePath);
        } catch (e) {
          addLog(`Error reading ${fileName}: ${e instanceof Error ? e.message : e}`, "error");
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
          addLog(`Skipped ${fileName}: unsupported format`, "error");
          continue;
        }

        // Write output
        await writeText(outputPath, assContent);
        const outName = outputPath.replace(/\\/g, "/").split("/").pop() ?? outputPath;
        addLog(`Done: ${outName}`, "success");
        successCount++;
      } catch (e) {
        addLog(`Error converting ${fileName}: ${e instanceof Error ? e.message : e}`, "error");
      }
    }

    addLog(`Conversion complete: ${successCount}/${paths.length} file(s) processed`, "success");
    setProcessing(false);
  }, [brightness, eotf, activeTemplate, style, addLog]);

  return (
    <div className="max-w-2xl space-y-5">
      {/* EOTF Selection */}
      <div className="space-y-2">
        <label className="block text-sm font-medium text-neutral-300">
          Content EOTF Curve
        </label>
        <select
          value={eotf}
          onChange={(e) => setEotf(e.target.value as Eotf)}
          disabled={processing}
          className="w-48 px-3 py-2 rounded-lg bg-neutral-800 border border-neutral-700 text-neutral-100 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
        >
          <option value="PQ">PQ (Perceptual Quantizer)</option>
          <option value="HLG">HLG (Hybrid Log-Gamma)</option>
        </select>
        <p className="text-xs text-neutral-500">{EOTF_INFO[eotf].desc}</p>
      </div>

      {/* Brightness */}
      <div className="space-y-2">
        <label className="block text-sm font-medium text-neutral-300">
          Target Subtitle Brightness (nits)
        </label>
        <NumberInput
          value={brightnessText}
          onChange={handleBrightnessChange}
          min={MIN_BRIGHTNESS}
          max={MAX_BRIGHTNESS}
          disabled={processing}
          className="w-36"
        />
        <p className="text-xs text-neutral-500">{EOTF_INFO[eotf].brightnessHint}</p>
      </div>

      {/* Output Template */}
      <div className="space-y-2">
        <label className="block text-sm font-medium text-neutral-300">
          Output Template
        </label>
        <div className="flex flex-col gap-2">
          <select
            value={template}
            onChange={(e) => setTemplate(e.target.value)}
            disabled={processing}
            className="w-64 px-3 py-2 rounded-lg bg-neutral-800 border border-neutral-700 text-neutral-100 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
          >
            {OUTPUT_PRESETS.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
            <option value="custom">Custom...</option>
          </select>
          {template === "custom" && (
            <input
              type="text"
              value={customTemplate}
              onChange={(e) => setCustomTemplate(e.target.value)}
              placeholder="{name}.hdr.{eotf}.ass"
              disabled={processing}
              className="w-64 px-3 py-2 rounded-lg bg-neutral-800 border border-neutral-700 text-neutral-100 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
            />
          )}
        </div>
      </div>

      {/* Collapsible Style Settings */}
      <div className="border border-neutral-800 rounded-lg">
        <button
          onClick={() => setShowStylePanel(!showStylePanel)}
          className="w-full flex items-center gap-2 px-4 py-3 text-sm font-medium text-neutral-300 hover:bg-neutral-800/50 rounded-lg transition-colors"
        >
          <span className="text-xs">{showStylePanel ? "▼" : "▶"}</span>
          Style Settings
          <span className="text-xs text-neutral-500 ml-2">
            (SRT/SUB input only)
          </span>
        </button>
        {showStylePanel && (
          <div className="px-4 pb-4 grid grid-cols-2 gap-3">
            <div>
              <label className="block text-xs text-neutral-500 mb-1">Font</label>
              <select
                value={COMMON_FONTS.includes(style.fontName) ? style.fontName : "__custom"}
                onChange={(e) => {
                  if (e.target.value !== "__custom") {
                    setStyle({ ...style, fontName: e.target.value });
                  }
                }}
                disabled={processing}
                className="w-full px-2 py-1.5 rounded bg-neutral-800 border border-neutral-700 text-sm text-neutral-100"
              >
                {COMMON_FONTS.map((f) => (
                  <option key={f} value={f}>{f}</option>
                ))}
                <option value="__custom">Custom...</option>
              </select>
              {!COMMON_FONTS.includes(style.fontName) && (
                <input
                  type="text"
                  value={style.fontName}
                  onChange={(e) => setStyle({ ...style, fontName: e.target.value })}
                  placeholder="Font family name"
                  disabled={processing}
                  className="w-full mt-1.5 px-2 py-1.5 rounded bg-neutral-800 border border-neutral-700 text-sm text-neutral-100"
                />
              )}
            </div>
            <div>
              <label className="block text-xs text-neutral-500 mb-1">Size</label>
              <NumberInput
                value={style.fontSize}
                onChange={(v) => setStyle({ ...style, fontSize: parseInt(v) || 48 })}
                min={1}
                max={200}
                disabled={processing}
              />
            </div>
            <div>
              <label className="block text-xs text-neutral-500 mb-1">
                Primary Color
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
                className="w-full h-8 rounded bg-neutral-800 border border-neutral-700 cursor-pointer"
              />
            </div>
            <div>
              <label className="block text-xs text-neutral-500 mb-1">
                Outline Color
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
                className="w-full h-8 rounded bg-neutral-800 border border-neutral-700 cursor-pointer"
              />
            </div>
            <div>
              <label className="block text-xs text-neutral-500 mb-1">
                Outline Width
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
              <label className="block text-xs text-neutral-500 mb-1">
                Shadow Depth
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
              <label className="block text-xs text-neutral-500 mb-1">
                FPS (SUB only)
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
          className="px-6 py-2.5 rounded-lg bg-blue-600 hover:bg-blue-500 disabled:bg-neutral-700 disabled:text-neutral-500 text-white font-medium text-sm transition-colors"
        >
          {processing ? "Converting..." : "Select Files & Convert"}
        </button>
        {processing && (
          <button
            onClick={() => { cancelRef.current = true; }}
            className="px-4 py-2.5 rounded-lg bg-red-600/20 hover:bg-red-600/30 text-red-400 text-sm transition-colors"
          >
            Cancel
          </button>
        )}
      </div>

      {/* Log Output */}
      {logs.length > 0 && (
        <div className="border border-neutral-800 rounded-lg bg-neutral-900/50">
          <div className="flex items-center justify-between px-3 py-2 border-b border-neutral-800">
            <span className="text-xs font-medium text-neutral-400">Log</span>
            <button
              onClick={() => setLogs([])}
              className="text-xs text-neutral-500 hover:text-neutral-300"
            >
              Clear
            </button>
          </div>
          <div className="max-h-48 overflow-y-auto p-3 font-mono text-xs space-y-0.5">
            {logs.map((log) => (
              <div
                key={log.id}
                className={
                  log.type === "error"
                    ? "text-red-400"
                    : log.type === "success"
                      ? "text-green-400"
                      : "text-neutral-400"
                }
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
