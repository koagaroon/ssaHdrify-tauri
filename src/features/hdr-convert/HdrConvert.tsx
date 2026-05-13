import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { pickSubtitleFiles, readText, writeText, fileNameFromPath } from "../../lib/tauri-api";
import { processAssContent, parseAssColor, formatAssColor } from "./ass-processor";
import {
  processSrtUserText,
  buildAssDocument,
  isNativeAss,
  isConvertible,
  DEFAULT_STYLE,
  type StyleConfig,
} from "./srt-converter";
import { resolveOutputPath, OUTPUT_PRESETS, DEFAULT_TEMPLATE } from "./output-naming";
import { DEFAULT_BRIGHTNESS, MIN_BRIGHTNESS, MAX_BRIGHTNESS, type Eotf } from "./color-engine";
import { ask } from "@tauri-apps/plugin-dialog";

import { parseSubtitle } from "../../lib/subtitle-parser";
import NumberInput from "../../lib/NumberInput";
import NitViz from "./NitViz";
import { useI18n } from "../../i18n/useI18n";
import { useFileContext } from "../../lib/FileContext";
import type { Status } from "../../lib/StatusContext";
import { useTabStatus } from "../../lib/useTabStatus";
import { useFolderDrop } from "../../lib/useFolderDrop";
import { countExistingFiles } from "../../lib/output-collisions";
import { useClickOutside } from "../../lib/useClickOutside";
import { useLogPanel } from "../../lib/useLogPanel";
import { LogPanel } from "../../lib/LogPanel";
import { DropErrorBanner } from "../../lib/DropErrorBanner";
import {
  buildConflictMessage,
  normalizeOutputKey,
  sanitizeError,
  sanitizeForDialog,
} from "../../lib/dedup-helpers";

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

// Common fonts available on most systems (cross-platform).
// Kept as a plain array for <option> rendering order, plus a Set built once
// for O(1) membership checks in the style panel's "is this a preset?" logic.
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
const COMMON_FONTS_SET = new Set(COMMON_FONTS);

