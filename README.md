# SSA HDRify

[![License: GPL v3](https://img.shields.io/badge/License-GPL%20v3-blue.svg)](LICENSE) [![GitHub release](https://img.shields.io/github/v/release/koagaroon/ssaHdrify-tauri)](https://github.com/koagaroon/ssaHdrify-tauri/releases) ![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-blue)

> **SSA HDRify 是一款桌面工具，能将 SSA/ASS 字幕的颜色转换为适合 HDR 播放的值，同时提供时间轴偏移、字体嵌入和批量重命名等辅助功能。** 它是 [gky99/ssaHdrify](https://github.com/gky99/ssaHdrify)（Python 原版）的 Tauri 桌面重写版。
>
> _SSA HDRify is a desktop tool that converts SSA/ASS subtitle colors into HDR-ready values, with companion tools for timing shift, font embedding, and batch renaming._ It is a Tauri desktop rewrite of [gky99/ssaHdrify](https://github.com/gky99/ssaHdrify) (the original Python version).

### 浅色主题（中文）/ Light Theme (Chinese)

|                       HDR 转换 / HDR Convert                       |                       时间轴偏移 / Time Shift                       |
| :----------------------------------------------------------------: | :-----------------------------------------------------------------: |
| <img src="docs/screenshots/hdr-convert-light-zh.jpg" width="450"/> | <img src="docs/screenshots/timing-shift-light-zh.jpg" width="450"/> |

|                       字体嵌入 / Font Embed                       |                      批量重命名 / Batch Rename                      |
| :---------------------------------------------------------------: | :-----------------------------------------------------------------: |
| <img src="docs/screenshots/font-embed-light-zh.jpg" width="450"/> | <img src="docs/screenshots/batch-rename-light-zh.jpg" width="450"/> |

### 深色主题（英文）/ Dark Theme (English)

|                      HDR 转换 / HDR Convert                       |                      时间轴偏移 / Time Shift                       |
| :---------------------------------------------------------------: | :----------------------------------------------------------------: |
| <img src="docs/screenshots/hdr-convert-dark-en.jpg" width="450"/> | <img src="docs/screenshots/timing-shift-dark-en.jpg" width="450"/> |

|                      字体嵌入 / Font Embed                       |                     批量重命名 / Batch Rename                      |
| :--------------------------------------------------------------: | :----------------------------------------------------------------: |
| <img src="docs/screenshots/font-embed-dark-en.jpg" width="450"/> | <img src="docs/screenshots/batch-rename-dark-en.jpg" width="450"/> |

---

## 目录 | Contents

- [下载 | Download](#下载--download)
- [功能 | Features](#功能--features)
- [支持格式 | Supported Formats](#支持格式--supported-formats)
- [使用方法 | Usage](#使用方法--usage)
- [CLI 使用 | CLI Usage](#cli-使用--cli-usage)
- [使用场景 | Background](#使用场景--background)
- [HDR 转换原理 | How HDR Conversion Works](#hdr-转换原理--how-hdr-conversion-works)
- [从源码构建 | Build from Source](#从源码构建--build-from-source)
- [架构 | Architecture](#架构--architecture)
- [致谢 | Credits](#致谢--credits)
- [许可证 | License](#许可证--license)

---

## 下载 | Download

Windows 用户可从 [Releases](https://github.com/koagaroon/ssaHdrify-tauri/releases) 页面下载免安装的便携版 exe。建议优先使用最新稳定版；预览版也会保留在同一页面，用于测试尚未进入稳定版的功能。

> [!NOTE]
> 最新稳定版已包含 CLI、字体缓存 / 诊断、`.vtt` HDR 转换和 `.sup` 批量重命名侧车支持；预览版可能额外包含尚未进入稳定版的修复或改进。

- **`ssahdrify*.exe`** — 图形界面（GUI），适合手动操作
- **`ssahdrify-cli*.exe`** — 命令行（CLI），适合自动化流水线、批处理和脚本化场景

macOS / Linux 用户请参考下方「从源码构建」。

Windows users can download portable, no-installer exe files from [Releases](https://github.com/koagaroon/ssaHdrify-tauri/releases). Use the latest stable build by default; preview builds remain listed on the same page for testing features that have not entered a stable release yet.

> [!NOTE]
> The latest stable release includes the CLI, font cache / diagnostics, `.vtt` HDR conversion, and `.sup` Batch Rename sidecar support. Preview builds may also include fixes or improvements that have not entered the stable line yet.

- **`ssahdrify*.exe`** — graphical interface (GUI), for manual workflows
- **`ssahdrify-cli*.exe`** — command line (CLI), for automation pipelines, batch jobs, and scripts

macOS / Linux users, see "Build from Source" below.

---

## 功能 | Features

| 标签页 / Tab                            | 功能 / Description                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| --------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **HDR 色彩转换 / HDR Color Conversion** | 将字幕颜色转换为适配 BT.2100 PQ 或 HLG 的 HDR 色彩值 / Convert subtitle colors into HDR values for BT.2100 PQ or HLG                                                                                                                                                                                                                                                                                                                               |
| **时间轴偏移 / Timing Shift**           | 批量调整字幕时间戳；可从指定时间点之后开始偏移，并实时预览效果 / Batch-adjust subtitle timestamps; optionally start after a chosen timestamp, with live preview                                                                                                                                                                                                                                                                                    |
| **字体嵌入 / Font Embedding**           | 自动检测字幕引用的字体，在系统字体库或本地字体源中匹配，并把子集化后的字体嵌入 ASS 文件 / Detect fonts referenced by the subtitle, match them from system or local font sources, and embed subset fonts into the ASS file                                                                                                                                                                                                                          |
| **批量重命名 / Batch Rename**           | 自动匹配视频和字幕，并按视频文件名重命名字幕；当同一视频匹配到多个候选字幕时，可手动选择并调整配对，也可以启用多字幕模式，为每个视频保留多个语言的外挂字幕。 / Automatically match videos with subtitles and rename subtitles to match the video filename; when one video has multiple matching subtitle candidates, manually choose and adjust pairings, or enable multi-subtitle mode to keep multiple language sidecar subtitles for each video |

> [!TIP]
> **完整支持中文路径** — 包含中文、日文或其他非 ASCII 字符的文件路径都可以正常处理。Tauri 和 Rust 底层使用 Unicode API，不受传统 ANSI 编码限制。
>
> **Non-ASCII paths are supported** — File paths containing Chinese, Japanese, or other non-ASCII characters are handled correctly. Tauri and Rust use native Unicode APIs under the hood.

---

## 支持格式 | Supported Formats

不同功能的格式支持范围并不完全相同；下表是当前行为。`HDR 色彩转换` 会先将 `.srt`、`.sub`、`.vtt` 转换为 ASS 再进行处理。

Format support differs by workflow. The table below describes current behavior. `HDR Color Conversion` first converts `.srt`, `.sub`, and `.vtt` to ASS before processing.

| 功能 / Workflow                     | `.ass` / `.ssa`                            | `.srt`                                     | `.sub`                                     | `.vtt`                                                                                 | `.sup`                                                       |
| ----------------------------------- | ------------------------------------------ | ------------------------------------------ | ------------------------------------------ | -------------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| HDR 色彩转换 / HDR Color Conversion | 原生处理 / native                          | 转换为 ASS / convert to ASS                | 转换为 ASS / convert to ASS                | 基本文本 cue 转换为 ASS / basic text cues convert to ASS                               | 不支持 / no                                                  |
| 时间轴偏移 / Timing Shift           | 保持格式 / preserve format                 | 保持格式 / preserve format                 | 保持格式 / preserve format                 | 重建基础 cue；不保留全部 WebVTT 元数据 / rebuilds basic cues; not full WebVTT metadata | 不支持 / no                                                  |
| 字体嵌入 / Font Embedding           | 支持 / yes                                 | 不支持 / no                                | 不支持 / no                                | 不支持 / no                                                                            | 不支持 / no                                                  |
| `diagnose-fonts`                    | 支持 / yes                                 | 不支持 / no                                | 不支持 / no                                | 不支持 / no                                                                            | 不支持 / no                                                  |
| 批量重命名 / Batch Rename           | 配对、复制或重命名 / pair, copy, or rename | 配对、复制或重命名 / pair, copy, or rename | 配对、复制或重命名 / pair, copy, or rename | 配对、复制或重命名 / pair, copy, or rename                                             | 作为不解析的侧车文件配对、复制或重命名 / opaque sidecar only |
| `chain`                             | 取决于步骤 / depends on steps              | 取决于步骤 / depends on steps              | 取决于步骤 / depends on steps              | 仅支持本身接受 `.vtt` 的步骤 / only where the chosen step accepts `.vtt`               | 不支持 / no                                                  |

> [!NOTE]
> 这里的 `.sub` 指 MicroDVD 文本字幕。Blu-ray PGS `.sup` 和 VobSub `.sub/.idx` 属于图像字幕，不适用于 HDR 文本颜色转换、时间轴偏移、ASS 字体嵌入或 `diagnose-fonts`。`.sup` 只会在「批量重命名」中作为不解析内容的侧车文件进行配对、复制或重命名。如需把图像字幕变成文本字幕，请先使用专门的字幕转换/OCR 工具。
>
> Here `.sub` means MicroDVD text subtitles. Blu-ray PGS `.sup` and VobSub `.sub/.idx` are image subtitle formats, so HDR text color conversion, Timing Shift, ASS font embedding, and `diagnose-fonts` do not apply. `.sup` is only paired, copied, or renamed as an opaque sidecar in Batch Rename. To turn image subtitles into text subtitles, convert/OCR them with a dedicated subtitle tool first.

---

## 使用方法 | Usage

### HDR 色彩转换 / HDR Color Conversion

1. 选择 EOTF 曲线（PQ 或 HLG）/ Select EOTF curve (PQ or HLG)
2. 设置字幕目标亮度（默认 203 nits）/ Set target subtitle brightness (default: 203 nits)
3. 选择字幕文件（支持多选）/ Select subtitle files (multi-select supported)
4. 点击「转换」/ Click **Convert**
5. 默认输出扩展名为 `.hdr.ass`（可修改）/ Default output extension is `.hdr.ass` (customizable)

> **参数说明 | Parameter Guide**
>
> | 参数 / Parameter  | 默认值 / Default | 说明 / Description                                                                                                                                                       |
> | ----------------- | ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
> | EOTF curve        | PQ               | PQ (ST 2084) 用于 HDR10/杜比视界；HLG 用于广播 HDR / PQ for HDR10/Dolby Vision; HLG for broadcast HDR                                                                    |
> | Target brightness | 203 nits         | SDR 字幕峰值亮度（BT.2408 标准值）。如果字幕太亮就调低，太暗就调高 / Peak SDR subtitle brightness (BT.2408 reference value). Lower it if too bright; raise it if too dim |

### 时间轴偏移 / Timing Shift

1. 选择字幕文件 / Select a subtitle file
2. 输入偏移量（毫秒），并选择「提前」或「延后」/ Enter offset amount (ms), then choose Faster or Slower
3. 可选：只偏移指定时间点之后的字幕行 / Optionally shift only lines after a specific timestamp
4. 实时预览调整结果 / Preview the result in real time
5. 导出 / Export

### 字体嵌入 / Font Embedding

1. 点击「选择字幕文件 / Select Subtitle File」，选择一个或多个 ASS/SSA 字幕文件 / Click **Select Subtitle File** to pick one or more ASS/SSA files
2. 工具会自动检测字幕引用的字体，并优先尝试从系统字体库匹配 / The tool detects fonts referenced by the subtitle and first tries to match them against the system font library
3. 主面板会实时显示本地字体源覆盖情况（覆盖 N / M）和尚未匹配的字体；每个字体都会标注来源（本地 / 系统）和状态（已找到 / 缺失）/ The main panel shows live local-source coverage (Coverage: N / M) and lists any still-missing families; each detected font is tagged with its source (Local / System) and status (Found / Missing)
4. 选择输出位置：默认保存到源字幕旁，也可以保存到指定文件夹；指定文件夹模式会将输出平铺到该文件夹，重复输出名会自动跳过 / Choose the output location: save beside each source subtitle by default, or save into a chosen folder; chosen-folder mode writes flat outputs into that folder and skips duplicate output names
5. 点击「嵌入已选字体」，将子集化后的字体数据写入 `.embedded.ass` 输出文件 / Click **Embed Selected Fonts** to write the subset font data into `.embedded.ass` output files

字幕组排版常用字体通常没有安装在系统中。打开「字体来源 / Font Sources」面板，添加需要扫描的本地文件夹；这些字体无需系统安装，也可以参与匹配。

Fonts used in fan-sub typesetting often are not installed system-wide. Open **Font Sources**, add the local folders you want to scan, and those fonts can be matched without installing them into the OS.

支持大型字体文件夹；扫描时会实时显示已读取的字体数量，也可以随时取消。选择约 5000 个字体文件或总量约 5 GiB 以上的来源前，程序会先弹出确认对话框。扫描和缓存写入都有安全上限；超大或异常来源可能提前停止，或仅用于本次会话而不写入持久化缓存，并会在界面/日志中提示。

Large font folders are supported; the scan shows a real-time count of fonts found and can be cancelled at any time. Before scanning a source with about 5000 font files or about 5 GiB of content, the app asks for confirmation. Scanning and cache writes are bounded by safety ceilings; unusually large or malformed sources may stop early or be used only for the current session instead of being persisted, with a visible message.

> **字体名称匹配 / Font Name Matching**
>
> 工具会读取字体文件的 OpenType `name` 表，并索引受支持的本地化 family、typographic family、full-face 和 PostScript 名称变体（英文、中文等），同时内置了异常字体防护上限。ASS 脚本无论引用哪个受支持名称，都能匹配到同一个字体文件；`@家族名` 这类竖排前缀也会按同一字体处理。
>
> The tool reads each font's OpenType `name` table and indexes supported localized family, typographic-family, full-face, and PostScript name variants (English, Chinese, etc.), with built-in safety caps against abnormal fonts. An ASS script referencing any supported name resolves to the same font file; the ASS `@FamilyName` vertical-writing prefix is treated as the same font.
>
> 对 `.ttc` / `.otc` 字体集合，工具会按匹配到的 face index 抽出对应字形并嵌入为单 face 子集。ASS `[Fonts]` 里的 `fontname:` 是生成的嵌入项标签，例如 `dream_han_serif_sc_w22.ttf`，不代表源文件必须是 `.ttf`；实际匹配依赖子集字体内部保留的 `name` 表。
>
> For `.ttc` / `.otc` collections, the matched face index is subset into a single-face embedded font. The ASS `[Fonts]` `fontname:` line is a generated attachment label, such as `dream_han_serif_sc_w22.ttf`; it does not mean the source file had to be `.ttf`. Matching relies on the preserved internal `name` table in the subset font.

### 批量重命名 / Batch Rename

1. 拖入包含视频和字幕的文件夹（或点击「选择文件 / Select Files」手动选择）；程序会按文件格式自动归类 / Drop a folder containing both videos and subtitles (or click **Select Files** to pick manually); the app categorizes files by format automatically
2. 应用会按字幕组常见命名方式提取剧集号，并预填配对表 / The app extracts episode numbers from common fan-sub naming patterns and pre-fills the pairing table
3. 如果出现错配或漏配，可直接在对应行的下拉框中手动选择字幕；选中后该行会自动加入重命名队列 / If the app mispairs or misses a row, choose a subtitle from that row's dropdown; once selected, the row is automatically added to the rename queue
4. 如果要为同一视频保留多个语言字幕，启用「保留每个视频的多个字幕」；带语言标签的字幕会写成 `Video.sc.ass`、`Video.tc.ass` 这类文件名 / To keep multiple language subtitles for the same video, enable **Keep multiple subtitles per video**; tagged subtitles are written as names such as `Video.sc.ass` and `Video.tc.ass`
5. 选择输出策略：原文件直接改名 / 复制到视频所在目录 / 复制到自定义目录 / Pick the output strategy: rename in place, copy to the video's directory, or copy to a custom directory
6. 点击「运行」；如果目标路径已存在按同一规则生成的同名文件，程序会先弹出覆盖确认对话框 / Click **Run**; if a file with the generated name already exists at the target path, an overwrite confirmation appears first

> **配对算法 | Pairing Algorithm**
>
> 处理流程：清理括号内容 → 按优先级尝试剧集号正则（`S\d+E\d+`、`][NN][`、`- NN`、`第N话`、`EP\d+`）→ 分季并行扫描 → `(season, episode)` 配对键 → LCS 回退 → 最后由手动选择兜底。规则已在多组真实字幕组命名样本上验证过，包括中日双语标题、外挂多语字幕、季度后缀变体等。
>
> Pipeline: bracket cleanup → priority-ordered episode regex (`S\d+E\d+`, `][NN][`, `- NN`, `第N话`, `EP\d+`) → parallel season-aware scan → `(season, episode)` pairing key → LCS fallback → manual selection as the final fallback. Pattern coverage was validated against representative real-world fan-sub naming variants, including bilingual CJK titles, externally shipped multi-language subtitles, and season-suffix variants.

---

## CLI 使用 | CLI Usage

`ssahdrify-cli` 是 GUI 的命令行版（CLI），与 GUI 从同一份源代码构建。四个核心功能（HDR 转换 / 时间轴偏移 / 字体嵌入 / 批量重命名）和 GUI 版保持对等；CLI 另外提供 `chain`（一次调用串联多个步骤，只有最后一步写入文件）、`refresh-fonts`（构建或刷新 CLI 字体缓存）和 `diagnose-fonts`（只诊断字体解析，不写字幕）等子命令。

`ssahdrify-cli` is the command-line (CLI) version of the GUI, built from the same source. The four core features (HDR convert / Timing shift / Font embed / Batch rename) stay in parity with the GUI; the CLI additionally exposes `chain` (multiple steps in one invocation, with only the final step writing files), `refresh-fonts` (build or refresh the CLI font cache), and `diagnose-fonts` (diagnose font resolution without writing subtitles).

### 快速示例 | Quick Examples

下面示例中的 `<font-folder>`、`<series-folder>` 等都是占位符；请替换成你自己电脑上的实际路径。

Placeholders such as `<font-folder>` and `<series-folder>` mean your own local paths; replace them before running the commands.

```text
# HDR 色彩转换（PQ 曲线）/ HDR conversion (PQ curve)
ssahdrify-cli hdr --eotf pq input.ass

# 时间轴偏移 +500ms / Timing shift +500ms
ssahdrify-cli shift --offset +500ms input.ass

# 时间轴映射：按多个时间段应用不同偏移 / Timing map: apply different offsets by segment
ssahdrify-cli shift --map timing-map.json input.srt

# 字体嵌入：从指定文件夹查找字体 / Font embed: search a folder for fonts
ssahdrify-cli embed --font-dir "<font-folder>" input.ass

# 字体解析诊断：不写输出文件 / Font diagnostics: no output subtitle writes
ssahdrify-cli diagnose-fonts --font-dir "<font-folder>" input.ass

# 持久化字体缓存：先扫描一次，后续 embed 复用 / Persistent font cache: scan once, reuse later
ssahdrify-cli refresh-fonts --font-dir "<font-folder>"
ssahdrify-cli embed input.ass            # 自动使用缓存 / uses cache automatically

# 链式调用：一次完成 HDR 转换和时间轴偏移，只有最后一步写文件 / Chain: HDR + shift in one command, only the final step writes
ssahdrify-cli chain hdr --eotf pq + shift --offset +500ms input.ass

# 批量重命名：默认复制到视频所在目录 / Batch rename (default: copy sub next to video)
ssahdrify-cli rename "<series-folder>"

# 多外挂字幕：保留语言后缀，避免 sc/tc 同扩展字幕互相覆盖 / Multiple sidecar subtitles: keep language suffixes
ssahdrify-cli rename "<series-folder>" --langs all --dry-run
```

`rename --langs auto` 保持和 GUI 一致的默认行为：每个视频只选一个字幕，输出文件名精确匹配视频 stem（如 `Video.ass`）。`rename --langs all` 或显式列表（如 `--langs sc,jp`）可以为同一个视频规划多个字幕，并写成带语言后缀的文件名（如 `Video.sc.ass`、`Video.jp.srt`）；没有语言标记的字幕仍使用精确视频名（如 `Video.ass`）。如果多行会写到同一个目标路径，CLI 会在写入前阻止这些冲突行。

`rename --langs auto` keeps the GUI-style behavior: one subtitle per video, named exactly like the video stem (`Video.ass`). `rename --langs all` or an explicit list such as `--langs sc,jp` can plan multiple subtitles for the same video and writes language-suffixed names such as `Video.sc.ass` and `Video.jp.srt`; untagged subtitles still use the exact video name (`Video.ass`). If multiple rows would write to the same target path, the CLI blocks those conflict rows before writing.

`shift --map <FILE>` 使用一个只读时间轴映射文件，不运行 Sushi、alass、FFmpeg 或任何音频自动同步工具。JSON 格式可以写成 `{"rules":[{"start":"00:00:00.000","end":"00:05:00.000","offset":"+1s","label":"opening"},{"startMs":5000,"offsetMs":-500}]}`。也可以使用简单 CSV 行：`start,end,offset,label,enabled`，例如 `00:00:00.000,00:05:00.000,+1s,opening,true`。`start` 为闭区间，`end` 为开区间；规则出现重叠时，按文件顺序以最先匹配的为准。映射文件会在批处理开始前先解析和校验，失败时不会写任何字幕。

`shift --map <FILE>` uses a read-only timing-map file; it does not run Sushi, alass, FFmpeg, or any audio auto-sync helper. JSON can look like `{"rules":[{"start":"00:00:00.000","end":"00:05:00.000","offset":"+1s","label":"opening"},{"startMs":5000,"offsetMs":-500}]}`. A simple CSV shape is also accepted: `start,end,offset,label,enabled`, for example `00:00:00.000,00:05:00.000,+1s,opening,true`. `start` is inclusive, `end` is exclusive, and if rules overlap, the first matching row in file order wins. The map is parsed and validated before batch processing starts, so invalid maps do not write subtitle outputs.

### 全部子命令 | All Subcommands

每个子命令都可通过 `--help` 查看完整参数。

Each subcommand supports `--help` for the full parameter reference.

```bash
ssahdrify-cli --help
ssahdrify-cli hdr            --help
ssahdrify-cli shift          --help
ssahdrify-cli embed          --help
ssahdrify-cli rename         --help
ssahdrify-cli diagnose-fonts --help
ssahdrify-cli refresh-fonts  --help
ssahdrify-cli chain          --help
```

### 全局选项 | Global Options

| 选项 / Option         | 说明 / Description                                                                                                                                                                                                                                           |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `--lang <en\|zh>`     | 输出语言；不指定时按系统区域设置自动检测（zh\* → zh，否则 en）/ Output language; auto-detected from OS locale when omitted (zh\* → zh, otherwise en)                                                                                                         |
| `--json`              | 为支持的子命令输出机器可读 JSON 报告；详见下方 JSON 模式 / Emit machine-readable JSON for supported subcommands; see JSON Mode below                                                                                                                         |
| `--verbose`           | 显示更详细的进度 / Show more detailed progress                                                                                                                                                                                                               |
| `--quiet`             | 隐藏常规进度输出 / Suppress normal progress output                                                                                                                                                                                                           |
| `--dry-run`           | 预览计划执行的操作，不写入文件 / Preview planned work without writing files                                                                                                                                                                                  |
| `--overwrite`         | 允许覆盖已存在的输出文件 / Replace existing output files instead of skipping                                                                                                                                                                                 |
| `--output-dir <DIR>`  | 将输出重定向到指定目录 / Redirect output to a specific directory                                                                                                                                                                                             |
| `--no-cache`          | 跳过本次运行的字体缓存；缓存文件本身保持不变 / Skip the font cache for this run; leave the cache file untouched                                                                                                                                              |
| `--cache-file <PATH>` | 使用指定缓存文件路径，覆盖默认路径 / Use a specific cache file path instead of the OS default (see Cache Location below)                                                                                                                                     |
| `--fail-fast`         | 任一文件失败即停止处理后续输入；已成功写出的文件会保留，失败文件的目标位置可能留下部分写入产物 / Abort the batch on the first failed file; previously-succeeded outputs are kept, but the failed input may leave a partial-write artifact at its destination |

> **JSON 模式 | JSON Mode**
>
> `--json` 目前适用于 `hdr` / `shift` / `embed` / `rename` 和 `diagnose-fonts`。常规子命令会输出固定 schema 的报告，按文件列出 status (`written` / `planned` / `skipped` / `failed`)、output path、encoding、warnings 等字段；stderr 仍可输出供人阅读的诊断信息。`diagnose-fonts --json` 直接输出诊断报告。`chain` v1 会明确提示不支持 JSON，并改用纯文本报告；`refresh-fonts` 使用 stderr 输出状态。
>
> `--json` currently applies to `hdr` / `shift` / `embed` / `rename` and `diagnose-fonts`. Normal subcommands emit a fixed-schema report listing per-file status (`written` / `planned` / `skipped` / `failed`), output path, encoding, warnings, and related fields; stderr can still carry human-readable diagnostics. `diagnose-fonts --json` emits the diagnostic report directly. `chain` v1 explicitly reports that JSON output is not supported and falls back to plain text; `refresh-fonts` reports status on stderr.
>
> 启用 `--diagnose` 时，JSON 会额外包含完整 `diagnostics` 对象，即使人类输出模式是默认的 summary。未启用 `--diagnose` 时，常规 JSON schema 保持不变。
>
> When `--diagnose` is enabled, JSON includes a full `diagnostics` object even when human output is the default summary mode. Without `--diagnose`, the normal JSON schema is unchanged.
>
> **终端字符串插值安全注意事项 | Terminal string-interpolation safety note**
>
> `--json` 按 RFC 8259 输出；BiDi 控制符（U+200E/U+202E 等）、零宽字符，以及 U+2028/U+2029 行分隔符在 JSON 字符串中都是合法字符，因此不会额外转义。如果用 `jq -r` 将 `.input` / `.output` 等字段还原后再插入到终端（例如 `echo`、提示符或其他 CLI 参数），恶意构造的文件名可能影响终端显示。下游脚本应在终端输出边界自行过滤（如 jq 的 `gsub` 或 shell 包装工具）。CLI 自身供人阅读的输出（未启用 `--json`）已在所有打印点调用 `sanitize_for_display`，不受此问题影响。
>
> `--json` output follows RFC 8259, but BiDi format characters (U+200E/U+202E etc.), zero-width characters, and the U+2028/U+2029 line separators are valid in JSON strings and are not additionally escaped. If you pipe to `jq -r` to extract `.input` / `.output` and then insert or interpolate those values back into a terminal (`echo`, prompts, or another CLI's arguments), crafted filenames may affect terminal display. Downstream scripts should sanitize at the terminal boundary (for example with jq's `gsub` or a wrapping shell tool). The CLI's own human-readable output (without `--json`) already passes every print site through `sanitize_for_display` and is not affected.

### 诊断输出 | Diagnostics

`hdr` / `shift` / `embed` / `rename` 支持 `--diagnose[=summary|full]`。`--diagnose` 与 `--diagnose=summary` 等价，会在命令完成后附加紧凑诊断；`--diagnose=full` 会列出逐文件细节，`embed` 还会列出字体解析层级（本次传入的字体源、持久化缓存、系统字体）和缓存状态。`chain` 与 `refresh-fonts` 不支持 `--diagnose`，传入会报错而不是静默忽略。

`hdr` / `shift` / `embed` / `rename` support `--diagnose[=summary|full]`. `--diagnose` and `--diagnose=summary` are equivalent and attach compact diagnostics after the command finishes; `--diagnose=full` lists per-file details, and `embed` also lists font-resolution tiers (current run sources, persistent cache, system fonts) plus cache status. `chain` and `refresh-fonts` do not support `--diagnose`; passing it errors instead of being silently ignored.

`diagnose-fonts` 是独立的详细诊断命令，默认输出 verbose 报告，而且只读：不写输出字幕、不刷新或修改字体缓存。它接受字幕输入和字体解析选项：`--font-dir`、`--font-file`、`--no-system-fonts`、`--no-cache`、`--cache-file`、`--lang`、`--json`。需要确认字体文件是否真的能被子集化时，可显式加入 `--subset-check`；该检查只在内存中运行，不写出字幕。

`diagnose-fonts` is the standalone detailed diagnostic command. It is verbose by default and read-only: it does not write output subtitles and does not refresh or mutate the font cache. It accepts subtitle inputs plus font-resolution options: `--font-dir`, `--font-file`, `--no-system-fonts`, `--no-cache`, `--cache-file`, `--lang`, and `--json`. Add `--subset-check` only when you want to verify that resolved font files can actually be subset; the check runs in memory and does not write subtitles.

`diagnose-fonts` 和带 `--diagnose` 的 `embed` 还会输出 package-level Font QA 状态：`complete`、`incomplete` 或 `blocked`。`complete` 表示已检查文件里的字体引用全部解析成功；`incomplete` 表示有缺失字体、警告或被跳过的可选子集化检查；`blocked` 表示文件诊断失败、字体解析错误，或 `--subset-check` 明确失败。

`diagnose-fonts` and `embed --diagnose` also report a package-level Font QA status: `complete`, `incomplete`, or `blocked`. `complete` means all inspected font references resolved; `incomplete` means missing fonts, warnings, or skipped optional subset checks remain; `blocked` means a file diagnostic failed, a font resolution errored, or `--subset-check` explicitly failed.

为避免诊断命令在异常字幕/字体组合上长时间运行，`--subset-check` 有命令级预算：最多 128 次子集化调用，累计子集输出约 100 MiB；超出后剩余检查会标记为 skipped，并输出一次 budget exhausted 警告。

To keep diagnostics bounded on unusual subtitle/font combinations, `--subset-check` uses one command-level budget: up to 128 subset calls and about 100 MiB of cumulative subset output. After that, remaining checks are marked skipped and one budget-exhausted warning is emitted.

```bash
# 附加紧凑诊断 / Attach compact diagnostics
ssahdrify-cli embed --diagnose input.ass

# 附加完整诊断 / Attach full diagnostics
ssahdrify-cli embed --diagnose=full --font-dir "<font-folder>" input.ass

# 只诊断字体解析，不写字幕 / Diagnose font resolution only, no subtitle writes
ssahdrify-cli diagnose-fonts --font-dir "<font-folder>" input.ass

# 额外检查已解析字体能否子集化 / Also check whether resolved fonts can be subset
ssahdrify-cli diagnose-fonts --subset-check --font-dir "<font-folder>" input.ass

# 下游打包必须完整嵌入字体时推荐 / Recommended when downstream packaging requires every font
ssahdrify-cli embed --font-dir "<font-folder>" --on-missing fail --fail-fast --diagnose input.ass
```

`embed` 默认仍使用 `--on-missing warn`：能嵌入的字体会继续嵌入，缺失或子集化失败的字体会变成 warning。此时输出文件可能已经写出，但 summary 会明确显示 `written with warnings / incomplete`，避免把部分成功误读成“全部字体都成功”。

`embed` still defaults to `--on-missing warn`: fonts that can be embedded are embedded, and missing or failed-to-subset fonts become warnings. In that case the output file may be written, but the summary explicitly says `written with warnings / incomplete` so partial success is not mistaken for “all fonts succeeded.”

### 字体缓存 | Font Cache

`embed` 每次启动通常都要扫描每个 `--font-dir` 下的字体文件，构建查找表（一般几秒到几十秒；5000+ 字体可能需要几分钟）。**持久化字体缓存**能将这一过程变为一次性操作：先运行 `refresh-fonts`，将字体元数据写入磁盘上的 SQLite 文件；之后 `embed` 会在缓存仍有效时直接复用它，跳过扫描。对于字幕组按集批量处理尤其有用。

The `embed` subcommand normally rescans every `--font-dir` on each invocation to build its lookup table (usually seconds to tens of seconds; minutes for 5000+ font collections). The **persistent font cache** makes that process a one-time step: run `refresh-fonts` to write font metadata into a SQLite file on disk, then later `embed` calls reuse it while the cache is still valid. This is especially useful for fan-sub teams processing episodes in batches.

#### 工作流 | Workflow

```bash
# 一次性扫描字体目录，构建缓存 / Scan once to build the cache
ssahdrify-cli refresh-fonts --font-dir "<anime-font-folder>" --font-dir "<latin-font-folder>"

# 后续 embed 自动复用缓存（不再扫描） / Subsequent embed uses cache (no scan)
ssahdrify-cli embed input.ass

# 也可以继续加 --font-dir，临时合并额外字体源（缓存 + 额外目录） /
# You can still pass --font-dir to merge extra dirs with the cache
ssahdrify-cli embed --font-dir "<project-font-folder>" input.ass

# 本次强制不用缓存 / Force no-cache for one run
ssahdrify-cli --no-cache embed --font-dir "<font-folder>" input.ass

# 字体目录变更后刷新缓存 / Refresh cache after fonts change
ssahdrify-cli refresh-fonts --font-dir "<anime-font-folder>" --font-dir "<latin-font-folder>"
```

#### 缓存位置 | Cache Location

默认位置按操作系统决定（与 GUI 缓存独立，避免锁竞争）：

- Windows: `%APPDATA%/ssahdrify/cli_font_cache.sqlite3`
- macOS: `$HOME/Library/Application Support/ssahdrify/cli_font_cache.sqlite3`
- Linux: `${XDG_DATA_HOME:-$HOME/.local/share}/ssahdrify/cli_font_cache.sqlite3`

`--cache-file <PATH>` 可改用指定路径。

Default locations are OS-specific and separate from the GUI cache to avoid lock contention:

- Windows: `%APPDATA%/ssahdrify/cli_font_cache.sqlite3`
- macOS: `$HOME/Library/Application Support/ssahdrify/cli_font_cache.sqlite3`
- Linux: `${XDG_DATA_HOME:-$HOME/.local/share}/ssahdrify/cli_font_cache.sqlite3`

Use `--cache-file <PATH>` to choose a different path.

#### 漂移检测 | Drift Detection

`embed` 启动时会对缓存做轻量校验：对每个已缓存文件夹执行一次 `stat()`，检查 mtime 是否变化。如果发现漂移（说明你添加 / 删除 / 替换 / 重命名了字体文件），CLI 会在 stderr 列出发生变化的文件夹，本次运行自动退回无缓存模式（使用 `--font-dir` 或系统字体），并提示你运行 `refresh-fonts` 更新。**缓存不会被静默重建**——缓存写入必须由 `refresh-fonts` 显式触发。

`embed` runs a lightweight cache validation at startup: one `stat()` per cached folder checks for mtime drift. If drift is detected (you added / deleted / replaced / renamed font files), the CLI lists the changed folders on stderr, falls back to no-cache for this run (using `--font-dir` or system fonts), and tells you to run `refresh-fonts`. **The cache is never silently rebuilt** — cache writes are always explicit via `refresh-fonts`.

#### 限制 | Limitations

- 每个 `--font-dir` 只扫描一层（不递归），与 `embed --font-dir` 语义一致。树状字体目录需要逐层显式传入。
- 字体缓存最多记录 256 个源文件夹；更大的字体树请先整理为更少的叶子目录，或拆成多个缓存文件使用。
- 单个缓存来源最多安全写入约 **20,000 个 font faces**；超过时 `refresh-fonts` 会跳过该来源，GUI 本次扫描可继续使用会话索引，但不会为该超大来源写入持久化缓存。
- 单个字体文件的扫描/子集化读取上限为 **64 MiB**；超过会被拒绝并报告错误。
- GUI 和 CLI 各自使用独立缓存文件，避免 SQLite 锁竞争；同一个可执行文件同时只会读写一个缓存文件（默认路径或 `--cache-file` 覆盖路径）。`chain` v1 暂不读取缓存（其中的 embed 步始终使用显式 `--font-dir` 或系统字体）。
- 跨版本不会自动迁移缓存结构；版本不匹配时，CLI 会明确提示删除缓存文件并重新运行 `refresh-fonts`。

- Each `--font-dir` is scanned one level deep (non-recursive), matching `embed --font-dir` semantics. Pass each leaf folder explicitly for tree-shaped collections.
- The font cache tracks at most 256 source folders. For larger font trees, organize fonts into fewer leaf folders or split work across separate cache files.
- A single cached source can safely persist about **20,000 font faces**. If it exceeds that cap, `refresh-fonts` skips that source, while the GUI can still use the current session index but will not persist acceleration for that oversized source.
- A single font file is capped at **64 MiB** for scanning/subsetting; larger files are refused with an error.
- GUI and CLI use separate cache files to avoid SQLite lock contention; a single binary opens exactly one cache at a time (default path or `--cache-file` override). `chain` v1 does not consult the cache; its embed step always uses explicit `--font-dir` or system fonts.
- There is no automatic schema migration across releases. Version mismatch surfaces as an explicit prompt to delete the cache file and rerun `refresh-fonts`.

---

## 使用场景 | Background

SSA/ASS 字幕自身不带色彩空间元数据，渲染器通常会按 SDR 处理，结果是字幕在 HDR 画面里显得过饱和、过亮。播放 HDR 视频时，显示设备会进入 HDR 模式，但字幕仍按 SDR 混合，色差就来自这里。

SSA/ASS subtitles do not carry color-space metadata, so renderers usually treat them as SDR content, making subtitles look oversaturated and overly bright in HDR video. When an HDR video plays, the display enters HDR mode, but subtitles are still blended as SDR; that is where the color mismatch comes from.

> 如果你的播放器已经能正确处理字幕亮度（例如 mpv 的 `blend-subtitles=video`，或 madVR 配合 xy-SubFilter 的字幕色彩管理），则不需要本工具。
>
> If your player already handles subtitle brightness correctly (e.g. mpv with `blend-subtitles=video`, or madVR with xy-SubFilter color management), you don't need this tool.

_相关讨论 / Related discussion: [libass/libass#297](https://github.com/libass/libass/issues/297)_

相关工具 / Related tool: [arition/SubRenamer](https://github.com/arition/SubRenamer) 也是视频与字幕重命名工作流的一个选择（按字母序 + 下标配对）。本项目的批量重命名功能（Tab 4）则使用面向字幕组命名习惯的正则配对流程，代码独立实现。

For subtitle-and-video rename workflows, [arition/SubRenamer](https://github.com/arition/SubRenamer) is another option (alphabetical + index pairing). This project's Batch Rename feature (Tab 4) uses an independently implemented regex pairing flow built around common fan-sub naming patterns.

---

## HDR 转换原理 | How HDR Conversion Works

```
SSA/ASS 字幕颜色 (sRGB)
├─ 1. sRGB → rec2100-linear（Color.js 色彩空间转换）
├─ 2. 亮度缩放：Y × (targetBrightness / 203)
├─ 3. rec2100-linear → rec2100pq 或 rec2100hlg
└─ 4. 输出 RGB
```

```
SSA/ASS subtitle colors (sRGB)
├─ 1. sRGB → rec2100-linear (Color.js color space conversion)
├─ 2. Luminance scaling: Y × (targetBrightness / 203)
├─ 3. rec2100-linear → rec2100pq or rec2100hlg
└─ 4. Output RGB
```

### 精度说明 | Accuracy Note

PQ 模式已验证与 Python 原版（colour-science）逐像素一致。HLG 模式使用手动实现的 BT.2100 逆 OOTF + OETF（绕过 Color.js 的 rec2100hlg 空间），同样与 Python 原版完全一致。

PQ mode is verified pixel-exact against the Python version (colour-science). HLG mode uses a manually implemented BT.2100 inverse OOTF + OETF (bypassing Color.js's rec2100hlg space) and also matches the Python version exactly.

由于字幕混合链路和 HDR 显示环境很复杂（HDMI 元数据协商、显示器色调映射等），实际效果主要保证“红还是红、蓝还是蓝”的基础观感，不适合严格校色场景。

Due to the complexity of subtitle blending pipelines and HDR display environments (HDMI metadata negotiation, display tone mapping, etc.), the result is intended to preserve basic color identity, not to satisfy strict color-accuracy or color-grading requirements.

---

## 从源码构建 | Build from Source

### 前置条件 | Prerequisites

- [Node.js](https://nodejs.org/) (v20+)
- [Rust 工具链 / Rust toolchain](https://rustup.rs/) (1.77.2+)
- Windows: WebView2 (Windows 10/11 已预装 / pre-installed on Windows 10/11)
- macOS / Linux: 参考 / see [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)

### 开发 | Development

```bash
cd ssaHdrify-tauri
npm install
npm run tauri dev
```

### 构建 | Production Build

```bash
# GUI portable executable
npm run tauri build

# CLI portable executable
npm run build:cli

# Build both GUI and CLI
npm run build:all
```

在 Windows 上，便携式 exe 会生成到 `src-tauri/target/release/`，可直接运行，无需安装。`tauri.conf.json` 目前设置了 `bundle.active: false`，因此默认生成便携式二进制文件，而不是安装包。

On Windows, portable executables are produced under `src-tauri/target/release/` and can be run directly, with no install step required. `tauri.conf.json` currently sets `bundle.active: false`, so the default output is portable binaries rather than installers.

Expected Windows release build outputs:

```text
src-tauri/target/release/ssahdrify.exe
src-tauri/target/release/ssahdrify-cli.exe
```

### 测试 | Testing

```bash
npm run test:run                                  # 前端单元测试 / Frontend unit tests
cargo test --manifest-path src-tauri/Cargo.toml   # Rust 后端测试 / Rust backend tests
```

> `npm test` 默认进入 watch 模式（开发用）；`npm run test:run` 是单次运行。
>
> `npm test` defaults to watch mode (development); use `npm run test:run` for a single-pass run.

---

## 架构 | Architecture

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  Shared TypeScript engine                                                    │
│  - 4 features: HDR Convert, Time Shift, Font Embed, Batch Rename             │
│  - Color.js (PQ/HLG color math), ass-compiler (font collection)              │
│  - Custom subtitle parser, fan-sub regex pairing engine                      │
└──────────────┬───────────────────────────────────────┬───────────────────────┘
               │                                       │
   imported as React modules              bundled via esbuild (IIFE)
               │                                       │
┌──────────────┴───────────────┐ ┌─────────────────────┴───────────────────────┐
│  GUI binary                  │ │  CLI binary                                 │
│  ssahdrify.exe               │ │  ssahdrify-cli.exe                          │
│                              │ │                                             │
│  Tauri 2 + React +           │ │  clap (argv parsing)                        │
│  Tailwind frontend           │ │  deno_core / V8 (embedded JS bundle)        │
│  - 4 tabs                    │ │  - feature and utility subcommands          │
│  - i18n (zh/en),             │ │  - JSON reports + font diagnostics          │
│    dark/light/auto theme     │ │  - env_logger (stderr warnings)             │
│  - FontSourceModal UI        │ │  - sys-locale (--lang auto)                 │
└──────────────┬───────────────┘ └─────────────────────┬───────────────────────┘
               │                                       │
           Tauri IPC                       execute_script + JSON
               │                                       │
               └────────────────────┬──────────────────┘
                                    │
┌───────────────────────────────────┴──────────────────────────────────────────┐
│  Shared Rust crates                                                          │
│  - font-kit (system font discovery + matching)                               │
│  - fontcull stack (subsetting + name-table reader)                           │
│  - chardetng + encoding_rs (encoding detection + conversion)                 │
│  - serde / serde_json (serialization)                                        │
│  - rusqlite (font cache + user font index)                                   │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## 致谢 | Credits

- 原项目 / Original project: [ying](https://github.com/ying) (2021), [gky99/ssaHdrify](https://github.com/gky99/ssaHdrify) (2024-2025)
- <a href="https://www.flaticon.com/free-icons/hdr" title="hdr icons">Hdr icons created by Freepik - Flaticon</a>

---

## 许可证 | License

Copyright (C) 2021 ying  
Copyright (C) 2024-2025 gky99  
Copyright (C) 2026 koagaroon

本项目采用 [GNU 通用公共许可证 v3.0 或更高版本](LICENSE) 授权。

This project is licensed under the [GNU General Public License v3.0 or later](LICENSE).

### 来源与衍生作品 | Origin and Derivative Work

本项目是 [ssaHdrify](https://github.com/gky99/ssaHdrify) 的 Tauri 桌面重写版，原项目由 ying (2021) 创建，后由 gky99 (2024-2025) 维护。原项目同样采用 GPL-3.0 授权。

This is a Tauri desktop rewrite of [ssaHdrify](https://github.com/gky99/ssaHdrify),
originally created by ying (2021) and later maintained by gky99 (2024-2025).
The original project is also licensed under GPL-3.0.

HDR 色彩转换算法由 TypeScript（基于 [Color.js](https://colorjs.io/)）重新实现，方案参考了 Python 原版（使用 [colour-science](https://www.colour-science.org/)）。没有逐字复制代码；实现本身是新的，但按许可证语境仍按衍生作品处理。

The HDR color conversion algorithm was reimplemented in TypeScript (using
[Color.js](https://colorjs.io/)) based on the approach in the Python version
(which used [colour-science](https://www.colour-science.org/)). No code was
copied verbatim; the implementation is new, but the project is treated as a
derivative work for license purposes.

### 算法归属 | Algorithm Attribution

`src/features/font-embed/font-collector.ts` 中的字体收集算法受 [Aegisub](https://github.com/Aegisub/Aegisub) 的 FontCollector 设计（BSD-3-Clause）启发。未复制 Aegisub 代码，实现为本项目原创 TypeScript。

The font collection algorithm in `src/features/font-embed/font-collector.ts`
is inspired by [Aegisub](https://github.com/Aegisub/Aegisub)'s FontCollector
design (BSD-3-Clause). No Aegisub code was copied; the implementation is
original TypeScript written for this project.

### 第三方依赖 | Third-Party Dependencies

下表列出主要直接依赖和随应用分发的资产；完整的传递依赖链请分别参见 `package-lock.json` 和 `src-tauri/Cargo.lock`。

The tables below list the main direct dependencies and bundled assets. For the complete transitive dependency chain, see `package-lock.json` and `src-tauri/Cargo.lock`, respectively.

#### 运行时依赖（随应用分发）| Runtime (shipped with the application)

| 组件 / Component                                                             | 许可证 / License                     | 用途 / Usage                                                                                                   |
| ---------------------------------------------------------------------------- | ------------------------------------ | -------------------------------------------------------------------------------------------------------------- |
| [Tauri](https://tauri.app/)                                                  | Apache-2.0 OR MIT                    | 桌面应用框架 / Desktop app framework                                                                           |
| [Tauri plugins](https://v2.tauri.app/plugin/)                                | Apache-2.0 OR MIT                    | 对话框、文件访问和日志插件 / Dialog, filesystem, and logging plugins                                           |
| [React](https://react.dev/) / React DOM                                      | MIT                                  | UI 框架 / UI framework                                                                                         |
| [React Window](https://github.com/bvaughn/react-window)                      | MIT                                  | 大列表虚拟滚动 / Virtualized large lists                                                                       |
| [Color.js](https://colorjs.io/)                                              | MIT                                  | HDR 色彩空间转换 (PQ/HLG) / HDR color space conversion                                                         |
| [ass-compiler](https://github.com/weizhenye/ass-compiler)                    | MIT                                  | ASS 字幕解析（字体收集）/ ASS subtitle parsing for font collection                                             |
| [font-kit](https://github.com/servo/font-kit)                                | MIT OR Apache-2.0                    | 跨平台系统字体发现 (Rust) / Cross-platform system font discovery                                               |
| [fontcull](https://github.com/bearcove/fontcull)                             | MIT / MIT OR Apache-2.0              | 字体子集化（含 fontcull-klippa、fontcull-skrifa）/ Font subsetting (includes fontcull-klippa, fontcull-skrifa) |
| [chardetng](https://github.com/hsivonen/chardetng)                           | MIT OR Apache-2.0                    | 编码检测 (Firefox 引擎) / Encoding detection (Firefox's engine)                                                |
| [encoding_rs](https://github.com/hsivonen/encoding_rs)                       | (Apache-2.0 OR MIT) AND BSD-3-Clause | 编码转换 / Encoding conversion                                                                                 |
| [rusqlite](https://github.com/rusqlite/rusqlite)                             | MIT                                  | 字体缓存和本地字体索引 / Font cache and local font index                                                       |
| [serde](https://serde.rs/) / serde_json                                      | MIT OR Apache-2.0                    | Rust 序列化 / Rust serialization                                                                               |
| [deno_core](https://github.com/denoland/deno)                                | MIT                                  | 嵌入式 V8 JS 运行时（CLI）/ Embedded V8 JS runtime (CLI)                                                       |
| [V8](https://v8.dev/)                                                        | BSD-3-Clause                         | JavaScript 引擎（经 deno_core 嵌入，CLI）/ JavaScript engine via deno_core (CLI)                               |
| [clap](https://github.com/clap-rs/clap)                                      | MIT OR Apache-2.0                    | CLI 参数解析（CLI）/ CLI argument parsing (CLI)                                                                |
| [env_logger](https://github.com/rust-cli/env_logger)                         | MIT OR Apache-2.0                    | CLI 日志后端 stderr（CLI）/ CLI logging backend on stderr (CLI)                                                |
| [sys-locale](https://github.com/1Password/sys-locale)                        | MIT OR Apache-2.0                    | OS 区域设置检测（驱动 `--lang` 自动检测，CLI）/ OS locale detection driving `--lang` auto (CLI)                |
| [base64](https://github.com/marshallpierce/rust-base64)                      | MIT OR Apache-2.0                    | Rust 侧字体载荷 base64 编码 / Base64 encoding for Rust-side font payloads                                      |
| [unicode-normalization](https://github.com/unicode-rs/unicode-normalization) | MIT OR Apache-2.0                    | Unicode 路径 / 输出键规范化 / Unicode path and output-key normalization                                        |
| [rfd](https://github.com/PolyMeilex/rfd)                                     | MIT                                  | 启动失败时的原生错误对话框 / Native error dialog for startup failures                                          |

#### 捆绑字体（随应用分发）| Bundled Fonts (shipped with the application)

| 字体 / Font                                                                                                                   | 许可证 / License                                                                | 用途 / Usage                                                                                    |
| ----------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| [Inter](https://rsms.me/inter/) · © The Inter Project Authors                                                                 | [SIL Open Font License 1.1](src/assets/fonts/inter/LICENSE.txt) · OFL-1.1       | 英文界面正文与标题 / English UI body + display face                                             |
| [Smiley Sans 得意黑](https://github.com/atelier-anchor/smiley-sans) · © 2022–2024 [atelierAnchor](https://atelier-anchor.com) | [SIL Open Font License 1.1](src/assets/fonts/smiley-sans/LICENSE.txt) · OFL-1.1 | 中文界面标题展示字体（仅用于标题）/ Chinese-mode application title display face (headline only) |

> OFL-1.1 允许这些字体与任何软件一起捆绑、嵌入和再分发，包括 GPL-3.0 项目；字体及其衍生作品必须继续以 OFL 授权，不得单独销售，且修改版本不得使用其保留字体名称。
>
> OFL-1.1 allows these fonts to be bundled, embedded, and redistributed alongside any software, including GPL-3.0 projects. The fonts and their derivatives must remain licensed under OFL, must not be sold on their own, and modified versions must not use the Reserved Font Names (`Inter`, `Smiley`, `得意黑`).

#### 构建时依赖（不随应用分发）| Build-time only (not shipped)

| 组件 / Component                                   | 许可证 / License  | 用途 / Usage                                                           |
| -------------------------------------------------- | ----------------- | ---------------------------------------------------------------------- |
| [Tailwind CSS](https://tailwindcss.com/)           | MIT               | CSS 工具框架 / CSS utility framework                                   |
| [TypeScript](https://www.typescriptlang.org/)      | Apache-2.0        | 类型检查 / Type checking                                               |
| [Vite](https://vite.dev/)                          | MIT               | 构建工具 / Build tool                                                  |
| [Tauri CLI](https://tauri.app/)                    | MIT OR Apache-2.0 | Tauri 构建入口 / Tauri build entry point                               |
| [ESLint](https://eslint.org/)                      | MIT               | 代码检查 / Linting                                                     |
| [Stylelint](https://stylelint.io/)                 | MIT               | CSS 代码检查 / CSS linting                                             |
| [Prettier](https://prettier.io/)                   | MIT               | 代码格式化 / Code formatter                                            |
| [Vitest](https://vitest.dev/)                      | MIT               | 单元测试 / Unit testing                                                |
| [js-base64](https://github.com/dankogai/js-base64) | BSD-3-Clause      | 测试侧 base64 wire-format 编码 / Test-side base64 wire-format encoding |
| [esbuild](https://esbuild.github.io/)              | MIT               | 为 CLI 嵌入打包 engine.js / Bundles engine.js for CLI embedding        |
