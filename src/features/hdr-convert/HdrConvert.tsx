import { parseAssColor, formatAssColor } from "./ass-processor";
import { OUTPUT_PRESETS } from "./output-naming";
import { MIN_BRIGHTNESS, MAX_BRIGHTNESS, type Eotf } from "./color-engine";
import NumberInput from "../../lib/NumberInput";
import NitViz from "./NitViz";
import { type HdrFpsMode } from "./hdr-style-ui-state";
import { LogPanel } from "../../lib/LogPanel";
import { DropErrorBanner } from "../../lib/DropErrorBanner";
import { renderSubtitleFileStrip } from "../../lib/subtitle-file-strip";
import { COMMON_FONTS, useHdrConvertController } from "./useHdrConvertController";

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
    isCustomFont,
    handleFontSelectChange,
    handleCustomFontNameChange,
    fontSizeText,
    fontSizeInvalid,
    handleFontSizeChange,
    outlineWidthText,
    outlineWidthInvalid,
    handleOutlineWidthChange,
    shadowDepthText,
    shadowDepthInvalid,
    handleShadowDepthChange,
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
      {renderSubtitleFileStrip({
        dropZoneRef,
        fileContainerRef,
        dropActive,
        filePaths: hdrFiles?.filePaths ?? [],
        fileNames: hdrFiles?.fileNames ?? [],
        showFileList,
        onToggleFileList: () => setShowFileList((v) => !v),
        emptyLabel: t("file_empty"),
        selectedTitle: t("files_selected_title", hdrFiles?.filePaths.length ?? 0),
        clearTitle: t("btn_clear_file"),
        selectLabel: t("btn_select_files"),
        clearDisabled: processing,
        selectDisabled: processing,
        onClearFiles: handleClearFiles,
        onSelectFiles: handleSelectFiles,
        trailingActions: (
          <>
            {processing && (
              <button
                type="button"
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
              type="button"
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
          </>
        ),
      })}

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
              <label
                htmlFor="hdr-style-font"
                className="block text-xs mb-1"
                style={{ color: "var(--text-muted)" }}
              >
                {t("style_font")}
              </label>
              <select
                id="hdr-style-font"
                value={isCustomFont ? "__custom" : style.fontName}
                onChange={(e) => handleFontSelectChange(e.target.value)}
                disabled={processing}
                className="w-full px-2 py-1.5 rounded text-sm focus:outline-none focus:ring-2 focus:ring-[var(--accent)]"
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
              {isCustomFont && (
                <input
                  id="hdr-style-custom-font"
                  type="text"
                  value={style.fontName}
                  onChange={(e) => handleCustomFontNameChange(e.target.value)}
                  placeholder={t("style_font_placeholder")}
                  aria-label={t("style_font_placeholder")}
                  disabled={processing}
                  className="w-full mt-1.5 px-2 py-1.5 rounded text-sm focus:outline-none focus:ring-2 focus:ring-[var(--accent)]"
                  style={{
                    background: "var(--bg-input)",
                    border: "1px solid var(--border)",
                    color: "var(--text-primary)",
                  }}
                />
              )}
            </div>
            <div>
              <label
                htmlFor="hdr-style-size"
                className="block text-xs mb-1"
                style={{ color: "var(--text-muted)" }}
              >
                {t("style_size")}
              </label>
              <NumberInput
                id="hdr-style-size"
                value={fontSizeText}
                onChange={handleFontSizeChange}
                min={1}
                max={200}
                disabled={processing}
                invalid={fontSizeInvalid}
              />
            </div>
            <div>
              <label
                htmlFor="hdr-style-primary-color"
                className="block text-xs mb-1"
                style={{ color: "var(--text-muted)" }}
              >
                {t("style_primary_color")}
              </label>
              <input
                id="hdr-style-primary-color"
                type="color"
                value={assColorToHex(style.primaryColor)}
                onChange={(e) =>
                  setStyle({ ...style, primaryColor: hexToAssColor(e.target.value) })
                }
                disabled={processing}
                className="w-full h-8 rounded cursor-pointer focus:outline-none focus:ring-2 focus:ring-[var(--accent)]"
                style={{
                  background: "var(--bg-input)",
                  border: "1px solid var(--border)",
                }}
              />
            </div>
            <div>
              <label
                htmlFor="hdr-style-outline-color"
                className="block text-xs mb-1"
                style={{ color: "var(--text-muted)" }}
              >
                {t("style_outline_color")}
              </label>
              <input
                id="hdr-style-outline-color"
                type="color"
                value={assColorToHex(style.outlineColor)}
                onChange={(e) =>
                  setStyle({ ...style, outlineColor: hexToAssColor(e.target.value) })
                }
                disabled={processing}
                className="w-full h-8 rounded cursor-pointer focus:outline-none focus:ring-2 focus:ring-[var(--accent)]"
                style={{
                  background: "var(--bg-input)",
                  border: "1px solid var(--border)",
                }}
              />
            </div>
            <div>
              <label
                htmlFor="hdr-style-outline-width"
                className="block text-xs mb-1"
                style={{ color: "var(--text-muted)" }}
              >
                {t("style_outline_width")}
              </label>
              <NumberInput
                id="hdr-style-outline-width"
                value={outlineWidthText}
                onChange={handleOutlineWidthChange}
                min={0}
                max={20}
                step="0.5"
                disabled={processing}
                invalid={outlineWidthInvalid}
              />
            </div>
            <div>
              <label
                htmlFor="hdr-style-shadow-depth"
                className="block text-xs mb-1"
                style={{ color: "var(--text-muted)" }}
              >
                {t("style_shadow_depth")}
              </label>
              <NumberInput
                id="hdr-style-shadow-depth"
                value={shadowDepthText}
                onChange={handleShadowDepthChange}
                min={0}
                max={20}
                step="0.5"
                disabled={processing}
                invalid={shadowDepthInvalid}
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
