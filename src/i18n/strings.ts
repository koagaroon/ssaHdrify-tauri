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
  app_tagline: { en: "HDR subtitle toolkit", zh: "HDR 字幕工具箱" },
  footer_ready: { en: "Ready", zh: "就绪" },
  file_empty: { en: "No file selected", zh: "未选择文件" },
  hdr_files_title: {
    en: "Selected subtitle files ({0})",
    zh: "已选字幕文件（{0}）",
  },
  nit_target: { en: "Target brightness", zh: "目标亮度" },
  nit_unit: { en: "nits", zh: "尼特" },
  nit_presets_label: { en: "Quick presets", zh: "常用预设" },
  nit_presets_hint: { en: "click to apply a standard", zh: "点击应用标准" },
  nit_interaction_hint: {
    en: "Drag the track · click a preset · ← → adjust by 10 · Shift+← → by 100 · Home/End jump to ends",
    zh: "拖动滑轨 · 点击预设 · ← → 步进 10 · Shift+← → 步进 100 · Home/End 跳两端",
  },
  preset_sdr_desc: { en: "Standard", zh: "标清" },
  preset_bt2408_desc: { en: "Reference", zh: "参考白" },
  preset_hdr10_desc: { en: "Consumer", zh: "消费级" },
  preset_dv_desc: { en: "Dolby Vision", zh: "杜比视界" },
  template_tokens_hint: {
    en: "Placeholders — {name}: input filename without extension · {eotf}: pq or hlg (lowercase)",
    zh: "占位符 — {name}：输入文件名（不含扩展名）· {eotf}：pq 或 hlg（小写）",
  },
  tab_hdr: { en: "HDR Convert", zh: "HDR 转换" },
  tab_hdr_desc: { en: "SDR → HDR color space conversion", zh: "SDR → HDR 色彩空间转换" },
  tab_timing: { en: "Time Shift", zh: "时间轴偏移" },
  tab_timing_desc: { en: "Batch subtitle time adjustment", zh: "批量字幕时间轴调整" },
  tab_fonts: { en: "Font Embed", zh: "字体嵌入" },
  tab_fonts_desc: { en: "Subset & embed fonts into ASS", zh: "字体子集化并嵌入 ASS" },
  // Version label is injected from vite.config.ts at build time — see
  // `resolveAppVersion()`. Do not hardcode a version string here; it will
  // silently drift behind git tags. The template below is the ONLY place
  // this string is assembled.
  footer_version: {
    en: `SSA HDRify ${__APP_VERSION__}`,
    zh: `SSA HDRify ${__APP_VERSION__}`,
  },

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
  btn_select_files: { en: "Select Subtitle File(s)", zh: "选择字幕文件（可多选）" },
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
  msg_skipped_duplicate: {
    en: "Skipped {0}: duplicate output path",
    zh: "已跳过 {0}：输出路径重复",
  },
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
  preview_title: { en: "Preview — {0} captions", zh: "预览 — {0} 条字幕" },
  col_index: { en: "#", zh: "#" },
  col_original: { en: "Original", zh: "原始" },
  col_shifted: { en: "After Shift", zh: "偏移后" },
  col_text: { en: "Text", zh: "原文" },
  threshold_format_hint: {
    en: "Format: HH:MM:SS.ms — hours : minutes : seconds . milliseconds",
    zh: "格式：HH:MM:SS.ms — 时 : 分 : 秒 . 毫秒",
  },
  btn_save_as: { en: "Save As...", zh: "另存为…" },
  msg_saved: { en: "Saved: {0} ({1} captions)", zh: "已保存：{0}（{1} 条字幕）" },

  // ── Font Embed ──────────────────────────────────────────
  btn_analyzing: { en: "Analyzing...", zh: "分析中…" },
  fonts_title: { en: "Detected Fonts", zh: "检测到的字体" },
  fonts_title_count: { en: "Detected Fonts ({0})", zh: "检测到的字体（{0}）" },
  fonts_scanning: { en: "Scanning fonts...", zh: "扫描字体中…" },
  fonts_empty: { en: "No file loaded", zh: "未加载文件" },
  fonts_empty_hint: {
    en: "Select an .ass or .ssa file to detect fonts used in the subtitle",
    zh: "选择 .ass 或 .ssa 文件以检测字幕中使用的字体",
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
    en: "Fonts are subset to only the glyphs used in this subtitle. Safety padding (ASCII + CJK fullwidth) is included automatically.",
    zh: "字体已子集化为仅包含本字幕使用的字形，并自动包含安全填充（ASCII + CJK 全角字符）",
  },
  btn_select_subtitle_file: { en: "Select Subtitle File", zh: "选择字幕文件" },
  btn_select_font_files: {
    en: "Select Font Files / Folder",
    zh: "选择字体文件 / 文件夹",
  },
  btn_select_font_files_with_count: {
    en: "Font Sources ({0})",
    zh: "字体来源（{0}）",
  },
  badge_local: { en: "Local", zh: "本地" },
  badge_system: { en: "System", zh: "系统" },

  // Font source modal
  font_sources_title: { en: "Font Sources", zh: "字体来源" },
  font_sources_empty_hint: {
    en: "No local sources yet. Add a folder or individual files to match fonts without installing them system-wide.",
    zh: "尚未添加本地字体来源。添加文件夹或独立文件即可在不安装字体的情况下完成匹配。",
  },
  font_sources_add_folder: { en: "Add Folder", zh: "添加文件夹" },
  font_sources_add_files: { en: "Add Files", zh: "添加文件" },
  font_sources_folder_entry: {
    en: "{0} ({1} fonts)",
    zh: "{0}（{1} 个字体）",
  },
  font_sources_files_entry: {
    en: "{0} file(s) ({1} fonts)",
    zh: "{0} 个文件（{1} 个字体）",
  },
  font_sources_no_fonts_in_folder: {
    en: "No fonts found in {0}.",
    zh: "{0} 中未找到字体。",
  },
  font_sources_no_fonts_in_files: {
    en: "No fonts found in the {0} selected file(s).",
    zh: "所选 {0} 个文件中未找到字体。",
  },
  font_sources_scanning: { en: "Scanning...", zh: "扫描中…" },
  font_coverage: { en: "Coverage: {0} / {1}", zh: "覆盖进度：{0} / {1}" },
  font_coverage_complete: {
    en: "All required fonts matched",
    zh: "所有所需字体均已匹配",
  },
  font_coverage_missing: { en: "Missing: {0}", zh: "未匹配：{0}" },
  font_coverage_no_subtitle: {
    en: "Load a subtitle file to see match progress.",
    zh: "加载字幕文件后可查看匹配进度。",
  },
  font_sources_close: { en: "Close", zh: "关闭" },
  font_sources_remove: { en: "Remove source", zh: "移除此来源" },
  font_sources_all_duplicate: {
    en: "All fonts from this selection are already loaded.",
    zh: "此选择的字体已全部加载过。",
  },
  font_sources_partial_duplicate: {
    en: "Added {0} new font(s); {1} were already loaded.",
    zh: "新增 {0} 个字体；{1} 个已存在。",
  },
  font_sources_added: {
    en: "Added {0} font(s).",
    zh: "新增 {0} 个字体。",
  },

  // ── Shared ──────────────────────────────────────────────
  btn_select_file: { en: "Select Subtitle File", zh: "选择字幕文件" },
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
