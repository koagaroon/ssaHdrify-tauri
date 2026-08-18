/**
 * All user-facing strings for en/zh i18n.
 *
 * Key naming: snake_case, grouped by feature prefix.
 * Parametric strings use {0}, {1}, ... placeholders.
 */

export const LANGS = ["en", "zh"] as const;
export type Lang = (typeof LANGS)[number];

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
  status_fonts_noop: { en: "Nothing to embed", zh: "无需嵌入" },
  status_fonts_partial: { en: "Fonts embedded with warnings", zh: "字体已嵌入，但有警告" },
  status_fonts_error: { en: "Embed failed", zh: "嵌入失败" },
  status_fonts_cancelled: { en: "Embed cancelled", zh: "已取消嵌入" },
  status_fonts_batch_pending: {
    en: "Ready to embed · {0} font(s) across {1} file(s)",
    zh: "可嵌入 · 跨 {1} 个文件的 {0} 个字体",
  },
  status_style_idle: { en: "No ASS/SSA files loaded", zh: "未加载 ASS/SSA 文件" },
  status_style_analyzing: { en: "Analyzing styles…", zh: "正在分析样式…" },
  status_style_needs_operation: {
    en: "Enable at least one style operation",
    zh: "请至少启用一项样式操作",
  },
  status_style_invalid_operation: {
    en: "Fix the highlighted style operation",
    zh: "请修正标出的样式操作",
  },
  status_style_preview_error: {
    en: "Some files cannot be analyzed",
    zh: "部分文件无法分析",
  },
  status_style_pending: {
    en: "Ready to write · {0} style change(s) across {1} file(s)",
    zh: "可写入 · {1} 个文件中有 {0} 项样式更改",
  },
  status_style_busy: { en: "Writing style edits…", zh: "正在写入样式更改…" },
  status_style_done: { en: "Style edits complete", zh: "样式编辑完成" },
  status_style_partial: {
    en: "Style edits completed with issues",
    zh: "样式编辑已完成，但有问题",
  },
  status_style_error: { en: "Style edit failed", zh: "样式编辑失败" },
  status_style_cancelled: { en: "Style edit cancelled", zh: "已取消样式编辑" },
  status_style_noop: { en: "Nothing to change", zh: "无需更改" },
  msg_style_output_probe_error: {
    en: "Could not check output {0}: {1}",
    zh: "无法检查输出文件 {0}：{1}",
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
    // list every supported token. Custom templates now
    // throw at conversion time for unknown tokens (post-bb85bd9
    // substituteTemplate strict mode), so the user needs the full
    // list to avoid surprises. `{lang}` auto-extracts from filename
    // suffix (.zh.ass etc.); `{video_name}` is meaningful only when
    // paired with a video (otherwise empty).
    en: "Placeholders — {name}: filename without extension · {eotf}: pq / hlg · {lang}: language tag from filename · {video_name}: paired video stem (empty if unpaired)",
    zh: "占位符 — {name}：输入文件名（不含扩展名）· {eotf}：pq / hlg · {lang}：从文件名后缀提取的语言标签 · {video_name}：配对视频文件名（未配对时为空）",
  },
  tab_hdr: { en: "HDR Convert", zh: "HDR 转换" },
  tab_timing: { en: "Time Shift", zh: "时间轴偏移" },
  tab_fonts: { en: "Font Embed", zh: "字体嵌入" },
  tab_rename: { en: "Batch Rename", zh: "批量重命名" },
  tab_style: { en: "Style Edit", zh: "样式编辑" },
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
  licenses_open: { en: "Licenses", zh: "许可证" },
  licenses_title: { en: "About & Licenses", zh: "关于与许可证" },
  licenses_intro: {
    en: "Offline copies of the application license and notices for bundled fonts and adapted interface icons.",
    zh: "应用许可证、捆绑字体及改编界面图标声明的离线副本。",
  },
  licenses_exact_text_note: {
    en: "Legal texts below are embedded verbatim in English. Chinese UI text is an informal navigation aid, not a legal translation.",
    zh: "下列英文法律文本均为逐字嵌入；中文界面文字仅用于辅助阅读，不构成法律翻译。",
  },
  licenses_close: { en: "Close licenses", zh: "关闭许可证" },
  licenses_source: { en: "Source", zh: "来源" },
  licenses_full_text: { en: "Full license text", zh: "完整许可证正文" },
  licenses_ssahdrify_summary: {
    en: "Copyright © 2021 ying, 2024–2025 gky99, and 2026 koagaroon. SSA HDRify is licensed under GPL-3.0-or-later and comes with absolutely no warranty; see the full terms below.",
    zh: "版权所有 © 2021 ying、2024–2025 gky99、2026 koagaroon。SSA HDRify 以 GPL-3.0-or-later 授权，且不附带任何保证；完整条款见下方。",
  },
  licenses_inter_summary: {
    en: "Bundled interface font. Copyright © 2016 The Inter Project Authors. The bundled license declares no Reserved Font Name.",
    zh: "捆绑界面字体。版权所有 © 2016 The Inter Project Authors。随附许可证未声明保留字体名称。",
  },
  licenses_smiley_sans_summary: {
    en: "Bundled display font. Copyright © 2022–2024 atelierAnchor. Reserved Font Names: Smiley and 得意黑.",
    zh: "捆绑展示字体。版权所有 © 2022–2024 atelierAnchor。保留字体名称：Smiley、得意黑。",
  },
  licenses_feather_summary: {
    en: "Some interface glyphs are adapted from Feather Icons. Copyright © 2013–2023 Cole Bemis.",
    zh: "部分界面图标改编自 Feather Icons。版权所有 © 2013–2023 Cole Bemis。",
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
  template_required: {
    en: "Enter a custom output template.",
    zh: "请输入自定义输出模板。",
  },
  style_settings: { en: "Style Settings", zh: "样式设置" },
  style_hint: { en: "(SRT/SUB/VTT input only)", zh: "（仅 SRT/SUB/VTT 输入）" },
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
  btn_cancelling: { en: "Cancelling…", zh: "正在取消…" },
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
  msg_inferred_utf16: {
    en: "{0}: detected BOM-less {1} from its byte pattern. This is a best-effort guess; verify the preview or output.",
    zh: "已根据字节模式将 {0} 推测为无 BOM 的 {1} 编码。此结果并非完全确定，请核对预览或输出。",
  },
  msg_unsupported: { en: "Skipped {0}: unsupported format", zh: "已跳过 {0}：不支持的格式" },
  // HDR Convert and Time Shift both surface a per-file count of
  // Rebuilding writers cannot safely transform captions above the
  // MAX_CAPTION_TEXT_LEN (64 KB) cap. Structure-preserving writers use the
  // sibling unchanged warning instead of claiming the source cue was dropped.
  msg_oversized_skipped: {
    en: "Dropped {0} oversized caption(s) from {1}: text exceeded 64 KB per-caption cap",
    zh: "{1} 中有 {0} 条超大字幕（单条 64 KB 上限）已丢弃",
  },
  msg_oversized_unchanged: {
    en: "Left {0} oversized caption(s) in {1} unchanged: text exceeded the 64 KB per-caption processing cap",
    zh: "{1} 中有 {0} 条超大字幕超过单条 64 KB 的处理上限，已保留原内容且未调整时间",
  },
  msg_done: { en: "Done: {0}", zh: "完成：{0}" },
  msg_convert_error: { en: "Error converting {0}: {1}", zh: "转换 {0} 出错：{1}" },
  msg_complete: {
    en: "Conversion complete: {0}/{1} file(s) processed",
    zh: "转换完成：已处理 {0}/{1} 个文件",
  },
  msg_all_failed: {
    en: "Conversion failed: all {0} file(s) errored",
    zh: "转换失败：{0} 个文件全部出错",
  },
  msg_cancelled: { en: "Conversion cancelled.", zh: "转换已取消。" },
  msg_no_subtitle_in_drop: {
    en: "No supported subtitle files in the dropped items",
    zh: "拖入的内容中没有支持的字幕文件",
  },
  err_batch_too_many_files: {
    en: "Batch too large: {0} files exceeds the {1}-file safety cap. Split the selection.",
    zh: "批量过大：选择了 {0} 个文件，超过 {1} 的安全上限。请分批处理。",
  },
  err_batch_aggregate_too_large: {
    en: "Batch content exceeds the {1} MB safety cap (reached {0} MB). Split the selection.",
    zh: "批量内容超过 {1} MB 安全上限（已达 {0} MB）。请分批处理。",
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
  preview_title_truncated: {
    en: "Preview — first {0} of {1} captions",
    zh: "预览 — 共 {1} 条字幕，显示前 {0} 条",
  },
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
  msg_timing_all_failed: {
    en: "Save failed: all {0} file(s) errored",
    zh: "保存失败：{0} 个文件全部出错",
  },
  msg_timing_cancelled: { en: "Save cancelled.", zh: "已取消保存。" },
  msg_timing_error: { en: "Error saving {0}: {1}", zh: "保存 {0} 出错：{1}" },
  preview_title_first: {
    en: "Preview — {0} captions ({1})",
    zh: "预览 — {1} 的 {0} 条字幕",
  },
  preview_title_first_truncated: {
    en: "Preview — first {0} of {1} captions ({2})",
    zh: "预览 — {2} 共 {1} 条字幕，显示前 {0} 条",
  },
  timing_preview_error: {
    en: "Preview unavailable: {0}",
    zh: "无法生成预览：{0}",
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
  fonts_output_beside_input: { en: "Save beside input", zh: "保存到源文件旁" },
  fonts_output_chosen_dir: { en: "Save to chosen folder", zh: "保存到指定文件夹" },
  msg_fonts_start: {
    en: "Starting embed: {0} file(s)",
    zh: "开始嵌入：{0} 个文件",
  },
  msg_fonts_complete: {
    en: "Embed complete: {0}/{1} file(s) processed",
    zh: "嵌入完成：已处理 {0}/{1} 个文件",
  },
  msg_fonts_complete_mixed: {
    en: "Embed complete: {0} file(s) written, {1} unchanged",
    zh: "嵌入完成：已写入 {0} 个文件，{1} 个文件无需更改",
  },
  msg_fonts_complete_partial: {
    en: "Embed incomplete: {0}/{1} file(s) written, {2} issue(s)",
    zh: "嵌入不完整：已写入 {0}/{1} 个文件，{2} 个问题",
  },
  msg_fonts_complete_partial_mixed: {
    en: "Embed incomplete: {0} file(s) written, {1} unchanged, {2} issue(s)",
    zh: "嵌入不完整：已写入 {0} 个文件，{1} 个文件无需更改，{2} 个问题",
  },
  msg_fonts_skipped_count: {
    en: "Note: {0} file(s) were skipped before this prompt (see log).",
    zh: "注意：另有 {0} 个文件已在此提示前跳过（见日志）。",
  },
  msg_fonts_all_failed: {
    en: "Embed failed on all {0} file(s); see errors above",
    zh: "全部 {0} 个文件嵌入失败，详见上方错误",
  },
  msg_fonts_all_no_change: {
    en: "Nothing to embed — all referenced fonts are already present in {0} file(s)",
    zh: "无需嵌入 —— {0} 个文件引用的字体均已存在",
  },
  msg_fonts_cancelled: { en: "Embed cancelled.", zh: "已取消嵌入。" },
  msg_fonts_error: { en: "Error embedding {0}: {1}", zh: "嵌入 {0} 出错：{1}" },
  msg_fonts_analysis_unavailable: {
    en: "Analysis data is unavailable. Select the files again.",
    zh: "分析数据已不可用，请重新选择文件。",
  },
  msg_fonts_file_warning: { en: "{0}: {1}", zh: "{0}：{1}" },
  msg_fonts_missing_warning: {
    en: "{0} referenced font(s) were missing and were not embedded",
    zh: "有 {0} 个引用字体缺失，未被嵌入",
  },
  fonts_drop_hint: {
    en: "Tip: drag .ass / .ssa files or a folder onto the file strip above (other files in the folder are skipped automatically)",
    zh: "提示：可将 .ass / .ssa 文件或文件夹拖到上方文件栏（文件夹内其他类型文件会自动忽略）",
  },
  btn_embedding: { en: "Embedding…", zh: "嵌入中…" },
  font_style_bold: { en: "Bold", zh: "粗体" },
  font_style_italic: { en: "Italic", zh: "斜体" },
  msg_subsetting: { en: "Subsetting {0}…", zh: "子集化 {0}…" },
  msg_font_skipped: { en: "Skipped {0}: {1}", zh: "跳过 {0}：{1}" },
  msg_no_fonts_for_file: {
    en: "Skipped {0} — none of the selected fonts are referenced by this file",
    zh: "跳过 {0} — 本文件未引用任何已选字体",
  },
  msg_drop_truncated: {
    en: "Drop too large — first {0} files accepted, the rest were ignored. Retry with a smaller batch.",
    zh: "拖入过大 — 仅接受前 {0} 个文件，其余已忽略。请缩小批量后重试。",
  },
  msg_drop_no_usable: {
    en: "Drop expanded to zero usable paths",
    zh: "拖入未展开出任何可用路径",
  },
  msg_embed_saved: {
    en: "Saved: {0} ({1} font(s) embedded)",
    zh: "已保存：{0}（已嵌入 {1} 个字体）",
  },
  msg_embed_saved_partial: {
    en: "Saved with warnings: {0} ({1} font(s) embedded, {2} warning(s))",
    zh: "已保存但有警告：{0}（已嵌入 {1} 个字体，{2} 条警告）",
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
    en: "{0} font entries across {1} source(s) (shared fonts count in each source)",
    zh: "{1} 个字体来源中共有 {0} 个字体条目（共享字体会在每个来源中分别计数）",
  },
  badge_local: { en: "Local", zh: "本地" },
  badge_cache: { en: "Cache", zh: "缓存" },
  badge_system: { en: "System", zh: "系统" },

  // Font source modal
  font_sources_title: { en: "Font Sources", zh: "字体来源" },
  font_sources_modal_sub: {
    en: "Pick a top-level folder, a nested font library, or individual files — duplicates are filtered automatically",
    zh: "可选择仅第一层文件夹、包含子文件夹的字体库或单独文件，重复项会自动过滤",
  },
  font_sources_add_folder_sub: {
    en: "Scan top-level font files in a folder",
    zh: "扫描文件夹第一层的字体文件",
  },
  font_sources_add_files_sub: {
    en: "Pick one or more individual font files",
    zh: "选择一个或多个字体文件",
  },
  font_sources_add_library_sub: {
    en: "Scan font files in this folder and all subfolders",
    zh: "扫描此文件夹及其所有子文件夹中的字体文件",
  },
  font_sources_empty_hint: {
    en: "No local sources yet. Add a folder, a font library, or individual files to match fonts without installing them system-wide.",
    zh: "尚未添加本地字体来源。添加文件夹、字体库或独立文件，即可在不安装字体的情况下完成匹配。",
  },
  font_sources_add_folder: { en: "Add Folder", zh: "添加文件夹" },
  font_sources_add_library: { en: "Add Font Library", zh: "添加字体库" },
  font_sources_add_files: { en: "Add Files", zh: "添加文件" },
  font_sources_folder_entry: {
    en: "{0} (top level · {1} fonts)",
    zh: "{0}（仅第一层 · {1} 个字体）",
  },
  font_sources_library_entry: {
    en: "{0} (folder + all subfolders · {1} fonts)",
    zh: "{0}（本文件夹及所有子文件夹 · {1} 个字体）",
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
  font_scan_inspecting_folders: {
    en: "Inspecting font folders…",
    zh: "正在检查字体文件夹…",
  },
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
    en: "Scan cancelled — kept {0} font(s) for this session; this source was not cached.",
    zh: "已取消扫描——本次会话保留 {0} 个字体，但未缓存此来源。",
  },
  font_scan_cancelled_with_dupes: {
    en: "Scan cancelled — kept {0} new font(s) for this session; {1} were already loaded. This source was not cached.",
    zh: "已取消扫描——本次会话保留 {0} 个新字体；{1} 个为已加载的重复项。此来源未缓存。",
  },
  font_scan_inspection_cancelled: {
    en: "Folder inspection cancelled — no fonts were loaded.",
    zh: "已取消文件夹检查，尚未加载字体。",
  },
  font_scan_ceiling_hit: {
    en: "Source too large — kept the first {0} font(s) for this session; this source was not cached.",
    zh: "字体来源过大——本次会话仅保留前 {0} 个字体，但未缓存此来源。",
  },
  font_scan_incomplete_io: {
    en: "Scan incomplete — part of the library changed or could not be read. {0} font(s) were loaded for this session; the source was not cached.",
    zh: "扫描未完成——字体库的一部分发生变化或无法读取。本次会话已加载 {0} 个字体，但未缓存此来源。",
  },
  font_scan_session_only: {
    en: "Loaded {0} font(s) for this session, but the source was not cached. Add it again after restarting.",
    zh: "已为本次会话加载 {0} 个字体，但未缓存此来源；重启后请重新添加。",
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

  // ── Persistent Font Cache (Drift Modal, #5) ─────────────
  font_cache_drift_title: {
    en: "Font cache out of date",
    zh: "字体缓存已过期",
  },
  font_cache_drift_summary: {
    en: "{0} font source(s) changed since the cache was last refreshed: {1} modified, {2} removed.",
    zh: "自上次刷新以来，有 {0} 个字体来源发生变化：{1} 个被修改，{2} 个已移除。",
  },
  font_cache_drift_modified_label: { en: "Modified:", zh: "已修改：" },
  font_cache_drift_removed_label: { en: "Removed:", zh: "已移除：" },
  font_cache_source_scope_shallow: { en: "top level", zh: "仅第一层" },
  font_cache_source_scope_recursive: {
    en: "folder + all subfolders",
    zh: "本文件夹及所有子文件夹",
  },
  font_cache_drift_close_hint: {
    en: "Closing this dialog (✕ / Esc) is the same as “Use as-is”.",
    zh: "关闭此对话框（✕ / Esc）等同于「保持原样使用」。",
  },
  font_cache_drift_btn_rescan: { en: "Rescan now", zh: "立即重新扫描" },
  font_cache_drift_btn_use_as_is: { en: "Use as-is", zh: "保持原样使用" },
  font_cache_drift_btn_clear: { en: "Clear cache", zh: "清除缓存" },
  font_cache_rescanning: { en: "Rescanning font cache…", zh: "正在重新扫描字体缓存…" },
  font_cache_clearing: { en: "Clearing cache…", zh: "正在清除缓存…" },
  font_cache_rescan_done: {
    en: "Rescanned {0} font source(s); removed cached data for {1} source(s).",
    zh: "已重新扫描 {0} 个字体来源；已移除 {1} 个来源的缓存数据。",
  },
  font_cache_rescan_skipped_label: {
    en: "{0} font source(s) could not be refreshed cleanly. Stale entries were dropped where possible:",
    zh: "{0} 个字体来源未能完整刷新；系统已尽可能移除过期缓存条目：",
  },
  font_cache_cleared: { en: "Font cache cleared.", zh: "字体缓存已清除。" },
  font_cache_unavailable_banner: {
    en: "Font cache unavailable — embed will use system fonts only.",
    zh: "字体缓存不可用 —— 嵌入将仅使用系统字体。",
  },
  font_cache_rebuild_required_title: {
    en: "Font cache needs rebuilding",
    zh: "字体缓存需要重建",
  },
  font_cache_rebuild_required_body: {
    en: "The font cache file is from a different release. Click “Clear cache” to wipe it and start fresh.",
    zh: "字体缓存文件来自不同版本。点击「清除缓存」将其清空并重新开始。",
  },

  // ── Shared ──────────────────────────────────────────────
  btn_clear_file: { en: "Clear", zh: "清除" },
  btn_dismiss: { en: "Dismiss", zh: "关闭" },
  lang_switch_to_zh: { en: "切换到中文", zh: "切换到中文" },
  lang_switch_to_en: { en: "Switch to English", zh: "Switch to English" },
  msg_dedup_blocked: {
    en: "Can't load — {0} file(s) already loaded in the {1} tab. Clear them there first.",
    zh: "无法加载 — {0} 个文件已在「{1}」标签页中。请先在该标签页中清除。",
  },
  // multi-tab variant. The single-tab message
  // reads "in the {N} tab" with `{N}` being a single tab name; that
  // wording breaks for the multi-tab case where `{1}` becomes a
  // compound "3 HDR / 2 Shift" string, producing "in the 3 HDR / 2
  // Shift tab". The plural form drops the article + "tab" suffix
  // and lets the compound speak for itself.
  msg_dedup_blocked_multi: {
    en: "Can't load — {0} file(s) already loaded ({1}). Clear them there first.",
    zh: "无法加载 — {0} 个文件已在其他标签页中（{1}）。请先在相应标签页中清除。",
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
  rename_multi_subtitle_mode: {
    en: "Keep multiple subtitles per video",
    zh: "保留每个视频的多个字幕",
  },
  rename_multi_subtitle_mode_hint: {
    en: "Tagged subtitles write as Video.sc.ass / Video.tc.ass; untagged subtitles keep Video.ass.",
    zh: "带标签字幕会写成 Video.sc.ass / Video.tc.ass；无标签字幕仍使用 Video.ass。",
  },
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
  msg_rename_input_conflict: {
    en: "Blocked {0}: it is part of a planned conflict where an output targets a loaded subtitle input. No files in that conflicting chain were changed.",
    zh: "已阻止 {0}：该字幕涉及计划批次中的冲突，其中一个输出指向已加载的字幕输入。该冲突链中的文件均未更改。",
  },
  msg_rename_skipped_count: {
    // Covers path-derivation failures and output-to-loaded-input
    // conflicts found before confirmation. No-op targets, duplicate outputs,
    // and loop-time copy/rename errors have their own reporting paths.
    en: "Note: {0} pairing(s) failed preflight and won't run (see log).",
    zh: "注意：另有 {0} 对配对未通过预检、不会执行（见日志）。",
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
  msg_rename_all_failed: {
    en: "Rename failed: all {0} file(s) errored",
    zh: "重命名失败：{0} 个文件全部出错",
  },
  msg_rename_already_named: {
    en: "Already correctly named: {0} (skipped no-op)",
    zh: "已是目标名：{0}（无需操作）",
  },
  msg_rename_all_already_named: {
    en: "All {0} subtitle file(s) already match their videos — nothing to do.",
    zh: "全部 {0} 个字幕文件已与视频同名 — 无需操作。",
  },

  // ── Style Edit (Tab 5) ──────────────────────────────────
  style_drop_hint: {
    en: "Tip: drag .ass / .ssa files or a folder onto the file strip above. Other files are skipped automatically.",
    zh: "提示：可将 .ass / .ssa 文件或文件夹拖到上方文件栏，其他文件会自动忽略。",
  },
  style_operations_title: { en: "Style operations", zh: "样式操作" },
  style_operations_hint: {
    en: "Enable either operation or both. Changes apply only to checked Style rows.",
    zh: "可启用任一项或同时启用两项；更改只应用于已勾选的 Style 行。",
  },
  style_change_font_family: { en: "Change font family", zh: "更改字体族" },
  style_target_font_family: { en: "New font family", zh: "新字体族" },
  style_target_font_placeholder: { en: "e.g. Microsoft YaHei", zh: "例如：Microsoft YaHei" },
  style_filter_source_family: {
    en: "Only replace one existing font family",
    zh: "仅替换指定的现有字体族",
  },
  style_source_font_family: { en: "Existing font family", zh: "现有字体族" },
  style_source_font_placeholder: { en: "Family to replace", zh: "要替换的字体族" },
  style_change_font_size: { en: "Change font size", zh: "更改字号" },
  style_target_font_size: { en: "New font size", zh: "新字号" },
  style_font_size_range: { en: "1–200", zh: "1–200" },
  style_inline_untouched: {
    en: "Inline \\fn and \\fs override tags stay unchanged in this version.",
    zh: "此版本不会更改行内 \\fn 和 \\fs 覆盖标签。",
  },
  style_output_note: {
    en: "Outputs are new sibling files named .styled.ass / .styled.ssa. Existing outputs are never overwritten.",
    zh: "输出为源文件旁的新 .styled.ass / .styled.ssa 文件；绝不覆盖已有输出。",
  },
  style_preview_title: {
    en: "Style preview · {0} row(s) across {1} file(s)",
    zh: "样式预览 · {1} 个文件中的 {0} 行",
  },
  style_preview_change_summary: {
    en: "{0} effective change(s)",
    zh: "{0} 项有效更改",
  },
  style_select_all: { en: "Select all", zh: "全选" },
  style_clear_selection: { en: "Clear", zh: "清除" },
  style_no_preview: {
    en: "Load ASS/SSA files to inspect their Style rows.",
    zh: "加载 ASS/SSA 文件以检查其中的 Style 行。",
  },
  style_col_file: { en: "File", zh: "文件" },
  style_col_style: { en: "Style", zh: "样式" },
  style_col_font_family: { en: "Font family", zh: "字体族" },
  style_col_font_size: { en: "Size", zh: "字号" },
  style_col_result: { en: "Result", zh: "结果" },
  style_row_select_aria: {
    en: "Select style {0} in {1}",
    zh: "选择 {1} 中的样式 {0}",
  },
  style_row_will_change: { en: "Will change", zh: "将更改" },
  style_row_no_change: { en: "No change", zh: "无更改" },
  style_row_parse_error: { en: "Cannot preview", zh: "无法预览" },
  style_no_editable_styles: { en: "No editable Style rows", zh: "没有可编辑的 Style 行" },
  style_font_error_required: { en: "Enter a font family.", zh: "请输入字体族。" },
  style_font_error_surrounding_whitespace: {
    en: "Remove leading or trailing spaces.",
    zh: "请移除开头或结尾的空格。",
  },
  style_font_error_too_long: {
    en: "Font family must be 128 characters or fewer.",
    zh: "字体族不得超过 128 个字符。",
  },
  style_font_error_comma: {
    en: "Font family cannot contain a comma.",
    zh: "字体族不能包含逗号。",
  },
  style_font_error_control: {
    en: "Font family contains an invisible or control character.",
    zh: "字体族包含不可见字符或控制字符。",
  },
  style_font_size_error: {
    en: "Enter a font size from 1 to 200.",
    zh: "请输入 1 到 200 之间的字号。",
  },
  btn_write_style_edits: { en: "Write ({0})", zh: "写入（{0}）" },
  btn_writing_style_edits: { en: "Writing…", zh: "写入中…" },
  msg_no_ass_in_drop: {
    en: "No ASS/SSA files found in the dropped items",
    zh: "拖入的内容中没有 ASS/SSA 文件",
  },
  msg_style_too_many_rows: {
    en: "Selection contains more than {0} Style rows. Split the batch so every change can be previewed.",
    zh: "所选内容超过 {0} 个 Style 行。请拆分批量，以便完整预览每项更改。",
  },
  msg_style_source_bytes_too_large: {
    en: "Batch source files exceed the {1} MB safety cap (reached {0} MB). Split the selection.",
    zh: "批量源文件超过 {1} MB 安全上限（已达 {0} MB）。请拆分选择。",
  },
  msg_style_decoded_bytes_too_large: {
    en: "Decoded batch text exceeds the {1} MB memory cap (reached {0} MB). Split the selection.",
    zh: "批量解码文本超过 {1} MB 内存上限（已达 {0} MB）。请拆分选择。",
  },
  msg_style_lossy_encoding: {
    en: "Cannot safely edit {0}: its text encoding could not be decoded without data loss.",
    zh: "无法安全编辑 {0}：该文件的文本编码无法无损解码。",
  },
  msg_style_parse_error: {
    en: "Cannot analyze {0}: {1}",
    zh: "无法分析 {0}：{1}",
  },
  msg_style_outputs_exist: {
    en: "{0} output file(s) already exist. No files were written; remove or rename those outputs first.",
    zh: "已有 {0} 个输出文件。未写入任何文件；请先移除或重命名这些输出。",
  },
  msg_style_duplicate_outputs: {
    en: "Multiple inputs resolve to the same .styled output. No files were written.",
    zh: "多个输入会生成同一个 .styled 输出。未写入任何文件。",
  },
  msg_style_start: {
    en: "Starting style edit: {0} file(s), {1} selected change(s)",
    zh: "开始样式编辑：{0} 个文件，{1} 项已选更改",
  },
  msg_style_written: {
    en: "Written: {0} ({1} Style row(s) changed)",
    zh: "已写入：{0}（更改了 {1} 个 Style 行）",
  },
  msg_style_file_noop: {
    en: "Skipped {0}: no selected Style rows would change",
    zh: "已跳过 {0}：所选 Style 行无需更改",
  },
  msg_style_write_error: {
    en: "Error writing {0}: {1}",
    zh: "写入 {0} 出错：{1}",
  },
  msg_style_complete: {
    en: "Style edit complete: {0} written, {1} unchanged, {2} failed",
    zh: "样式编辑完成：写入 {0} 个，未更改 {1} 个，失败 {2} 个",
  },
  msg_style_all_failed: {
    en: "Style edit failed on all {0} writable file(s)",
    zh: "全部 {0} 个可写文件均编辑失败",
  },
  msg_style_all_noop: {
    en: "Nothing to write — no selected Style rows would change.",
    zh: "无需写入——所选 Style 行均无需更改。",
  },
  msg_style_cancelled: { en: "Style edit cancelled.", zh: "已取消样式编辑。" },
  msg_style_cancelled_summary: {
    en: "Style edit cancelled: {0} written, {1} failed, {2} not started",
    zh: "已取消样式编辑：写入 {0} 个，失败 {1} 个，未开始 {2} 个",
  },
};
