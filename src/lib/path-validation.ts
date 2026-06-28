import { normalizeOutputKey } from "./dedup-helpers";
import { isWindowsRuntime } from "./platform";
import { ASCII_CONTROL_CHARS, hasUnicodeControls, stripUnicodeControls } from "./unicode-controls";

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
 * here keeps TS↔Rust parity.
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
// Built via new RegExp so the C0/DEL/C1 range is sourced from
// `ASCII_CONTROL_CHARS` (single source of truth, no per-callsite
// eslint disable directive needed).
export const ILLEGAL_FILENAME_CHARS = new RegExp(`[${ASCII_CONTROL_CHARS}<>:"|?*\\\\/{}]`);

// BatchRename's `deriveRenameOutputPath` builds the output name from
// the user's verbatim video filename (no template machinery in
// play), so legitimate fan-sub video names like `[Group] Show
// {1080p}.mkv` would otherwise trip the brace reject above and force
// the user to rename the source file by hand. This sibling pattern
// omits `{` and `}` for the BatchRename callsite; every other
// consumer (HDR/Shift/Embed/chain template resolvers) keeps the
// strict variant so typo'd tokens surface as errors instead of
// literal `{Format}` filenames.
const ILLEGAL_FILENAME_CHARS_BRACES_OK = new RegExp(`[${ASCII_CONTROL_CHARS}<>:"|?*\\\\/]`);

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