export default function HdrConvert() {
  const { t } = useI18n();
  const { hdrFiles, setHdrFiles, clearFile, isFileInUse } = useFileContext();
  const [eotf, setEotf] = useState<Eotf>("PQ");
  const [brightness, setBrightness] = useState(DEFAULT_BRIGHTNESS);
  const [brightnessText, setBrightnessText] = useState(String(DEFAULT_BRIGHTNESS));
  const [template, setTemplate] = useState<string>(DEFAULT_TEMPLATE);
  const [customTemplate, setCustomTemplate] = useState("");
  const [showStylePanel, setShowStylePanel] = useState(false);
  const [style, setStyle] = useState<StyleConfig>({ ...DEFAULT_STYLE });
  const [processing, setProcessing] = useState(false);
  const { logs, addLog, clearLogs, logScrollRef } = useLogPanel();
  const [showFileList, setShowFileList] = useState(false);
  // Tracks the outcome of the last convert attempt — null between
  // selections or before any attempt; gets cleared when hdrFiles changes
  // so "done" / "cancelled" don't linger after the user picks a new file.
  // "cancelled" covers both pre-flight cancel (user dismissed the
  // overwrite-confirm dialog) and mid-batch cancel (user clicked the
  // in-flight Cancel button); both should read the same to the user.
  const [lastActionResult, setLastActionResult] = useState<
    "success" | "error" | "cancelled" | null
  >(null);
  // N-of-M progress for the active batch, surfaced in the footer chip.
  // Null between batches; never persists past `setProcessing(false)` in
  // the convert finally block.
  const [progress, setProgress] = useState<{ processed: number; total: number } | null>(null);
  // Drag-active highlight on the file strip — toggled by useFolderDrop.
  const [dropActive, setDropActive] = useState(false);
  // Error banner shown above the file strip when a selection is rejected
  // (e.g., cross-tab dedup conflict). Strict mode: any conflict rejects
  // the entire drop, no state change, banner persists until the next
  // selection attempt or until the user clicks Clear.
  const [dropError, setDropError] = useState<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  // Synchronous double-click guard — `processing` state lags behind the
  // setProcessing(true) call by one render, so a fast second click on
  // Convert can pass the disabled gate before React paints the busy
  // state. busyRef is written synchronously at handler entry and read
  // by the next click before any state machinery sees it.
  const busyRef = useRef(false);
  // Generation counter for stale-pick guards on async file pickers.
  // Mirrors the pattern in TimingShift / FontEmbed / BatchRename:
  // bump on each handle*Files entry, then the handler discards results
  // whose generation no longer matches.
  const pickGenRef = useRef(0);
  const fileContainerRef = useRef<HTMLDivElement>(null);
  const dropZoneRef = useRef<HTMLDivElement>(null);

  // Reset the last-convert outcome when the selection changes so the
  // footer indicator doesn't stay green on a brand-new batch.
  useEffect(() => {
    setLastActionResult(null);
  }, [hdrFiles]);

  // Publish current status to the shared context — the footer reads it.
  const tabStatus = useMemo<Status>(() => {
    if (!hdrFiles) return { kind: "idle", message: t("status_hdr_idle") };
    if (processing) {
      // Pass progress only while it's a real { processed, total } object —
      // the footer chip suppresses itself when total === 0 anyway, but
      // keeping the field undefined (vs zeroed) avoids a brief 0/0 flash
      // on the first render of `processing = true`.
      return {
        kind: "busy",
        message: t("status_hdr_busy"),
        progress: progress ?? undefined,
      };
    }
    if (lastActionResult === "success") return { kind: "done", message: t("status_hdr_done") };
    if (lastActionResult === "error") return { kind: "error", message: t("status_hdr_error") };
    if (lastActionResult === "cancelled") {
      // Cancellation is neither a success nor a failure — the user
      // intentionally stepped back. Use the "pending" kind (amber dot)
      // since files are still loaded and a fresh attempt is one click
      // away; only the message differs from default pending.
      return { kind: "pending", message: t("status_hdr_cancelled") };
    }
    return {
      kind: "pending",
      message: t("status_hdr_pending", hdrFiles.filePaths.length),
    };
  }, [hdrFiles, processing, lastActionResult, progress, t]);
  useTabStatus("hdr", tabStatus);

  // File-list dropdown: close on click outside or Escape.
  useClickOutside(showFileList, fileContainerRef, () => setShowFileList(false));

  const handleBrightnessChange = (value: string) => {
    setBrightnessText(value);
    const num = parseInt(value, 10);
    if (!Number.isNaN(num) && num >= MIN_BRIGHTNESS && num <= MAX_BRIGHTNESS) {
      setBrightness(num);
    }
  };

  // Derived: brightnessText parses cleanly but lands outside the
  // [MIN_BRIGHTNESS, MAX_BRIGHTNESS] window. Used to drive the input's
  // visible-error border (N-R5-FEFEAT-25). Without this signal, a user
  // typing "99999" sees no validation feedback and the Convert button
  // proceeds with the prior in-range value silently — the kind of
  // surprise vibe-coding.md no-silent-action exists to prevent.
  const brightnessOutOfRange = (() => {
    const num = parseInt(brightnessText, 10);
    return !Number.isNaN(num) && (num < MIN_BRIGHTNESS || num > MAX_BRIGHTNESS);
  })();

  // Slider / preset paths emit a validated number — keep both state slots in sync.
  const handleBrightnessFromNits = useCallback((nits: number) => {
    setBrightness(nits);
    setBrightnessText(String(nits));
  }, []);

  const activeTemplate = template === "custom" ? customTemplate : template;
  const convertDisabled = !hdrFiles || processing;

  // ── File selection (separate from conversion) ──────────
  // Strict cross-tab dedup contract: any conflict rejects the WHOLE
  // selection — see buildConflictMessage / FileContext for rationale.
  const handleSelectFiles = useCallback(async () => {
    // Stale-pick guard: if the user clicks Select again before the OS
    // dialog returns (or clears the prior selection in another way),
    // each open() call has its own `gen` snapshot. Once the dialog
    // resolves, only the most recent `gen` is allowed to commit. Mirrors
    // the pattern in TimingShift / FontEmbed / BatchRename.
    const gen = (pickGenRef.current = pickGenRef.current + 1);
    const paths = await pickSubtitleFiles(t);
    if (gen !== pickGenRef.current) return;
    if (!paths || paths.length === 0) return;

    const conflictMsg = buildConflictMessage(paths, "hdr", isFileInUse, t);
    if (conflictMsg) {
      setDropError(conflictMsg);
      return;
    }
    setDropError(null);

    // Silent replace: see FileContext.tsx for design rationale
    const names = paths.map(fileNameFromPath);
    setHdrFiles({ filePaths: paths, fileNames: names });
  }, [isFileInUse, setHdrFiles, t]);

  // ── Folder drag-drop ingestion ──────────────────────────
  // Dropped paths come back from Rust expansion already flat-listed (one
  // level deep). HDR Convert is a subtitle-only tab, so video files in a
  // mixed-folder drop are filtered out via the same isNativeAss /
  // isConvertible gate the convert loop uses. This keeps "drop a show
  // folder" working even when it contains .mkv siblings.
  const handleDroppedPaths = useCallback(
    (paths: string[]) => {
      const subtitlePaths = paths.filter((p) => {
        const name = fileNameFromPath(p);
        return isNativeAss(name) || isConvertible(name);
      });

      if (subtitlePaths.length === 0) {
        // Surface through both the log AND the standard DropErrorBanner
        // (N-R5-FEFEAT-09 mirror). Users with collapsed log panels see
        // nothing from log-only — banner is the always-visible feedback.
        const msg = t("msg_no_subtitle_in_drop");
        addLog(msg, "error");
        setDropError(msg);
        return;
      }

      const conflictMsg = buildConflictMessage(subtitlePaths, "hdr", isFileInUse, t);
      if (conflictMsg) {
        setDropError(conflictMsg);
        return;
      }
      setDropError(null);

      const names = subtitlePaths.map(fileNameFromPath);
      setHdrFiles({ filePaths: subtitlePaths, fileNames: names });
    },
    [isFileInUse, setHdrFiles, setDropError, addLog, t]
  );

  useFolderDrop({
    ref: dropZoneRef,
    onPaths: handleDroppedPaths,
    onActiveChange: setDropActive,
    onError: (e) => setDropError(sanitizeError(e)),
    disabled: processing,
  });

  // ── Conversion (uses already-selected files) ───────────
  const handleConvert = useCallback(async () => {
    if (!hdrFiles) return;
    // Synchronous double-click gate — `processing` state lags
    // setProcessing(true) by one render, so a fast second click can
    // pass the disabled gate before React paints. busyRef is written
    // synchronously here and released in the outer finally below so
    // every exit path (validation fail / pre-flight cancel / loop done
    // / loop throw) clears it.
    if (busyRef.current) return;
    busyRef.current = true;
    try {
      // Validate brightness
      if (brightness < MIN_BRIGHTNESS || brightness > MAX_BRIGHTNESS) {
        addLog(t("msg_invalid_brightness", MIN_BRIGHTNESS, MAX_BRIGHTNESS), "error");
        return;
      }

      const paths = hdrFiles.filePaths;

      // Pre-flight overwrite check. Template-derived output paths are
      // deterministic — re-clicking Convert (e.g., the user came back to
      // the window after minimizing and forgot the run already finished)
      // would silently overwrite previous outputs. Stat each projected
      // path; if any already exist, surface a single ask() dialog before
      // entering the busy state. Failed-to-resolve paths skip pre-flight
      // (the main loop will log per-file errors as before).
      const projectedOutputs: string[] = [];
      for (const filePath of paths) {
        try {
          projectedOutputs.push(resolveOutputPath(filePath, activeTemplate, eotf));
        } catch {
          // Resolution failure logged in the main loop with file context;
          // pre-flight just skips so the existence check doesn't see an
          // invalid path.
        }
      }
      try {
        const existingCount = await countExistingFiles(projectedOutputs);
        if (existingCount > 0) {
          const confirmed = await ask(t("msg_overwrite_confirm", existingCount, paths.length), {
            title: t("dialog_overwrite_title"),
            kind: "warning",
          });
          if (!confirmed) {
            addLog(t("msg_cancelled"), "info");
            setLastActionResult("cancelled");
            return;
          }
        }
      } catch (e) {
        addLog(t("error_prefix", sanitizeError(e)), "error");
        setLastActionResult("error");
        return;
      }

      // Construct the AbortController at the boundary into busy state —
      // pre-flight bail-out paths must not leak unaborted controllers.
      // busyRef above is the synchronous gate for double clicks; the
      // controller is only allocated once we're committed to a run.
      abortRef.current = new AbortController();
      setProcessing(true);
      setProgress({ processed: 0, total: paths.length });

      try {
        addLog(t("msg_start_conversion", paths.length, eotf, brightness));

        const outputPaths = new Set<string>();
        let successCount = 0;
        let processedCount = 0;

        for (const filePath of paths) {
          if (abortRef.current?.signal.aborted) {
            addLog(t("msg_cancelled"), "info");
            break;
          }

          // Defensive fallback: `fileNameFromPath` is currently total but
          // a future refactor (path-validation rejection, etc.) could
          // make it throw — and then the catch block at the bottom of
          // the loop would lose `fileName` (Round 1 F2.N-R1-1 brittle
          // TDZ shape). `let` + initial = filePath gives the catch a
          // usable identifier in every code path.
          //
          // Wave 7.1 BiDi parity: fileName flows into ~8 addLog calls
          // below; sanitize once at source so every downstream
          // interpolation is automatically BiDi-scrubbed without each
          // callsite remembering to wrap. Same pattern as FontEmbed.
          let fileName = sanitizeForDialog(filePath);
          try {
            fileName = sanitizeForDialog(fileNameFromPath(filePath));
          } catch {
            // Keep the raw path — better than no attribution.
          }
          addLog(t("msg_processing", fileName));

          try {
            // Check file extension FIRST — cheap test, avoids wasted work in
            // resolveOutputPath / readText for obviously-unsupported files.
            if (!isNativeAss(fileName) && !isConvertible(fileName)) {
              addLog(t("msg_unsupported", fileName), "error");
              continue;
            }

            // Resolve output path
            let outputPath: string;
            try {
              outputPath = resolveOutputPath(filePath, activeTemplate, eotf);
            } catch (e) {
              addLog(
                t("msg_skipped", fileName, sanitizeError(e)),
                "error"
              );
              continue;
            }

            // Within-batch output dedup — see normalizeOutputKey for the
            // NFC + forward-slash + lowercase semantics.
            const normalizedOut = normalizeOutputKey(outputPath);
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
                t("msg_read_error", fileName, sanitizeError(e)),
                "error"
              );
              continue;
            }

            // Check cancel after I/O
            if (abortRef.current?.signal.aborted) break;

            let assContent: string;

            if (isNativeAss(fileName)) {
              // Direct ASS processing
              assContent = processAssContent(content, brightness, eotf);
            } else if (isConvertible(fileName)) {
              // SRT/SUB → ASS conversion path. processSrtUserText composes
              // the two-step user-text pipeline (escape user braces, then
              // inject our trusted color tags) so the order can't be swapped
              // or one step skipped by a future caller.
              const preprocessed = processSrtUserText(content);

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
            if (abortRef.current?.signal.aborted) break;

            // Write output
            await writeText(outputPath, assContent);
            const outName = fileNameFromPath(outputPath);
            addLog(t("msg_done", outName), "success");
            successCount++;
          } catch (e) {
            addLog(
              t("msg_convert_error", fileName, sanitizeError(e)),
              "error"
            );
          } finally {
            // Bump the N-of-M counter once per iteration regardless of
            // outcome (success / skip / error). If cancel is seen at the
            // top of the next loop, this block does not run for that next
            // file; if cancel lands mid-file, the current file is counted
            // as processed before the outer finally clears the progress chip.
            processedCount++;
            setProgress({ processed: processedCount, total: paths.length });
          }
        }

        // Record outcome for the footer status. Order matters: a cancel
        // takes precedence over partial success/error counts, because the
        // user explicitly stepped back — surfacing "Conversion complete"
        // when they cancelled mid-batch would be a lie. Only treat the
        // outcome as success/error when the loop ran to completion.
        // Avoid success-log vs error-footer contradiction
        // (N-R5-FEFEAT-16): when every file failed, the previous form
        // fired both "complete: 0/N" + red "failed" footer. Split so
        // success-log fires only when at least one file landed;
        // full-batch failure gets its own error line.
        const aborted = !!abortRef.current?.signal.aborted;
        if (aborted) {
          setLastActionResult("cancelled");
        } else if (successCount > 0) {
          addLog(t("msg_complete", successCount, paths.length), "success");
          setLastActionResult("success");
        } else {
          addLog(t("msg_all_failed", paths.length), "error");
          setLastActionResult("error");
        }
      } finally {
        setProcessing(false);
        setProgress(null);
      }
    } finally {
      busyRef.current = false;
    }
  }, [hdrFiles, brightness, eotf, activeTemplate, style, addLog, t]);

  const handleClearFiles = useCallback(() => {
    clearFile("hdr");
    setDropError(null);
  }, [clearFile]);

  return (
    <div className="space-y-4">
      {/* ── File strip — always visible; filename + clear + Select button ──
           When >1 file is selected, the filename area becomes a clickable
           dropdown showing all selected files (max ~5 rows, scroll beyond).
           The strip doubles as a drag-drop target: drop subtitle files or a
           folder anywhere on it, video siblings inside the folder are
           silently filtered out. */}
      <div
        ref={dropZoneRef}
        className={`drop-zone flex items-center gap-2${dropActive ? " drop-active" : ""}`}
      >
        <div ref={fileContainerRef} className="flex-1 min-w-0" style={{ position: "relative" }}>
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
                  {t("files_selected_title", hdrFiles.filePaths.length)}
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
              background: processing ? "var(--bg-input)" : "var(--cancel-bg)",
              color: processing ? "var(--text-muted)" : "var(--cancel-text)",
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
        {processing && (
          <button
            onClick={() => {
              abortRef.current?.abort();
            }}
            className="flex-none px-4 rounded-lg text-sm transition-colors"
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
          className="flex-none px-6 rounded-lg font-medium text-sm transition-colors"
          style={{
            background: convertDisabled ? "var(--bg-input)" : "var(--accent)",
            color: convertDisabled ? "var(--text-muted)" : "white",
            height: "38px",
            minWidth: "120px",
          }}
        >
          {processing ? t("btn_converting") : t("btn_convert")}
        </button>
      </div>

      {/* Selection-rejected banner. Sticky until the next selection
           attempt or until the user clicks ✕ on the file strip. */}
      <DropErrorBanner message={dropError} onDismiss={() => setDropError(null)} />

      {/* Drop-zone discoverability hint — drag is invisible without
           prompting; surface it inline so users don't have to read docs.
           Visible only when the strip is empty (idle), where the hint is
           most useful. Hidden mid-batch to avoid distraction. */}
      {!hdrFiles && !dropError && (
        <p className="text-xs ml-1" style={{ color: "var(--text-muted)" }}>
          {t("hdr_drop_hint")}
        </p>
      )}

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
            invalid={brightnessOutOfRange}
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
        <p className="text-xs" style={{ color: "var(--text-muted)" }}>
          {t("template_tokens_hint")}
        </p>
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
                value={COMMON_FONTS_SET.has(style.fontName) ? style.fontName : "__custom"}
                onChange={(e) => {
                  // When the user picks "Custom", drop fontName to ""
                  // so `!COMMON_FONTS_SET.has(style.fontName)` flips
                  // true and the custom <input> below mounts. Without
                  // this drop the select's `value=` re-derives from
                  // the unchanged fontName ("Arial" etc.) and the
                  // dropdown visually snaps back to the previous font
                  // — the custom input never appears (N-R5-FEFEAT-20).
                  setStyle({
                    ...style,
                    fontName: e.target.value === "__custom" ? "" : e.target.value,
                  });
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
              {!COMMON_FONTS_SET.has(style.fontName) && (
                <input
                  type="text"
                  value={style.fontName}
                  onChange={(e) => setStyle({ ...style, fontName: e.target.value })}
                  placeholder={t("style_font_placeholder")}
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
                  const n = parseInt(v, 10);
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

      {/* Log Output */}
      <LogPanel logs={logs} onClear={clearLogs} scrollRef={logScrollRef} />
    </div>
  );
}
