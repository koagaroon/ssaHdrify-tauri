/**
 * All user-facing strings for en/zh i18n.
 *
 * Key naming: snake_case, grouped by feature prefix.
 * Parametric strings use {0}, {1}, ... placeholders.
 */

export type Lang = "en" | "zh";

type StringEntry = Record<Lang, string>;

export const strings: Record<string, StringEntry> = {
  // ── App Shell ───────────────────────────────────────────
  app_title: { en: "SSA HDRify", zh: "SSA HDRify" },
  tab_hdr: { en: "HDR Convert", zh: "HDR 转换" },
  tab_hdr_desc: { en: "SDR → HDR color space conversion", zh: "SDR → HDR 色彩空间转换" },
  tab_timing: { en: "Time Shift", zh: "时间轴偏移" },
  tab_timing_desc: { en: "Batch subtitle time adjustment", zh: "批量字幕时间轴调整" },
  tab_fonts: { en: "Font Embed", zh: "字体嵌入" },
  tab_fonts_desc: { en: "Subset & embed fonts into ASS", zh: "字体子集化并嵌入 ASS" },
  footer_version: { en: "SSA HDRify v0.1.0", zh: "SSA HDRify v0.1.0" },

  // ── Theme ───────────────────────────────────────────────
  theme_auto: { en: "Follow System", zh: "跟随系统" },
  theme_light: { en: "Light", zh: "浅色" },
  theme_dark: { en: "Dark", zh: "深色" },

  // ── HDR Convert ─────────────────────────────────────────
  eotf_label: { en: "EOTF Curve", zh: "EOTF 曲线" },
  eotf_pq: { en: "PQ (Perceptual Quantizer)", zh: "PQ（感知量化器）" },
  eotf_hlg: { en: "HLG (Hybrid Log-Gamma)", zh: "HLG（混合对数伽马）" },
  eotf_pq_desc: {
    en: "Absolute brightness, up to 10,000 nits. For HDR10 / Dolby Vision streaming and disc content.",
    zh: "绝对亮度映射，最高一万尼特。适用于 HDR10/杜比视界流媒体及蓝光内容。",
  },
  eotf_hlg_desc: {
    en: "Relative brightness, adapts to display. For broadcast HDR and SDR-compatible content.",
    zh: "相对亮度映射，适应显示器。适用于广播 HDR 及需兼容 SDR 的内容。",
  },
  brightness_label: { en: "Subtitle Brightness (nits)", zh: "字幕亮度（尼特）" },
  brightness_hint_pq: {
    en: "Recommended: 100–300 nits (BT.2408 standard: 203)",
    zh: "推荐：100–300 尼特（BT.2408 标准值 203）",
  },
  brightness_hint_hlg: {
    en: "Recommended: 100–400 nits (display-adaptive)",
    zh: "推荐：100–400 尼特（随显示器自适应）",
  },
  template_label: { en: "Output Template", zh: "输出模板" },
  template_custom: { en: "Custom...", zh: "自定义…" },
  style_settings: { en: "Style Settings", zh: "样式设置" },
  style_hint: { en: "(SRT/SUB input only)", zh: "（仅 SRT/SUB 输入）" },
  style_font: { en: "Font", zh: "字体" },
  style_size: { en: "Size", zh: "字号" },
  style_primary_color: { en: "Primary Color", zh: "主要颜色" },
  style_outline_color: { en: "Outline Color", zh: "描边颜色" },
  style_outline_width: { en: "Outline Width", zh: "描边宽度" },
  style_shadow_depth: { en: "Shadow Depth", zh: "阴影深度" },
  style_fps: { en: "FPS (SUB only)", zh: "帧率（仅 SUB）" },
  style_font_custom: { en: "Custom...", zh: "自定义…" },
  btn_select_files: { en: "Select File(s)", zh: "选择文件" },
  btn_convert: { en: "Convert", zh: "转换" },
  btn_converting: { en: "Converting...", zh: "转换中…" },
  btn_cancel: { en: "Cancel", zh: "取消" },
  log_title: { en: "Log", zh: "日志" },
  log_clear: { en: "Clear", zh: "清空" },

  // HDR Convert — log messages
  msg_invalid_brightness: {
    en: "Invalid brightness: must be {0}–{1} nits",
    zh: "亮度无效：须在 {0}–{1} 尼特范围内",
  },
  msg_start_conversion: {
    en: "Starting conversion: {0} file(s), {1} @ {2} nits",
    zh: "开始转换：{0} 个文件，{1} @ {2} 尼特",
  },
  msg_processing: { en: "Processing: {0}", zh: "处理中：{0}" },
  msg_skipped: { en: "Skipped {0}: {1}", zh: "已跳过 {0}：{1}" },
  msg_skipped_duplicate: { en: "Skipped {0}: duplicate output path", zh: "已跳过 {0}：输出路径重复" },
  msg_read_error: { en: "Error reading {0}: {1}", zh: "读取 {0} 出错：{1}" },
  msg_unsupported: { en: "Skipped {0}: unsupported format", zh: "已跳过 {0}：不支持的格式" },
  msg_done: { en: "Done: {0}", zh: "完成：{0}" },
  msg_convert_error: { en: "Error converting {0}: {1}", zh: "转换 {0} 出错：{1}" },
  msg_complete: {
    en: "Conversion complete: {0}/{1} file(s) processed",
    zh: "转换完成：已处理 {0}/{1} 个文件",
  },
  msg_cancelled: { en: "Conversion cancelled.", zh: "转换已取消。" },

  // ── Time Shift ──────────────────────────────────────────
  captions_count: { en: "{0} captions", zh: "{0} 条字幕" },
  offset_label: { en: "Offset", zh: "偏移量" },
  unit_ms: { en: "ms", zh: "毫秒" },
  unit_seconds: { en: "seconds", zh: "秒" },
  direction_slower: { en: "Slower (+)", zh: "延后（+）" },
  direction_faster: { en: "Faster (−)", zh: "提前（−）" },
  offset_hint: { en: "1 second = 1000 ms", zh: "1 秒 = 1000 毫秒" },
  threshold_label: { en: "Apply only after:", zh: "仅在此时间后应用：" },
  threshold_invalid: { en: "Invalid format (HH:MM:SS.mmm)", zh: "格式无效（HH:MM:SS.mmm）" },
  preview_title: { en: "Preview (first {0} of {1})", zh: "预览（前 {0} 条，共 {1} 条）" },
  col_index: { en: "#", zh: "#" },
  col_original: { en: "Original", zh: "原始" },
  col_shifted: { en: "After Shift", zh: "偏移后" },
  btn_save_as: { en: "Save As...", zh: "另存为…" },
  msg_saved: { en: "Saved: {0} ({1} captions)", zh: "已保存：{0}（{1} 条字幕）" },

  // ── Font Embed ──────────────────────────────────────────
  btn_analyzing: { en: "Analyzing...", zh: "分析中…" },
  fonts_title: { en: "Detected Fonts", zh: "检测到的字体" },
  fonts_title_count: { en: "Detected Fonts ({0})", zh: "检测到的字体（{0}）" },
  fonts_scanning: { en: "Scanning fonts...", zh: "扫描字体中…" },
  fonts_empty: { en: "No file loaded", zh: "未加载文件" },
  fonts_empty_hint: {
    en: "Select an .ass file to detect fonts used in the subtitle",
    zh: "选择 .ass 文件以检测字幕中使用的字体",
  },
  fonts_glyphs: { en: "— {0} glyphs referenced", zh: "— 引用 {0} 个字形" },
  fonts_found: { en: "Found", zh: "已找到" },
  fonts_missing: { en: "Missing", zh: "缺失" },
  btn_embed: { en: "Embed Selected Fonts ({0})", zh: "嵌入已选字体（{0}）" },
  btn_embed_default: { en: "Embed Fonts", zh: "嵌入字体" },
  btn_embedding: { en: "Embedding...", zh: "嵌入中…" },
  msg_subsetting: { en: "Subsetting {0}...", zh: "子集化 {0}..." },
  msg_font_skipped: { en: "Skipped {0}: {1}", zh: "跳过 {0}：{1}" },
  msg_no_fonts_selected: { en: "No fonts selected for embedding", zh: "未选择需嵌入的字体" },
  msg_embed_saved: {
    en: "Saved: {0} ({1} font(s) embedded)",
    zh: "已保存：{0}（已嵌入 {1} 个字体）",
  },
  fonts_full_embed_warning: {
    en: "Note: Full font files are currently embedded (byte-level subsetting coming in a future update). Output may be large.",
    zh: "注意：当前嵌入完整字体文件（字节级子集化将在后续版本实现），输出文件可能较大",
  },

  // ── Shared ──────────────────────────────────────────────
  btn_select_file: { en: "Select File", zh: "选择文件" },
  btn_clear_file: { en: "Clear", zh: "清除" },
  msg_file_in_use: {
    en: "This file is already loaded in the {0} tab. Clear it there first.",
    zh: "此文件已在「{0}」标签页中加载，请先在该标签页清除。",
  },
  msg_files_skipped_in_use: {
    en: "Skipped {0} file(s) already loaded in other tabs",
    zh: "已跳过 {0} 个在其他标签页中已加载的文件",
  },
  error_prefix: { en: "Error: {0}", zh: "错误：{0}" },
};