export interface OutputDirectoryParts {
  /** Slash-normalized directory with trailing separators removed. */
  dir: string;
  /** Original directory after platform-specific separator normalization. */
  normalized: string;
  /** Whether the chosen directory used Windows-style backslashes. */
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
 *   - Current-directory or parent-directory path components (`.` / `..`)
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
  // On POSIX, `\` is a valid filename character, not a path separator;
  // treating it as one normalizes
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
  // span so the gate stays TS↔Rust symmetric.
  if (new RegExp(`[${ASCII_CONTROL_CHARS}]`).test(normalized)) {
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

  // Reject `..` path components.
  // The Rust `validate_ipc_path` already rejects parent-directory
  // segments at IPC entry, so any path that
  // round-trips through the backend is bounded. But the TS engine
  // path consumes `decomposeInputPath` results directly for output-
  // template derivation BEFORE round-tripping — a raw
  // `C:/Allowed/../Denied/file.ass` would derive an output under
  // `C:/Denied/` and only get caught when that output path itself
  // round-trips. Reject at TS entry as defense-in-depth, symmetric
  // with the Rust backstop. Detect `..` as a path COMPONENT, not as
  // a substring — `foo..bar.ass` is a legitimate filename.
  // absolute-path check runs BEFORE parent-directory-segment check.
  // A non-absolute input containing `..` (e.g., the string "../foo.ass")
  // should throw "Input path must be absolute" rather than the less
  // accurate "Input path contains parent-directory segments" — the
  // latter misattributes the root cause (caller passed a relative path)
  // by surfacing a secondary symptom (one of its components happened to
  // be `..`). Callers see "must be absolute" first; absolute paths with
  // `..` segments still get caught immediately after.

  // Absolute = (a) starts with `/` (POSIX root or UNC after backslash
  // conversion), or (b) drive letter + separator. Drive-relative
  // `C:foo.ass` (no separator after colon) fails this check —
  // intentional: that path shape has no defined meaning at this layer.
  const isAbsolute = normalized.startsWith("/") || /^[A-Za-z]:\//.test(normalized);
  if (!isAbsolute) {
    throw new Error("Input path must be absolute");
  }
  assertNoCurrentDirectorySegments(normalized, "Input path");

  const hasDotDotSegment = normalized.split("/").some((seg) => seg === "..");
  if (hasDotDotSegment) {
    throw new Error("Input path contains parent-directory segments");
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
 *   a valid filename character. Normalizing
 *   `EP\01.ass` to `EP/01.ass` on Linux turns a single file into a path.
 * - **Case folding** is conditional on `isCaseInsensitiveFs`. NTFS (Win)
 *   and APFS / HFS+ (macOS default) are case-insensitive; Linux ext4 /
 *   btrfs / xfs are case-sensitive. Folding case on Linux false-conflates
 *   `Episode.ass` and `episode.ass`, which legitimately are distinct
 *   files there.
 *
 * Use this anywhere two paths need to be compared for "is this the same
 * file" semantics: cross-tab duplicate guard, self-overwrite check,
 * output-collision pre-check.
 */
export function pathsEqualOnFs(a: string, b: string): boolean {
  // Single source of truth for the NFC + slash + case-fold pipeline:
  // delegate to `normalizeOutputKey`. Pre-consolidation both functions
  // re-implemented the same 3-step transform independently; the dedup
  // path's `normalizeOutputKey` and the self-overwrite gate's
  // `pathsEqualOnFs` could silently disagree about path identity if a
  // future Unicode-width or normalization step landed in one but not
  // the other.
  return normalizeOutputKey(a) === normalizeOutputKey(b);
}

function normalizeForPathValidation(path: string): string {
  return isWindowsRuntime ? path.replace(/\\/g, "/") : path;
}

function trimTrailingPathSeparators(path: string): string {
  return path.replace(/\/+$/, "");
}

function assertOutputPathShape(normalizedOutput: string): void {
  if (new RegExp(`[${ASCII_CONTROL_CHARS}]`).test(normalizedOutput)) {
    throw new Error(`Output path contains control characters: ${normalizedOutput}`);
  }

  if (hasUnicodeControls(normalizedOutput)) {
    throw new Error(
      `Output path contains invisible or bidi-control characters: ${normalizedOutput}`
    );
  }

  if (/(^|\/)\.\.($|\/)/.test(normalizedOutput)) {
    throw new Error(`Output path contains directory traversal: ${normalizedOutput}`);
  }
}

function assertNoCurrentDirectorySegments(normalizedPath: string, label: string): void {
  if (/(^|\/)\.($|\/)/.test(normalizedPath)) {
    throw new Error(`${label} contains current-directory segments: ${normalizedPath}`);
  }
}

function assertOutputPathLength(normalizedOutput: string): void {
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
  //
  // POSIX runtimes get PATH_MAX = 4096 (Linux's standard limit,
  // matches Rust-side `RELOCATED_PATH_MAX_LEN` cfg-gated POSIX path)
  // instead of the Windows 259. An earlier hard-coded 259 false-
  // rejected legitimate Linux paths approaching ~260 chars; the
  // per-OS branch closes the gap.
  const lower = normalizedOutput.toLowerCase();
  const isLongLocalPath = lower.startsWith("//?/") && !lower.startsWith("//?/unc/");
  let maxPathLen: number;
  if (isLongLocalPath) {
    maxPathLen = 32766;
  } else if (isWindowsRuntime) {
    maxPathLen = 259;
  } else {
    maxPathLen = 4096;
  }
  if (normalizedOutput.length > maxPathLen) {
    throw new Error(`Output path too long (${normalizedOutput.length} chars, max ${maxPathLen})`);
  }
}

function assertOutputPathInsideDirectory(normalizedOutput: string, normalizedDir: string): void {
  const dir = trimTrailingPathSeparators(normalizedDir);
  if (!dir) {
    throw new Error("Output directory is empty after normalization");
  }

  if (!normalizedOutput.startsWith(dir + "/")) {
    throw new Error(`Output path escapes output directory: ${normalizedOutput}`);
  }
}

export function decomposeOutputDirectoryPath(outputDir: string): OutputDirectoryParts {
  if (!outputDir) {
    throw new Error("Output directory must be absolute");
  }

  const usedBackslash = isWindowsRuntime && outputDir.includes("\\");
  const normalized = usedBackslash ? outputDir.replace(/\\/g, "/") : outputDir;
  assertOutputPathShape(normalized);

  const isAbsolute = normalized.startsWith("/") || /^[A-Za-z]:\//.test(normalized);
  if (!isAbsolute) {
    throw new Error("Output directory must be absolute");
  }
  assertNoCurrentDirectorySegments(normalized, "Output directory");

  const dir = trimTrailingPathSeparators(normalized);
  if (!dir) {
    throw new Error("Output directory is empty after normalization");
  }

  return { dir, normalized, usedBackslash };
}

/**
 * Substitute `{token}` placeholders in `template` with `vars` values.
 *
 * Three safety choices this helper must preserve:
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
 * 3. **Strict on unknown tokens + 32-char identifier cap.** Tokens
 *    that match the lexer but aren't in `vars` THROW instead of
 *    substituting to "" — the
 *    silent-collapse path previously turned typos like `{namE}.hdr.ass`
 *    into a hidden `.hdr.ass` filename. The 32-char identifier cap
 *    aligns the lexer with the chain validator's bound
 *    (`chain-runtime.ts::resolveChainOutputPath`); without the cap,
 *    long lowercase unknown tokens (≥33 chars) bypassed the chain
 *    validator and were silently consumed here. Lexer stays
 *    lowercase-only by design — uppercase `{NAME}` typos fall through
 *    as literal text and surface at `assertSafeOutputFilename`'s
 *    brace gate.
 *
 * Caller contract for `vars`: keys MUST match the
 * lexer's accepted shape `[a-z_][a-z0-9_]{0,31}` — keys containing
 * uppercase letters, hyphens, or other characters are unreachable
 * (the lexer never matches them, so the lookup never fires). Pass
 * lowercase ASCII keys only. The lookup itself is case-sensitive
 * exact-match: `vars["Lang"]` is NOT seen by a `{lang}` token even
 * though both name the same intent.
 */
export function substituteTemplate(template: string, vars: Record<string, string>): string {
  // Parse template into ordered segments — alternating literal text
  // (template structure) and value text (substituted token data).
  type Seg = { kind: "literal" | "value"; text: string };
  const segments: Seg[] = [];
  // Identifier bound {0,31} (32 chars total): real tokens are short
  // (`name`, `ext`, `eotf`, `format`, `video_name`, `lang`); long
  // unknown identifiers were the historical bypass vector for the
  // chain-validator.
  const tokenRe = /\{([a-z_][a-z0-9_]{0,31})\}/g;
  let cursor = 0;
  let m: RegExpExecArray | null;
  while ((m = tokenRe.exec(template)) !== null) {
    if (m.index > cursor) {
      segments.push({ kind: "literal", text: template.slice(cursor, m.index) });
    }
    const name = m[1]!;
    // Use hasOwnProperty rather than `in` so prototype-chain keys
    // (`constructor`, `toString`, `hasOwnProperty`, …) don't satisfy
    // the lookup. With `in`, `{constructor}` would skip this throw and
    // surface as a downstream TypeError when `vars[name]` returned
    // Function.prototype.constructor and `.startsWith` was called on
    // it. CLI argv and GUI input fields both flow into `template`, so
    // a user-typed `--out '{constructor}.ass'` would crash with a
    // misleading error instead of the documented unknown-token throw.
    if (!Object.prototype.hasOwnProperty.call(vars, name)) {
      throw new Error(
        `output template references unknown token '{${name}}'; ` +
          `known tokens: ${Object.keys(vars).join(", ") || "(none)"}`
      );
    }
    segments.push({ kind: "value", text: vars[name]! });
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
  let prevEmptyValue = false;
  for (const seg of segments) {
    let chunk = seg.text;
    // Drop a boundary dot when the left side already ended with one, OR when
    // the immediately-preceding segment was an EMPTY-valued token. The latter
    // catches a leading empty token (`{lang}.{name}` with lang="") whose
    // separator dot would otherwise lead the filename → a hidden-file output
    // (`.name`). Internal value dots stay verbatim.
    if (chunk.startsWith(".") && (out.endsWith(".") || prevEmptyValue)) {
      chunk = chunk.slice(1);
    }
    out += chunk;
    prevEmptyValue = seg.kind === "value" && seg.text === "";
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
  // Reject BiDi / zero-width controls (parity sweep).
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
  // `toLocaleUpperCase("en-US")` pins the upper-casing locale —
  // `toUpperCase()` honors the runtime's display locale, which on
  // Turkish hosts upcases ASCII `i` to `İ` (U+0130) and breaks
  // reserved-name lookups. None of the current reserved names
  // contain `i`, so this is forward-looking insurance against
  // future additions rather than a today-bug.
  if (WINDOWS_RESERVED_NAMES.has(firstSegment.toLocaleUpperCase("en-US"))) {
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
  const normalizedOutput = normalizeForPathValidation(outputPath);
  const normalizedInput = normalizeForPathValidation(inputPath);
  assertOutputPathShape(normalizedOutput);

  const inputDirEnd = normalizedInput.lastIndexOf("/");
  if (inputDirEnd < 0) {
    throw new Error("Input path has no directory component");
  }
  const inputDir = normalizedInput.slice(0, inputDirEnd);

  // Output must stay inside the input directory. Comparing against
  // `inputDir + "/"` avoids the `/dir1` vs `/dir12` prefix collision.
  //
  // Drive-root edge (intentional defense-in-depth gap): for a
  // drive-root input like `/file.ass`, `inputDir` is empty and this
  // check degenerates to "output starts with `/`" — any absolute
  // path passes the dir-escape gate. The `..` traversal regex above
  // still fires, so output can't traverse OUT of the implied root,
  // but it could land in any sibling directory of the drive root.
  // Defense-in-depth gap accepted: drive-root inputs are
  // pathological (a user dropping `/file.ass` directly), and the
  // attack surface (re-rooting output to `/etc/...` instead of `/`)
  // is rejected by `assertSafeOutputFilename` upstream because
  // `/etc/...` contains a separator.
  if (!normalizedOutput.startsWith(inputDir + "/")) {
    throw new Error(`Output path escapes input directory: ${normalizedOutput}`);
  }

  assertOutputPathLength(normalizedOutput);

  // Self-overwrite. Filesystem-aware: case-folding only on Windows /
  // macOS where the FS is case-insensitive. On Linux ext4 / btrfs / xfs,
  // `Episode.ass` and `episode.ass` are distinct files so unconditional
  // lowercase would false-reject a legitimate `episode.ass` output when
  // the input was `Episode.ass`.
  if (pathsEqualOnFs(normalizedOutput, normalizedInput)) {
    throw new Error("Output path is the same as input (would overwrite source file)");
  }
}

export function assertSafeOutputPathInDirectory(
  outputPath: string,
  outputDir: string,
  sourcePath?: string
): void {
  const normalizedOutput = normalizeForPathValidation(outputPath);
  const { dir: normalizedDir } = decomposeOutputDirectoryPath(outputDir);

  assertOutputPathShape(normalizedOutput);
  assertOutputPathInsideDirectory(normalizedOutput, normalizedDir);
  assertOutputPathLength(normalizedOutput);

  if (sourcePath && pathsEqualOnFs(normalizedOutput, sourcePath)) {
    throw new Error("Output path is the same as input (would overwrite source file)");
  }
}

/**
 * Extract the filename from a full file path and strip BiDi / zero-
 * width + ASCII control characters. Backslash is treated as a separator
 * only on Windows (POSIX-correctness gate, parity with
 * `decomposeInputPath`); on POSIX `\` is a valid filename character.
 *
 * Empty-result fallback: `String.prototype.split` always returns a
 * non-empty array and `pop()` returns `""` for trailing-separator
 * input (`C:/Users/`). Logical OR falls back to the original path so
 * consumers (addLog / status messages / dropError banners) always get
 * a meaningful display string.
 *
 * Sanitization: stripUnicodeControls covers BiDi / zero-width /
 * U+2028 / U+2029 / etc.; the trailing regex strips ASCII C0
 * (`\x00-\x1f`), DEL (`\x7f`), and C1 (`\x80-\x9f`). Both GUI display
 * (addLog, dropdown options, drop-error banners) and CLI stderr /
 * JSON output consume the result, so a crafted argv path or a hostile
 * filename containing `\r`, `\0`, or U+202E (Trojan-Source) cannot
 * break log row formatting or smuggle a visual-reversal into the
 * displayed name.
 *
 * Extracted from `tauri-api.ts::fileNameFromPath` +
 * `cli-engine-entry.ts::fileNameFromPath` (the two had drifted on the
 * empty-result fallback — the CLI copy lacked the `|| path` fix and
 * silently dropped trailing-separator paths from the rename plan).
 */
export function fileNameFromPath(path: string): string {
  const normalized = isWindowsRuntime ? path.replace(/\\/g, "/") : path;
  const raw = normalized.split("/").pop() || path;
  return stripUnicodeControls(raw).replace(new RegExp(`[${ASCII_CONTROL_CHARS}]`, "g"), "");
}
