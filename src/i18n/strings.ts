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
  // ── Status indicator (footer) ──────────────────────────
  status_hdr_idle: { en: "No subtitles loaded", zh: "未加载字幕" },
  status_hdr_pending: {
    en: "Ready to convert · {0} file(s)",
    zh: "可转换 · {0} 个文件",
  },
  status_hdr_busy: { en: "Converting…", zh: "转换中…" },
  status_hdr_done: { en: "Conversion complete", zh: "转换完成" },
  status_hdr_error: { en: "Conversion failed", zh: "转换失败" },
  status_hdr_cancelled: { en: "Conversion cancelled", zh: "已取消转换" },
  status_timing_idle: { en: "No subtitles loaded", zh: "未加载字幕" },
  status_timing_pending: { en: "Adjust offset, then save", zh: "调整偏移后保存" },
  status_timing_busy: { en: "Saving…", zh: "保存中…" },
  status_timing_done: { en: "Save complete", zh: "保存完成" },
  status_timing_error: { en: "Save failed", zh: "保存失败" },
  status_timing_cancelled: { en: "Save cancelled", zh: "已取消保存" },
  status_fonts_idle: { en: "No subtitle loaded", zh: "未加载字幕" },
  status_fonts_analyzing: { en: "Analyzing fonts…", zh: "分析字体中…" },
  status_fonts_pick: { en: "Pick fonts to embed", zh: "选择要嵌入的字体" },
  status_fonts_pending: {
    en: "Ready to embed · {0} font(s)",
    zh: "可嵌入 · {0} 个字体",
  },
  status_fonts_busy: { en: "Embedding…", zh: "嵌入中…" },
  status_fonts_done: { en: "Fonts embedded", zh: "字体已嵌入" },
  status_fonts_error: { en: "Embed failed", zh: "嵌入失败" },
  status_fonts_cancelled: { en: "Embed cancelled", zh: "已取消嵌入" },
  status_fonts_batch_pending: {
    en: "Ready to embed · {0} font(s) across {1} file(s)",
    zh: "可嵌入 · 跨 {1} 个文件的 {0} 个字体",
  },
  file_empty: { en: "No file selected", zh: "未选择文件" },
  files_selected_title: {
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
  tab_timing: { en: "Time Shift", zh: "时间轴偏移" },
  tab_fonts: { en: "Font Embed", zh: "字体嵌入" },
  tab_rename: { en: "Batch Rename", zh: "批量重命名" },
  // Titlebar window controls — localized so screen readers and tooltip
  // hovers stay consistent with the app's current language.
  titlebar_minimize: { en: "Minimize", zh: "最小化" },
  titlebar_maximize: { en: "Maximize", zh: "最大化" },
  titlebar_close: { en: "Close", zh: "关闭" },
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
  template_custom: { en: "Custom…", zh: "自定义…" },
  style_settings: { en: "Style Settings", zh: "样式设置" },
  style_hint: { en: "(SRT/SUB input only)", zh: "（仅 SRT/SUB 输入）" },
  style_font: { en: "Font", zh: "字体" },
  style_font_placeholder: { en: "Font family name", zh: "字体族名称" },
  style_size: { en: "Size", zh: "字号" },
  style_primary_color: { en: "Primary Color", zh: "主要颜色" },
  style_outline_color: { en: "Outline Color", zh: "描边颜色" },
  style_outline_width: { en: "Outline Width", zh: "描边宽度" },
  style_shadow_depth: { en: "Shadow Depth", zh: "阴影深度" },
  style_fps: { en: "FPS (SUB only)", zh: "帧率（仅 SUB）" },
  style_font_custom: { en: "Custom…", zh: "自定义…" },
  btn_select_files: { en: "Select Subtitle File(s)", zh: "选择字幕文件（可多选）" },
  btn_convert: { en: "Convert", zh: "转换" },
  btn_converting: { en: "Converting…", zh: "转换中…" },
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
  msg_no_subtitle_in_drop: {
    en: "No supported subtitle files in the dropped items",
    zh: "拖入的内容中没有支持的字幕文件",
  },
  hdr_drop_hint: {
    en: "Tip: drag subtitle files or a folder onto the file strip above (videos in the folder are skipped automatically)",
    zh: "提示：可将字幕文件或文件夹拖到上方文件栏（文件夹内的视频会自动忽略）",
  },
  msg_overwrite_confirm: {
    en: "{0} of {1} output file(s) already exist. Overwrite them?",
    zh: "{0}/{1} 个输出文件已存在，确认覆盖？",
  },
  dialog_overwrite_title: { en: "Confirm Overwrite", zh: "确认覆盖" },
  dialog_filter_ass_ssa_subtitles: { en: "ASS/SSA Subtitles", zh: "ASS/SSA 字幕" },
  dialog_filter_srt_subtitles: { en: "SRT Subtitles", zh: "SRT 字幕" },
  dialog_filter_sub_subtitles: { en: "SUB (MicroDVD)", zh: "SUB（MicroDVD）" },
  dialog_filter_webvtt: { en: "WebVTT", zh: "WebVTT" },
  dialog_filter_all_subtitle_formats: { en: "All Subtitle Formats", zh: "所有字幕格式" },
  dialog_filter_all_files: { en: "All Files", zh: "所有文件" },
  dialog_filter_font_files: { en: "Font Files", zh: "字体文件" },
  dialog_filter_video_subtitle_files: { en: "Video & Subtitle Files", zh: "视频和字幕文件" },
  dialog_filter_video_files: { en: "Video Files", zh: "视频文件" },
  dialog_filter_subtitle_files: { en: "Subtitle Files", zh: "字幕文件" },
  dialog_pick_subtitle_files_title: { en: "Select subtitle files", zh: "选择字幕文件" },
  dialog_pick_ass_files_title: { en: "Select ASS/SSA files", zh: "选择 ASS/SSA 文件" },
  dialog_pick_rename_inputs_title: {
    en: "Select videos and subtitles",
    zh: "选择视频和字幕",
  },
  dialog_pick_output_directory_title: { en: "Choose output directory", zh: "选择输出文件夹" },
  dialog_pick_font_directory_title: { en: "Select font folder", zh: "选择字体文件夹" },
  dialog_pick_font_files_title: { en: "Select font files", zh: "选择字体文件" },

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
  threshold_exceeds_file: {
    en: "Threshold is past the last caption — nothing will shift",
    zh: "阈值超过最后一条字幕 — 不会发生偏移",
  },
  btn_save_as: { en: "Save As…", zh: "另存为…" },
  btn_save: { en: "Save", zh: "保存" },
  btn_save_all: { en: "Save All ({0})", zh: "全部保存（{0}）" },
  msg_saved: { en: "Saved: {0} ({1} captions)", zh: "已保存：{0}（{1} 条字幕）" },
  msg_timing_start: {
    en: "Starting save: {0} file(s), offset {1} ms",
    zh: "开始保存：{0} 个文件，偏移 {1} 毫秒",
  },
  msg_timing_complete: {
    en: "Save complete: {0}/{1} file(s) processed",
    zh: "保存完成：已处理 {0}/{1} 个文件",
  },
  msg_timing_cancelled: { en: "Save cancelled.", zh: "已取消保存。" },
  msg_timing_error: { en: "Error saving {0}: {1}", zh: "保存 {0} 出错：{1}" },
  preview_title_first: {
    en: "Preview — {0} captions ({1})",
    zh: "预览 — {1} 的 {0} 条字幕",
  },
  timing_drop_hint: {
    en: "Tip: drag subtitle files or a folder onto the file strip above (videos in the folder are skipped automatically)",
    zh: "提示：可将字幕文件或文件夹拖到上方文件栏（文件夹内的视频会自动忽略）",
  },

  // ── Font Embed ──────────────────────────────────────────
  btn_analyzing: { en: "Analyzing…", zh: "分析中…" },
  fonts_title: { en: "Detected Fonts", zh: "检测到的字体" },
  fonts_title_count: { en: "Detected Fonts ({0})", zh: "检测到的字体（{0}）" },
  fonts_title_count_batch: {
    en: "Detected Fonts ({0} unique across {1} files)",
    zh: "检测到的字体（{1} 个文件中 {0} 个独立字体）",
  },
  fonts_scanning: { en: "Scanning fonts…", zh: "扫描字体中…" },
  fonts_empty: { en: "No file loaded", zh: "未加载文件" },
  fonts_empty_hint: {
    en: "Select an .ass or .ssa file to detect fonts used in the subtitle",
    zh: "选择 .ass 或 .ssa 文件以检测字幕中使用的字体",
  },
  fonts_glyphs: { en: "— {0} glyphs referenced", zh: "— 引用 {0} 个字形" },
  col_font_name: { en: "Name", zh: "字体" },
  col_font_glyphs: { en: "Glyphs", zh: "字形数" },
  col_font_source: { en: "Source", zh: "来源" },
  col_font_status: { en: "Status", zh: "状态" },
  fonts_found: { en: "Found", zh: "已找到" },
  fonts_missing: { en: "Missing", zh: "缺失" },
  btn_embed: { en: "Embed Selected Fonts ({0})", zh: "嵌入已选字体（{0}）" },
  btn_embed_default: { en: "Embed Fonts", zh: "嵌入字体" },
  msg_fonts_start: {
    en: "Starting embed: {0} file(s)",
    zh: "开始嵌入：{0} 个文件",
  },
  msg_fonts_complete: {
    en: "Embed complete: {0}/{1} file(s) processed",
    zh: "嵌入完成：已处理 {0}/{1} 个文件",
  },
  msg_fonts_cancelled: { en: "Embed cancelled.", zh: "已取消嵌入。" },
  msg_fonts_error: { en: "Error embedding {0}: {1}", zh: "嵌入 {0} 出错：{1}" },
  fonts_drop_hint: {
    en: "Tip: drag .ass / .ssa files or a folder onto the file strip above (other files in the folder are skipped automatically)",
    zh: "提示：可将 .ass / .ssa 文件或文件夹拖到上方文件栏（文件夹内其他类型文件会自动忽略）",
  },
  btn_embedding: { en: "Embedding…", zh: "嵌入中…" },
  msg_subsetting: { en: "Subsetting {0}…", zh: "子集化 {0}…" },
  msg_font_skipped: { en: "Skipped {0}: {1}", zh: "跳过 {0}：{1}" },
  msg_no_fonts_selected: { en: "No fonts selected for embedding", zh: "未选择需嵌入的字体" },
  msg_embed_saved: {
    en: "Saved: {0} ({1} font(s) embedded)",
    zh: "已保存：{0}（已嵌入 {1} 个字体）",
  },
  msg_embed_no_change: {
    en: "Skipped {0} — no fonts were embedded (output would equal input)",
    zh: "跳过 {0} — 未嵌入任何字体（输出与输入相同，未写文件）",
  },
  fonts_full_embed_warning: {
    en: "Fonts are subset to only the glyphs used in this subtitle. Safety padding (ASCII + CJK fullwidth) is included automatically.",
    zh: "字体已子集化为仅包含本字幕使用的字形，并自动包含安全填充（ASCII + CJK 全角字符）",
  },
  btn_select_font_files: {
    en: "Select Font Files / Folder",
    zh: "选择字体文件 / 文件夹",
  },
  btn_select_font_files_with_count: {
    en: "Font Sources ({0})",
    zh: "字体来源（{0}）",
  },
  font_sources_loaded_summary: {
    en: "{0} local font(s) loaded from {1} source(s)",
    zh: "已从 {1} 个来源加载 {0} 个本地字体",
  },
  badge_local: { en: "Local", zh: "本地" },
  badge_system: { en: "System", zh: "系统" },

  // Font source modal
  font_sources_title: { en: "Font Sources", zh: "字体来源" },
  font_sources_modal_sub: {
    en: "Pick a folder or individual files — duplicates are filtered automatically",
    zh: "可选择文件夹或单独文件，重复项会自动过滤",
  },
  font_sources_add_folder_sub: {
    en: "Scan top-level font files in a folder",
    zh: "扫描文件夹第一层的字体文件",
  },
  font_sources_add_files_sub: {
    en: "Pick one or more individual font files",
    zh: "选择一个或多个字体文件",
  },
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
  font_sources_scanning: { en: "Scanning…", zh: "扫描中…" },
  font_scan_progress: {
    en: "Scanned {0} fonts so far…",
    zh: "已扫描 {0} 个字体…",
  },
  font_scan_cancel: { en: "Cancel", zh: "取消" },
  font_scan_cancelling: { en: "Cancelling…", zh: "正在取消…" },
  font_scan_cancel_failed: {
    en: "Could not request cancellation: {0}",
    zh: "取消请求失败：{0}",
  },
  font_scan_cancelled: {
    en: "Scan cancelled — kept {0} font(s).",
    zh: "已取消扫描，保留 {0} 个字体。",
  },
  font_scan_cancelled_with_dupes: {
    en: "Scan cancelled — kept {0} new font(s); {1} were already loaded.",
    zh: "已取消扫描，保留 {0} 个新字体；{1} 个为已加载的重复项。",
  },
  font_scan_ceiling_hit: {
    en: "Source too large — kept the first {0} font(s).",
    zh: "字体来源过大，仅保留前 {0} 个字体。",
  },
  font_scan_large_warning_title: { en: "Large Font Source", zh: "大型字体来源" },
  font_scan_large_warning: {
    en: "This selection contains about {0} font file(s) ({1}). Scanning may take time. SSA HDRify will store the source index on disk to reduce memory use. Continue?",
    zh: "此选择约包含 {0} 个字体文件（{1}）。扫描可能需要一些时间。SSA HDRify 会将来源索引暂存到磁盘以降低内存占用。继续吗？",
  },
  font_coverage: { en: "Local source coverage: {0} / {1}", zh: "本地来源覆盖：{0} / {1}" },
  font_coverage_complete: {
    en: "All required fonts covered locally",
    zh: "所需字体均已本地覆盖",
  },
  font_coverage_missing: {
    en: "Not in local sources: {0}",
    zh: "本地来源未收录：{0}",
  },
  font_coverage_hint: {
    en: "Missing fonts may still match via installed system fonts — check the Detected Fonts list.",
    zh: "未被本地来源收录的字体仍可能通过已安装的系统字体匹配 — 请查看主面板「检测到的字体」。",
  },
  font_coverage_no_subtitle: {
    en: "Load a subtitle file to see match progress.",
    zh: "加载字幕文件后可查看匹配进度。",
  },
  font_sources_close: { en: "Close", zh: "关闭" },
  font_sources_remove: { en: "Remove source", zh: "移除此来源" },
  btn_clear_font_sources: { en: "Clear all font sources", zh: "清除所有字体来源" },
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
  msg_dedup_blocked: {
    en: "Can't load — {0} file(s) already loaded in the {1} tab. Clear them there first.",
    zh: "无法加载 — {0} 个文件已在「{1}」标签页中。请先在该标签页中清除。",
  },
  error_prefix: { en: "Error: {0}", zh: "错误：{0}" },

  // ── Batch Rename (Tab 4) ────────────────────────────────
  status_rename_idle: { en: "No files loaded", zh: "未加载文件" },
  status_rename_pending: {
    en: "{0} video(s) · {1} subtitle(s)",
    zh: "{0} 个视频 · {1} 个字幕",
  },
  status_rename_busy: { en: "Renaming…", zh: "重命名中…" },
  status_rename_done: { en: "Rename complete", zh: "重命名完成" },
  status_rename_error: { en: "Rename failed", zh: "重命名失败" },
  status_rename_cancelled: { en: "Rename cancelled", zh: "已取消重命名" },
  status_rename_noop: {
    en: "Nothing changed — files already match",
    zh: "未做改动 — 文件已与视频同名",
  },
  rename_manual_edit_hint: {
    en: "Tip: pick a different subtitle from any row's dropdown to re-pair. ↺ Reset undoes all manual edits.",
    zh: "提示：从下拉框中选择其他字幕即可重新配对。↺ 重置 撤销全部手动改动。",
  },
  rename_pick_subtitle: {
    en: "Pick subtitle",
    zh: "选择字幕",
  },
  rename_pick_subtitle_none: {
    en: "— none —",
    zh: "— 无 —",
  },
  rename_reset_pairings: {
    en: "Reset",
    zh: "重置",
  },
  rename_reset_pairings_hint: {
    en: "Restore the engine's automatic pairing, discarding manual edits",
    zh: "恢复引擎自动配对，丢弃手动改动",
  },
  rename_drop_hint: {
    en: "Tip: drag video and subtitle files (or a whole show folder) onto the file strip above — videos and subs auto-categorize",
    zh: "提示：可将视频与字幕文件（或整个剧集文件夹）拖到上方文件栏 — 视频与字幕自动归类",
  },
  rename_chip_videos: { en: "{0} videos", zh: "{0} 个视频" },
  rename_chip_subtitles: { en: "{0} subtitles", zh: "{0} 个字幕" },
  rename_chip_unknown: { en: "{0} unknown", zh: "{0} 个未识别" },
  rename_chip_unknown_hint: {
    en: "Files without a video or subtitle extension are excluded from pairing",
    zh: "扩展名既非视频也非字幕的文件不参与配对",
  },
  btn_select_rename_inputs: {
    en: "Select Videos & Subtitles",
    zh: "选择视频与字幕",
  },
  msg_no_rename_inputs_in_drop: {
    en: "No videos or subtitles found in the dropped items",
    zh: "拖入的内容中未找到视频或字幕",
  },
  msg_rename_unknown_skipped: {
    en: "Skipped {0} file(s) — neither video nor subtitle",
    zh: "已跳过 {0} 个文件 — 既非视频也非字幕",
  },
  rename_grid_title: {
    en: "Pairing preview · {0} row(s)",
    zh: "配对预览 · {0} 行",
  },
  rename_grid_warning_suffix: {
    en: "{0} warning(s)",
    zh: "{0} 个警告",
  },
  rename_no_pairings: {
    en: "No pairings yet — load videos and subtitles to begin.",
    zh: "暂无配对 — 加载视频与字幕后开始。",
  },
  rename_col_video: { en: "Video", zh: "视频" },
  rename_col_subtitle: { en: "Subtitle", zh: "字幕" },
  rename_col_source: { en: "Source", zh: "来源" },
  rename_source_regex: { en: "regex", zh: "正则" },
  rename_source_lcs: { en: "LCS", zh: "LCS" },
  rename_source_manual: { en: "manual", zh: "手动" },
  rename_source_unmatched: { en: "—", zh: "—" },
  rename_source_warning: { en: "warning", zh: "冲突" },

  // ── Output-mode + run flow (Stage 5c) ────────────────────
  rename_row_select_aria: { en: "Select this pair", zh: "选中此对" },
  rename_mode_label: { en: "Output mode", zh: "输出方式" },
  rename_mode_copy_to_video: {
    en: "Copy to video directory",
    zh: "复制到视频所在目录",
  },
  rename_mode_copy_to_chosen: {
    en: "Copy to a chosen directory",
    zh: "复制到指定目录",
  },
  rename_mode_in_place: { en: "Rename in place", zh: "原地重命名" },
  rename_mode_default: { en: "(default)", zh: "（默认）" },
  rename_mode_in_place_hint: {
    en: "destructive — original subtitle filename is replaced",
    zh: "破坏性 — 原字幕文件名将被替换",
  },
  rename_mode_rename_short: { en: "rename", zh: "原地改名" },
  rename_mode_copy_to_video_short: { en: "copy → video dir", zh: "复制 → 视频目录" },
  rename_mode_copy_to_chosen_short: { en: "copy → chosen dir", zh: "复制 → 指定目录" },
  btn_pick_chosen_dir: {
    en: "Choose folder…",
    zh: "选择文件夹…",
  },
  rename_chosen_dir_empty: {
    en: "No folder chosen yet",
    zh: "尚未选择文件夹",
  },
  btn_rename_run: { en: "Run ({0})", zh: "执行（{0}）" },
  btn_renaming: { en: "Running…", zh: "执行中…" },
  msg_rename_no_chosen_dir: {
    en: "Choose an output folder first.",
    zh: "请先选择输出文件夹。",
  },
  msg_rename_skipped: {
    en: "Skipped {0}: {1}",
    zh: "已跳过 {0}：{1}",
  },
  msg_rename_skipped_count: {
    // Pinpointed to derive-time pairing failures specifically — not
    // covering noopTargets (already-correctly-named subs), within-batch
    // dedup skips, or loop-time copy/rename errors. Those happen before
    // or after this dialog is shown.
    en: "Note: {0} pairing(s) failed earlier and won't run (see log).",
    zh: "注意：另有 {0} 对配对早前失败、不会执行（见日志）。",
  },
  msg_rename_nothing_to_do: {
    en: "Nothing to do — all selected rows produced invalid output paths.",
    zh: "无可执行项 — 所选行的输出路径均无效。",
  },
  msg_rename_inplace_confirm: {
    en: "Rename {0} subtitle file(s) in place? This replaces the original filename.",
    zh: "原地重命名 {0} 个字幕文件？原文件名将被替换。",
  },
  msg_rename_inplace_more: {
    en: "…and {0} more",
    zh: "……及其他 {0} 个",
  },
  dialog_rename_inplace_title: {
    en: "Confirm Rename",
    zh: "确认重命名",
  },
  msg_rename_cancelled: { en: "Rename cancelled.", zh: "已取消重命名。" },
  msg_rename_start: {
    en: "Starting: {0} file(s) · {1}",
    zh: "开始执行：{0} 个文件 · {1}",
  },
  msg_rename_done: {
    en: "{0} → {1}",
    zh: "{0} → {1}",
  },
  msg_rename_error: {
    en: "Error renaming {0}: {1}",
    zh: "重命名 {0} 出错：{1}",
  },
  msg_rename_complete: {
    en: "Rename complete: {0}/{1} file(s) processed",
    zh: "重命名完成：已处理 {0}/{1} 个文件",
  },
  msg_rename_already_named: {
    en: "Already correctly named: {0} (skipped no-op)",
    zh: "已是目标名：{0}（无需操作）",
  },
  msg_rename_all_already_named: {
    en: "All {0} subtitle file(s) already match their videos — nothing to do.",
    zh: "全部 {0} 个字幕文件已与视频同名 — 无需操作。",
  },
};
