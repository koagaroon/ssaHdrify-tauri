import { isCaseInsensitiveFs, isWindowsRuntime } from "./platform";
import { hasUnicodeControls } from "./unicode-controls";

/**
 * Shared output-path validation helpers.
 *
 * Extracted from `output-naming.ts`'s HDR resolver so that Shift and
 * Embed (CLI + GUI sides) can apply the same safety checks. Before
 * this extraction, HDR's resolver was the canonical implementation
 * and the others rolled their own minimal byte-illegal-char checks —
 * `CON.ass`, `..` segments, MAX_PATH overflow, drive-relative paths
 * passed through. Aligning them here closes a real-but-narrow attack
 * surface (CLI receives untrusted argv; GUI takes user-typed
 * templates) and gives consistent error messaging across all three
 * commands.
 *
 * The HDR resolver still owns its `.ass` extension check and template-
 * variable substitution; this module only covers the per-filename and
 * per-path safety checks that are common to all three.
 */

// ── Windows reserved names ─────────────────────────────────
// Forbidden on Windows regardless of extension (NT object-namespace
// reservations: legacy device names that the kernel routes specially).
//
// Source: https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file
// Includes COM0–COM9, LPT0–LPT9, plus the ISO 8859-1 superscript-digit
// variants (COM¹/²/³ and LPT¹/²/³) that current Windows recognizes as
// device aliases.
//
// CONIN$ / CONOUT$ are runtime Win32 console aliases (Global?? namespace)
// rather than always-reserved device names; included defensively because
// they collide with Win32 conventions in practice.
export const WINDOWS_RESERVED_NAMES = new Set([
  "CON",
  "PRN",
  "AUX",
  "NUL",
  "COM0",
  "COM1",
  "COM2",
  "COM3",
  "COM4",
  "COM5",
  "COM6",
  "COM7",
  "COM8",
  "COM9",
  "COM¹", // COM¹
  "COM²", // COM²
  "COM³", // COM³
  "LPT0",
  "LPT1",
  "LPT2",
  "LPT3",
  "LPT4",
  "LPT5",
  "LPT6",
  "LPT7",
  "LPT8",
  "LPT9",
  "LPT¹", // LPT¹
  "LPT²", // LPT²
  "LPT³", // LPT³
  "CONIN$",
  "CONOUT$",
]);

/**
 * Match characters illegal in NTFS filenames. Includes control chars
 * (which Windows would silently truncate at) plus the explicit NTFS-
 * reserved punctuation and path separators (so a "filename" with a
 * separator can't sneak through).
 *
 * Control range `\x00-\x1f` covers C0; `\x7f-\x9f` covers DEL and C1.
 * Rust-side `char::is_control()` (used by `validate_ipc_path` and
 * `validate_font_family`) rejects all of Cc; matching the same span
 * here keeps TS↔Rust parity (Round 8 A-R8-N4-4 / N4-5).
 *
 * Cross-platform note: `:` is technically valid on macOS / Linux but
 * we reject it everywhere — this app's primary platform is Windows
 * and outputs cross machines, so the strictest filesystem's rules win.
 */
