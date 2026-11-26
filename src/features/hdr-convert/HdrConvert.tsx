import { parseAssColor, formatAssColor } from "./ass-processor";
import { OUTPUT_PRESETS } from "./output-naming";
import { MIN_BRIGHTNESS, MAX_BRIGHTNESS, type Eotf } from "./color-engine";
import NumberInput from "../../lib/NumberInput";
import NitViz from "./NitViz";
import { parseHdrStyleNumberInput, type HdrFpsMode } from "./hdr-style-ui-state";
import { LogPanel } from "../../lib/LogPanel";
import { DropErrorBanner } from "../../lib/DropErrorBanner";
import { useHdrConvertController } from "./useHdrConvertController";

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

// Common Windows fonts and optional subtitle-font presets.
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
  const {
    t,
    hdrFiles,
    eotf,
    setEotf,
    brightness,
    brightnessText,
    brightnessInvalid,
    handleBrightnessChange,
    handleBrightnessFromNits,
    template,
    setTemplate,
    customTemplate,
    setCustomTemplate,
    templateInvalid,
    showStylePanel,
    setShowStylePanel,
    style,
    setStyle,
    fpsMode,
    setFpsMode,
    fpsText,
    setFpsText,
    fpsInputInvalid,
    fpsInvalid,
    processing,
    convertDisabled,
    showFileList,
    setShowFileList,
    dropActive,
    dropError,
    setDropError,
    fileContainerRef,
    dropZoneRef,
    handleSelectFiles,
    handleClearFiles,
    handleConvert,
    requestCancel,
    logs,
    clearLogs,
    logScrollRef,
  } = useHdrConvertController();

  return (
    <div className="space-y-4">
      {/* ── File strip — always visible; filename + clear + Select button ── */}
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
            color: processing ? "var(--text-muted)" : "var(--accent-text)",
            height: "38px",
          }}
        >
          {t("btn_select_files")}
        </button>
        {processing && (
          <button
            onClick={requestCancel}
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
            color: convertDisabled ? "var(--text-muted)" : "var(--accent-text)",
            height: "38px",
            minWidth: "120px",
          }}
        >
          {processing ? t("btn_converting") : t("btn_convert")}
        </button>
      </div>

      {/* Selection-rejected banner */}
      <DropErrorBanner message={dropError} onDismiss={() => setDropError(null)} />

      {/* Drop-zone discoverability hint */}
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
            className="px-3 py-2 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-[var(--accent)]"
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
            invalid={brightnessInvalid}
            className="w-36"
          />
          <p className="text-xs" style={{ color: "var(--text-muted)" }}>
            {eotf === "PQ" ? t("brightness_hint_pq") : t("brightness_hint_hlg")}
          </p>
        </div>
      </div>

      {/* ── Brightness visualization (neutral slider + standard presets) ── */}
      <NitViz value={brightness} onChange={handleBrightnessFromNits} disabled={processing} />

      {/* Output Template */}
      <div className="space-y-2">
        <label
          id="hdr-template-label"
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
            className="w-64 px-3 py-2 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-[var(--accent)]"
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
            <>
              <input
                id="hdr-custom-template-input"
                type="text"
                value={customTemplate}
                onChange={(e) => setCustomTemplate(e.target.value)}
                placeholder="{name}.hdr.{eotf}.ass"
                maxLength={200}
                disabled={processing}
                aria-labelledby="hdr-template-label"
                aria-invalid={templateInvalid}
                aria-describedby={templateInvalid ? "hdr-custom-template-error" : undefined}
                className="w-64 px-3 py-2 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-[var(--accent)]"
                style={{
                  background: "var(--bg-input)",
                  border: templateInvalid ? "1px solid var(--error)" : "1px solid var(--border)",
                  color: "var(--text-primary)",
                }}
              />
              {templateInvalid && (
                <span
                  id="hdr-custom-template-error"
                  className="text-xs"
                  style={{ color: "var(--error)" }}
                >
                  {t("template_required")}
                </span>
              )}
            </>
          )}
        </div>
        <p className="text-xs" style={{ color: "var(--text-muted)" }}>
          {t("template_tokens_hint")}
        </p>
      </div>

      {/* Collapsible Style Settings */}
      <div className="rounded-lg" style={{ border: "1px solid var(--border)" }}>
        <button
          type="button"
          onClick={() => setShowStylePanel(!showStylePanel)}
          aria-expanded={showStylePanel}
          className="w-full flex items-center gap-2 px-4 py-3 text-sm font-medium rounded-lg transition-colors"
          style={{ color: "var(--text-secondary)" }}
        >
          <span className="text-xs">{showStylePanel ? "▼" : "▶"}</span>
          {t("style_settings")}
          <span className="text-xs ml-2" style={{ color: "var(--text-muted)" }}>
            {t("style_hint")}
          </span>
          {fpsInvalid && !showStylePanel && (
            <span className="text-xs ml-auto" style={{ color: "var(--error)" }}>
              {t("style_fps_invalid_summary")}
            </span>
          )}
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
                  const n = parseHdrStyleNumberInput(v, 1, 200);
                  if (n !== null) setStyle({ ...style, fontSize: n });
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
                  const n = parseHdrStyleNumberInput(v, 0, 20);
                  if (n !== null) setStyle({ ...style, outlineWidth: n });
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
                  const n = parseHdrStyleNumberInput(v, 0, 20);
                  if (n !== null) setStyle({ ...style, shadowDepth: n });
                }}
                min={0}
                max={20}
                step="0.5"
                disabled={processing}
              />
            </div>
            <div>
              <label
                htmlFor="hdr-fps-mode"
                className="block text-xs mb-1"
                style={{ color: "var(--text-muted)" }}
              >
                {t("style_fps")}
              </label>
              <select
                id="hdr-fps-mode"
                value={fpsMode}
                onChange={(event) => setFpsMode(event.target.value as HdrFpsMode)}
                disabled={processing}
                className="w-full px-2 py-1.5 rounded text-sm"
                style={{
                  background: "var(--bg-input)",
                  border: "1px solid var(--border)",
                  color: "var(--text-primary)",
                }}
              >
                <option value="auto">{t("style_fps_auto")}</option>
                <option value="manual">{t("style_fps_manual")}</option>
              </select>
            </div>
            <div>
              <label
                htmlFor="hdr-fps-value"
                className="block text-xs mb-1"
                style={{ color: "var(--text-muted)" }}
              >
                {t("style_fps_value")}
              </label>
              <NumberInput
                id="hdr-fps-value"
                value={fpsText}
                onChange={setFpsText}
                min={3}
                max={120}
                step="any"
                disabled={processing || fpsMode === "auto"}
                invalid={fpsInputInvalid}
                ariaDescribedBy={fpsInputInvalid ? "hdr-fps-error" : undefined}
              />
              {fpsInputInvalid && (
                <span
                  id="hdr-fps-error"
                  className="block text-xs mt-1"
                  style={{ color: "var(--error)" }}
                >
                  {t("style_fps_invalid")}
                </span>
              )}
            </div>
          </div>
        )}
      </div>

      {/* Log Output */}
      <LogPanel logs={logs} onClear={clearLogs} scrollRef={logScrollRef} />
    </div>
  );
}