// `{` and `}` are rejected too (NTFS allows them) so that templates
// with unrecognized tokens — e.g., `{Format}` typed instead of
// `{format}` — surface as filename errors rather than producing a
// literal `episode.{Format}.ass` file. The substitution path is
// case-sensitive; rejecting brace literals turns typos into errors.
// eslint-disable-next-line no-control-regex -- intentional: reject control chars in filenames
export const ILLEGAL_FILENAME_CHARS = /[\x00-\x1f\x7f-\x9f<>:"|?*\\/{}]/;

// Round 10 N-R10-005: BatchRename's `deriveRenameOutputPath` builds
// the output name from the user's verbatim video filename (no
// template machinery in play), so legitimate fan-sub video names
// like `[Group] Show {1080p}.mkv` would otherwise trip the brace
// reject above and force the user to rename the source file by hand.
// This sibling pattern omits `{` and `}` for the BatchRename
// callsite; every other consumer (HDR/Shift/Embed/chain template
// resolvers) keeps the strict variant so typo'd tokens surface as
// errors instead of literal `{Format}` filenames.
// eslint-disable-next-line no-control-regex -- intentional: reject control chars in filenames
const ILLEGAL_FILENAME_CHARS_BRACES_OK = /[\x00-\x1f\x7f-\x9f<>:"|?*\\/]/;

/**
 * Decomposed parts of a validated input path.
 */
export interface InputPathParts {
  /** Directory portion with no trailing slash. For files at a drive root
   *  like `C:\file.ass`, this is `C:` (drive letter only). Downstream
   *  concatenation `${dir}/${filename}` produces a correct rooted path. */
  dir: string;
  /** Filename without extension. */
  baseName: string;
  /** File extension WITH leading dot (e.g., `.ass`); empty string when no
   *  extension is present. */
  ext: string;
  /** Path with all backslashes converted to forward slashes. */
  normalized: string;
  /** Whether the input used Windows-style backslashes — caller restores
   *  native separators on the returned output path. */
  usedBackslash: boolean;
}

/**
 * Decompose an absolute input path into directory, base name, extension,
 * and separator-style parts. The single source of truth for what counts
 * as a valid root path across HDR / Shift / Embed resolvers (CLI + GUI).
 *
 * Accepts:
 *   - Drive-rooted Windows paths: `C:\foo\bar.ass`, `C:/foo/bar.ass`
 *   - Drive-root files: `C:\bar.ass`, `Z:/bar.ass` (dir = `C:` / `Z:`)
 *   - POSIX absolute paths: `/foo/bar.ass`
 *   - UNC paths: `\\server\share\bar.ass`
 *
 * Rejects:
 *   - Bare filenames: `foo.ass` (no directory at all)
 *   - Drive-relative: `C:foo.ass` (drive letter without separator —
 *     ambiguous; on Windows refers to the file in drive C's *current*
 *     directory, which has no defined meaning at this layer)
 *   - Empty / invalid stems
 *   - Control characters anywhere in the path
 *
 * Why drive-root files (`C:\file.ass`) MUST be accepted: a CLI user may
 * cd into a drive root and pass a bare filename; the Rust shell then
 * canonicalizes argv against cwd, producing a drive-rooted absolute
 * path. Earlier HDR-resolver code rejected `dir === "C:"` thinking it
 * was drive-relative; that rejection caught the legitimate drive-root
 * case along with the ambiguous one. This helper distinguishes them by
 * checking for the separator after the colon BEFORE splitting dir.
 *
 * Why bare filenames must be rejected even though Rust shell
 * canonicalizes argv: programmatic callers (tests, future internal
 * modules) may bypass the shell. The engine layer enforces its own
 * preconditions.
 */
export function decomposeInputPath(inputPath: string): InputPathParts {
  if (!inputPath) {
    throw new Error("Input path must be absolute");
  }
  // ANY backslash → output uses backslashes — but ONLY on Windows.
  // On POSIX, `\` is a valid filename character (Codex 8850ede7 /
  // edb0e74f), not a path separator; treating it as one normalizes
  // `/home/u/ep\01.srt` to `/home/u/ep/01.srt` and replaces output
  // slashes with backslashes, producing `\home\u\ep\01.shifted.srt`
  // which is a relative filename rather than a path under /home/u.
  // Mixed-separator Windows paths (a `\\server\share/file.ass` from
  // upstream JS normalization) still bias to native style on Windows.
  const usedBackslash = isWindowsRuntime && inputPath.includes("\\");
  const normalized = usedBackslash ? inputPath.replace(/\\/g, "/") : inputPath;

  // Reject control / NUL chars early. Windows would silently truncate
  // at NUL — `evil\0.exe.ass` becomes `evil`, bypassing the trailing
  // `.ass` extension allow-list. Range covers C0 (`\x00-\x1f`), DEL
  // and C1 (`\x7f-\x9f`); Rust's `char::is_control()` rejects the same
  // span so the gate stays TS↔Rust symmetric (Round 8 A-R8-N4-4).
  // eslint-disable-next-line no-control-regex -- intentional: reject control chars
  if (/[\x00-\x1f\x7f-\x9f]/.test(normalized)) {
    throw new Error("Input path contains control characters");
  }
  // Reject BiDi / zero-width controls. These slip past the C0/DEL
  // class above (which is Cc-only) and are the Trojan-Source class
  // (CVE-2021-42574): a filename like `EP01<U+202E>cssa.ass` displays
  // as `EP01ssa.shifted.ass` after the RLO flip but lands on disk
  // verbatim. Symmetric with Rust-side `validate_font_family` /
  // `validate_ipc_path` rejection sets — see `unicode-controls.ts`
  // for the codepoint enumeration.
  if (hasUnicodeControls(normalized)) {
    throw new Error("Input path contains invisible or bidi-control characters");
  }

  // Reject `..` path components (Round 6 Wave 6.2 parity sweep).
  // The Rust `validate_ipc_path` already rejects parent-directory
  // segments at IPC entry (Round 5 Wave 5.1 fix), so any path that
  // round-trips through the backend is bounded. But the TS engine
  // path consumes `decomposeInputPath` results directly for output-
  // template derivation BEFORE round-tripping — a raw
  // `C:/Allowed/../Denied/file.ass` would derive an output under
  // `C:/Denied/` and only get caught when that output path itself
  // round-trips. Reject at TS entry as defense-in-depth, symmetric
  // with the Rust backstop. Detect `..` as a path COMPONENT, not as
  // a substring — `foo..bar.ass` is a legitimate filename.
  const hasDotDotSegment = normalized.split("/").some((seg) => seg === "..");
  if (hasDotDotSegment) {
    throw new Error("Input path contains parent-directory segments");
  }

  // Absolute = (a) starts with `/` (POSIX root or UNC after backslash
  // conversion), or (b) drive letter + separator. Drive-relative
  // `C:foo.ass` (no separator after colon) fails this check —
  // intentional: that path shape has no defined meaning at this layer.
  const isAbsolute = normalized.startsWith("/") || /^[A-Za-z]:\//.test(normalized);
  if (!isAbsolute) {
    throw new Error("Input path must be absolute");
  }

  const lastSlash = normalized.lastIndexOf("/");
  // Defensive — isAbsolute guarantees a slash exists. Guard against
  // edge inputs slipping past the absolute check.
  if (lastSlash < 0) {
    throw new Error("Input path has no directory component");
  }

  const dir = normalized.slice(0, lastSlash);
  const fullName = normalized.slice(lastSlash + 1);
  if (!fullName) {
    throw new Error("Input path has no filename");
  }

  const lastDot = fullName.lastIndexOf(".");
  const baseName = lastDot > 0 ? fullName.slice(0, lastDot) : fullName;
  const ext = lastDot > 0 ? fullName.slice(lastDot) : "";

  if (!baseName || !baseName.replace(/^\.+/, "").trim()) {
    throw new Error("Input filename has no valid stem");
  }

  return { dir, baseName, ext, normalized, usedBackslash };
}

/**
 * Filesystem-aware path equality. Returns true if two paths refer to
 * the same file on the runtime's filesystem.
 *
 * Why two gates, not one:
 * - **Separator normalization** is conditional on `isWindowsRuntime`. On
 *   Windows `/` and `\` are interchangeable separators; on POSIX `\` is
 *   a valid filename character (Codex edb0e74f / 8850ede7). Normalizing
 *   `EP\01.ass` to `EP/01.ass` on Linux turns a single file into a path.
 * - **Case folding** is conditional on `isCaseInsensitiveFs`. NTFS (Win)
 *   and APFS / HFS+ (macOS default) are case-insensitive; Linux ext4 /
 *   btrfs / xfs are case-sensitive. Folding case on Linux false-conflates
 *   `Episode.ass` and `episode.ass`, which legitimately are distinct
 *   files there (Codex dd2d9554).
 *
 * Use this anywhere two paths need to be compared for "is this the same
 * file" semantics: cross-tab duplicate guard, self-overwrite check,
 * output-collision pre-check.
 */
export function pathsEqualOnFs(a: string, b: string): boolean {
  const normSep = (p: string) => (isWindowsRuntime ? p.replace(/\\/g, "/") : p);
  const normCase = (p: string) => (isCaseInsensitiveFs ? p.toLowerCase() : p);
  return normCase(normSep(a)) === normCase(normSep(b));
}

/**
 * Substitute `{token}` placeholders in `template` with `vars` values.
 *
 * Two design choices, both responses to Round 1 findings:
 *
 * 1. **No `$`-interpretation on values.** A naïve
 *    `template.replace(/\{token\}/g, value)` interprets `$&`, `$'`,
 *    `` $` ``, and `$<N>` inside `value` as backreference tokens.
 *    Filenames legitimately can contain `$` (rare but valid on every
 *    supported filesystem) — and CLI argv on Windows lets a user pass
 *    paths containing `$`. This helper substitutes via `split().join()`,
 *    which treats values literally.
 *
 * 2. **Boundary-only adjacent-dot collapse.** Earlier callsites used a
 *    blanket `replace(/\.{2,}/g, ".")` post-pass to clean up artifacts
 *    like `name..ass` (from `{name}.{ext}` with `ext = ".ass"`) or
 *    `name..ass` (from an empty `{lang}` between dots in
 *    `{name}.{lang}.ass`). That post-pass also collapsed `..` INSIDE
 *    user content (baseName like `[Group]Show..special` →
 *    `[Group]Show.special`, mangling intentional double-dots in fan-sub
 *    filenames). This helper inspects each token-substitution boundary
 *    instead and trims at most one boundary dot — user content's
 *    internal dots stay intact.
 *
 * `vars` keys are token names without braces; tokens whose key is
 * missing substitute to "".
 */
export function substituteTemplate(template: string, vars: Record<string, string>): string {
  // Parse template into ordered segments — alternating literal text
  // (template structure) and value text (substituted token data).
  type Seg = { kind: "literal" | "value"; text: string };
  const segments: Seg[] = [];
  const tokenRe = /\{([a-z_][a-z0-9_]*)\}/g;
  let cursor = 0;
  let m: RegExpExecArray | null;
  while ((m = tokenRe.exec(template)) !== null) {
    if (m.index > cursor) {
      segments.push({ kind: "literal", text: template.slice(cursor, m.index) });
    }
    segments.push({ kind: "value", text: vars[m[1]] ?? "" });
    cursor = m.index + m[0].length;
  }
  if (cursor < template.length) {
    segments.push({ kind: "literal", text: template.slice(cursor) });
  }

  // Phase A: collapse `\.{2,}` runs INSIDE template literals. Catches
  // user typos like `{name}..processed.ass`. Values are never touched
  // here — `[Group]Show..special` inside a baseName keeps its `..`.
  for (const seg of segments) {
    if (seg.kind === "literal") {
      seg.text = seg.text.replace(/\.{2,}/g, ".");
    }
  }

  // Phase B: concatenate segments. At each junction, if both sides
  // contribute a dot (value ends with `.` + next literal starts with
  // `.`, or literal ends with `.` + next value starts with `.`), drop
  // one boundary dot. An empty value collapses to "no contribution":
  // its surrounding `.`s only collapse if literal-meets-literal at the
  // junction (which produces a single `..` in the literal sequence,
  // not handled here — falls through to the next iteration's boundary
  // check). Internal value dots stay verbatim.
  let out = "";
  for (const seg of segments) {
    let chunk = seg.text;
    if (chunk.startsWith(".") && out.endsWith(".")) {
      chunk = chunk.slice(1);
    }
    out += chunk;
  }
  return out;
}

/**
 * Validate a single output filename (no path separators) for safety.
 * Caller is responsible for stripping the directory portion before
 * calling.
 *
 * Throws on:
 *   - empty / whitespace-only filename
 *   - illegal characters (control / NTFS-reserved / separators)
 *   - Windows reserved name (CON, PRN, etc., case-insensitive,
 *     applied to the stem with trailing whitespace + dots stripped)
 *
 * `options.allowBraces`: when true, `{` and `}` pass the illegal-char
 * gate. Used by BatchRename where the output name is the user's
 * verbatim video filename (no template substitution in play); the
 * default strict mode keeps brace-literal output for HDR / Shift /
 * Embed / chain so typo'd template tokens (`{Format}` vs `{format}`)
 * surface as errors instead of producing a literal `{Format}` file.
 */
export function assertSafeOutputFilename(
  filename: string,
  options?: { allowBraces?: boolean }
): void {
  if (!filename.trim()) {
    throw new Error("Template resolves to empty filename");
  }
  const illegalRe = options?.allowBraces
    ? ILLEGAL_FILENAME_CHARS_BRACES_OK
    : ILLEGAL_FILENAME_CHARS;
  if (illegalRe.test(filename)) {
    throw new Error(`Output filename contains illegal characters: ${filename}`);
  }
  // Reject BiDi / zero-width controls (Round 6 Wave 6.2 parity sweep).
  // ILLEGAL_FILENAME_CHARS covers C0 + DEL + NTFS punctuation but NOT
  // the Cf bidi format codepoints (U+200E..U+202E, U+2066..U+2069,
  // U+061C) or zero-width joiners — those flow through the regex and
  // land on disk. The Rust validate_ipc_path catches them at IPC entry
  // for paths that round-trip through the backend, but a template
  // resolved entirely on the TS side and used by a future Tauri
  // dialog plugin (or any future direct write) would bypass that
  // backstop. Reject here as defense-in-depth, symmetric with
  // `decomposeInputPath`'s hasUnicodeControls check on inputs.
  if (hasUnicodeControls(filename)) {
    throw new Error(`Output filename contains invisible or bidi-control characters: ${filename}`);
  }
  // Windows reserves these names regardless of extension: per
  // Microsoft, "NUL.txt" and "NUL.tar.gz" both resolve to the NUL
  // device. So check the FIRST segment (everything before the first
  // dot), not the final stem. Also strip trailing whitespace and dots
  // because `CON ` and `CON.` resolve to the device too.
  const firstDot = filename.indexOf(".");
  const firstSegment = (firstDot > 0 ? filename.slice(0, firstDot) : filename).replace(
    /[\s.]+$/,
    ""
  );
  if (WINDOWS_RESERVED_NAMES.has(firstSegment.toUpperCase())) {
    throw new Error(`Output filename is a Windows reserved name: ${firstSegment}`);
  }
}

/**
 * Validate a full output path against the input path's directory.
 * Throws on traversal, directory escape, MAX_PATH overflow, and
 * self-overwrite.
 *
 * Both arguments may use either separator style; the helper normalizes
 * to forward slashes internally before comparing.
 */
export function assertSafeOutputPath(outputPath: string, inputPath: string): void {
  // Backslash → forward only on Windows (Round 8 A-R8-N4-7 — POSIX-
  // correctness gate, parity with `pathsEqualOnFs` and
  // `decomposeInputPath`). On POSIX `\` is a valid filename character;
  // unconditional rewriting would mangle directory-escape and
  // self-overwrite checks on legitimate POSIX paths containing `\`.
  const normalizedOutput = isWindowsRuntime ? outputPath.replace(/\\/g, "/") : outputPath;
  const normalizedInput = isWindowsRuntime ? inputPath.replace(/\\/g, "/") : inputPath;
  // Round 10 N-R10-024: ASCII C0 + DEL + C1 control-char gate on the
  // full path. `decomposeInputPath` already rejects these on inputs,
  // and `assertSafeOutputFilename` covers the filename portion — but
  // BatchRename's `deriveRenameOutputPath` builds the directory
  // portion from `dirname(videoPath)` (rename mode), `dirname(videoPath)`
  // (copy_to_video mode), or `chosenDir` directly (copy_to_chosen
  // mode) without round-tripping the dir through `decomposeInputPath`.
  // A control-char-bearing directory name would land in the output
  // path and only get caught downstream at Rust's `validate_ipc_path`
  // (or worse, if a future call bypassed Rust validation). Mirror
  // `decomposeInputPath:165` here as defense-in-depth.
  // eslint-disable-next-line no-control-regex -- intentional: reject control chars
  if (/[\x00-\x1f\x7f-\x9f]/.test(normalizedOutput)) {
    throw new Error(`Output path contains control characters: ${normalizedOutput}`);
  }
  const inputDirEnd = normalizedInput.lastIndexOf("/");
  if (inputDirEnd < 0) {
    throw new Error("Input path has no directory component");
  }
  const inputDir = normalizedInput.slice(0, inputDirEnd);

  // Path traversal — `..` as a path component, not as a substring of
  // a longer name like `..foo` (which is legal).
  if (/(^|\/)\.\.($|\/)/.test(normalizedOutput)) {
    throw new Error(`Output path contains directory traversal: ${normalizedOutput}`);
  }

  // Output must stay inside the input directory. Comparing against
  // `inputDir + "/"` avoids the `/dir1` vs `/dir12` prefix collision.
  if (!normalizedOutput.startsWith(inputDir + "/")) {
    throw new Error(`Output path escapes input directory: ${normalizedOutput}`);
  }

  // MAX_PATH check. Local long-path inputs (`\\?\C:\...`,
  // forward-normalized to `//?/C:/...`) support up to 32767 chars on
  // Windows 10+. UNC long paths (`\\?\UNC\server\share\...` →
  // `//?/UNC/...`) keep the 260 cap because the server side may not
  // support long paths. Case-insensitive UNC prefix check so a
  // lowercased `//?/unc/...` still classifies as UNC.
  // Windows MAX_PATH is 260 INCLUDING the trailing null terminator, so
  // the practical buffer-fitting limit is 259 chars. A 260-char path
  // passes a > 260 check but trips ERROR_PATH_NOT_FOUND at write time.
  // Use 259 to surface the limit with a clear error here. Long-local
  // paths get the OS extended limit (32767 incl. null → 32766 usable).
  const lower = normalizedOutput.toLowerCase();
  const isLongLocalPath = lower.startsWith("//?/") && !lower.startsWith("//?/unc/");
  const maxPathLen = isLongLocalPath ? 32766 : 259;
  if (normalizedOutput.length > maxPathLen) {
    throw new Error(`Output path too long (${normalizedOutput.length} chars, max ${maxPathLen})`);
  }

  // Self-overwrite. Filesystem-aware: case-folding only on Windows /
  // macOS where the FS is case-insensitive. On Linux ext4 / btrfs / xfs,
  // `Episode.ass` and `episode.ass` are distinct files so unconditional
  // lowercase would false-reject a legitimate `episode.ass` output when
  // the input was `Episode.ass`.
  if (pathsEqualOnFs(normalizedOutput, normalizedInput)) {
    throw new Error("Output path is the same as input (would overwrite source file)");
  }
}
