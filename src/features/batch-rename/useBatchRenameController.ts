import { useState, useCallback, useEffect, useRef, useMemo } from "react";
import {
  pickRenameInputs,
  pickOutputDirectory,
  renamePath,
  copyPath,
  fileNameFromPath,
} from "../../lib/tauri-api";
import { ask } from "@tauri-apps/plugin-dialog";
import { useI18n } from "../../i18n/useI18n";
import { useFileContext } from "../../lib/FileContext";
import type { Status } from "../../lib/StatusContext";
import { useTabStatus } from "../../lib/useTabStatus";
import { useFolderDrop } from "../../lib/useFolderDrop";
import { findExistingOutputKeys } from "../../lib/output-collisions";
import { useLogPanel } from "../../lib/useLogPanel";
import { subtitleLanguageFromName } from "../../lib/lang-detection";
import {
  buildConflictMessage,
  normalizeOutputKey,
  sanitizeError,
  sanitizeForDialog,
} from "../../lib/dedup-helpers";
import {
  buildPairings,
  buildMultiSubtitlePairings,
  parseFilename,
  deriveRenameOutputPath,
  findDuplicateRenameOutputKeys,
  findRenameInputConflictIndexes,
  findRenameOutputOwnershipConflictIndexes,
  countUnpairedSubtitlePaths,
  refreshRenameFilesAfterSuccessfulInPlaceRenames,
  refreshPairingRowsAfterSuccessfulInPlaceRenames,
  summarizeRenameRun,
  isNoOpRename,
  assignSubtitleToRow,
  type PairingRow,
  type OutputMode,
} from "./pairing-engine";
import { categorizeForRename } from "../../lib/rename-extensions";

// VIDEO_EXTS / RENAME_SUBTITLE_EXTS / IGNORED_EXTS / Category /
// `categorizeForRename` now live in `src/lib/rename-extensions.ts` —
// shared with the CLI engine and the tauri-api picker filter. Add new
// rename-only sidecars there without widening parser-aligned text
// subtitle workflows.

interface Categorized {
  videos: string[];
  subtitles: string[];
  unknown: string[];
}

function categorizePaths(paths: string[]): Categorized {
  const videos: string[] = [];
  const subtitles: string[] = [];
  const unknown: string[] = [];
  for (const p of paths) {
    const cat = categorizeForRename(fileNameFromPath(p));
    if (cat === "video") videos.push(p);
    else if (cat === "subtitle") subtitles.push(p);
    else if (cat === "ignored") continue;
    else unknown.push(p);
  }
  return { videos, subtitles, unknown };
}

export function useBatchRenameController() {
  const { t } = useI18n();
  const { renameFiles, setRenameFiles, clearFile, isFileInUse, findLoadedInputConflictKeys } =
    useFileContext();

  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<{ processed: number; total: number } | null>(null);
  const [lastActionResult, setLastActionResult] = useState<
    "success" | "partial" | "error" | "cancelled" | "noop" | null
  >(null);
  const [dropActive, setDropActive] = useState(false);
  const [dropError, setDropError] = useState<string | null>(null);
  // Unknown-extension counter — surfaced in the chips but not in
  // renameFiles state, since the unknown bucket isn't pairing material.
  // Reset whenever renameFiles changes.
  const [unknownCount, setUnknownCount] = useState(0);
  const { logs, addLog, clearLogs, logScrollRef } = useLogPanel();
  // Output strategy: three modes.
  // Default `copy_to_video` matches the most common fan-sub workflow
  // (subs end up in the same folder as the videos, originals untouched).
  const [outputMode, setOutputMode] = useState<OutputMode>("copy_to_video");
  // Picked target directory for the `copy_to_chosen` mode. Required
  // before Run when that mode is active.
  const [chosenDir, setChosenDir] = useState<string | null>(null);
  const [multiSubtitleMode, setMultiSubtitleMode] = useState(false);
  // Pairing rows are direct state, not a derived view over an
  // overrides Map. The engine seeds them from the input file lists;
  // user actions (toggle / dropdown pick) mutate rows in place,
  // marking edited rows as `source: 'manual'`. ↺ Reset restores the
  // engine's seed.
  const [editedRows, setEditedRows] = useState<PairingRow[]>([]);
  const [activePickerRow, setActivePickerRow] = useState<string | null>(null);

  const pickGenRef = useRef(0);
  // Output-directory picks have their own generation because the chosen
  // directory intentionally survives ordinary input re-picks, but Clear and
  // a newer directory dialog must invalidate an older unresolved dialog.
  const chosenDirPickGenRef = useRef(0);
  const abortRef = useRef<AbortController | null>(null);
  // A successful in-place run rewrites shared file identities. Skip exactly
  // that resulting baseRows reset because the live row state is reconciled
  // directly, preserving manual assignments and unchecked/failed-row intent.
  const skipNextInternalBaseRowsResetRef = useRef(false);
  // Keep the exact internally-skipped seed too. Development StrictMode can
  // replay an effect; the replay must keep skipping that same seed without
  // suppressing a later mode toggle or external picker selection.
  const internallySkippedBaseRowsRef = useRef<PairingRow[] | null>(null);
  // Synchronous double-click guard — `busy` state lags setBusy(true) by
  // one render. busyRef is written synchronously at handler entry and
  // released in the outer finally so every exit path clears it.
  const busyRef = useRef(false);
  const dropZoneRef = useRef<HTMLDivElement>(null);

  const videoPaths = useMemo(() => renameFiles?.videoPaths ?? [], [renameFiles]);
  const videoNames = useMemo(() => renameFiles?.videoNames ?? [], [renameFiles]);
  const subtitlePaths = useMemo(() => renameFiles?.subtitlePaths ?? [], [renameFiles]);
  const subtitleNames = useMemo(() => renameFiles?.subtitleNames ?? [], [renameFiles]);
  const videoCount = videoPaths.length;
  const subtitleCount = subtitlePaths.length;
  const totalCount = videoCount + subtitleCount;

  // Pairing seed — recomputed whenever the file lists change. Row IDs
  // are anchored on the video path (or subtitle path for unmatched
  // rows) so adding one video to the picker doesn't shift every
  // existing row's ID . The old form used per-iteration
  // counters (`b1`, `b2`, ...): adding one video at the top renumbered
  // every row, React remounted every <select>, and any in-progress
  // dropdown interaction was lost. Path-anchored IDs survive list
  // mutations as long as the path itself is stable.
  //
  // Unmatched rows use `s|<sub>` because there's no video to anchor on.
  // A `v|` / `s|` prefix prevents accidental collision between a video
  // path that happens to equal a subtitle path (vanishingly unlikely
  // with disjoint extension sets, but cheap insurance).
  const baseRows = useMemo<PairingRow[]>(() => {
    const parsedVideos = videoPaths.map((p, i) => parseFilename(p, videoNames[i] ?? ""));
    const parsedSubs = subtitlePaths.map((p, i) => parseFilename(p, subtitleNames[i] ?? ""));
    // buildPairings now owns row id derivation
    // (v|<path> for every row — engine only emits video-bearing rows).
    // An earlier form of this site re-keyed every row with `s|...` /
    // `b|...` branches that were unreachable in practice (subtitle-
    // only or both-null rows are never produced by buildPairings), and
    // the engine's `makeRowId` output was discarded one line later.
    return multiSubtitleMode
      ? buildMultiSubtitlePairings(parsedVideos, parsedSubs)
      : buildPairings(parsedVideos, parsedSubs);
  }, [videoPaths, videoNames, subtitlePaths, subtitleNames, multiSubtitleMode]);

  // Reset edits when an external selection re-picks / clears the file lists.
  // A successful in-place operation also changes those lists, but reconciles
  // live rows directly and arms the one-shot skip above so user intent stays.
  useEffect(() => {
    if (skipNextInternalBaseRowsResetRef.current) {
      skipNextInternalBaseRowsResetRef.current = false;
      internallySkippedBaseRowsRef.current = baseRows;
      return;
    }
    if (internallySkippedBaseRowsRef.current === baseRows) return;
    internallySkippedBaseRowsRef.current = null;
    setEditedRows(baseRows);
  }, [baseRows]);

  const pairingRows = editedRows;

  const toggleRow = useCallback((rowId: string) => {
    setEditedRows((rows) =>
      rows.map((r) => (r.id === rowId ? { ...r, selected: !r.selected } : r))
    );
  }, []);

  // Subtitle pool for every row's dropdown. Built from the original
  // subtitle inputs (not from row state) so subs that aren't paired
  // with any row are still selectable — the user-supplied sub list
  // is the source of truth for what's available to pair.
  const availableSubtitles = useMemo(
    () => subtitlePaths.map((p, i) => ({ path: p, name: subtitleNames[i] ?? p })),
    [subtitlePaths, subtitleNames]
  );

  const assignSubtitleLocal = useCallback(
    (rowId: string, subPath: string | null) => {
      const sub = subPath ? availableSubtitles.find((s) => s.path === subPath) : null;
      // Defensive: caller should never pass an unknown path, but
      // guard against it rather than producing a broken row.
      if (subPath && !sub) return;
      setEditedRows((rows) => assignSubtitleToRow(rows, rowId, sub ?? null));
    },
    [availableSubtitles]
  );

  const resetPairings = useCallback(() => {
    setEditedRows(baseRows);
  }, [baseRows]);

  const hasManualEdits = useMemo(() => editedRows.some((r) => r.source === "manual"), [editedRows]);

  const warningCount = useMemo(
    () => pairingRows.filter((r) => r.source === "warning").length,
    [pairingRows]
  );
  const unpairedSubtitleCount = useMemo(
    () => countUnpairedSubtitlePaths(subtitlePaths, pairingRows),
    [subtitlePaths, pairingRows]
  );

  const actionableRows = useMemo(
    () => pairingRows.filter((r) => r.selected && r.video !== null && r.subtitle !== null),
    [pairingRows]
  );
  const actionableCount = actionableRows.length;

  const ingestPaths = useCallback(
    (paths: string[]) => {
      // gen-guard removed. The previous form took a
      // `gen` parameter and short-circuited via `gen !== pickGenRef.current`
      // for shape consistency with HDR / Timing / FontEmbed's async
      // ingest. Here the handler is synchronous — no `await` suspension
      // exists where `pickGenRef.current` could advance — so the guard
      // was structurally dead. Callers still bump `pickGenRef.current`
      // before calling so any concurrent async work (from a future
      // refactor that adds an await inside this handler) gets its
      // invalidation signal at the bump site.
      // Clear the previous run's action result on EVERY new ingest,
      // not just the happy path. An earlier form's conflict / empty-
      // input early returns left `lastActionResult` set to whatever
      // the last run produced ("success" / "error" / "cancelled" /
      // "noop"), so the footer rendered the stale "Rename complete" or
      // "Rename failed" message alongside the fresh red drop-error
      // banner — contradictory user signals. Resetting at the top of
      // ingest matches the natural mental model: a new selection means
      // "start fresh", regardless of whether the ingest itself
      // succeeds.
      setLastActionResult(null);

      const { videos, subtitles, unknown } = categorizePaths(paths);

      // Conflict check applies only to subtitles — videos can't collide
      // with another tab. Strict cross-tab dedup contract: any conflict
      // rejects the WHOLE selection (see buildConflictMessage).
      const conflictMsg = buildConflictMessage(subtitles, "rename", isFileInUse, t);
      if (conflictMsg) {
        setDropError(conflictMsg);
        return;
      }
      setDropError(null);

      if (videos.length === 0 && subtitles.length === 0) {
        // reset the unknown-count chip on this early
        // return too. Without it, a drop of only unknown / ignored files
        // after a prior drop with unknowns leaves the stale chip count
        // visible even though the current state shows zero videos +
        // zero subtitles. Mirrors the explicit `setUnknownCount(unknown.length)`
        // on the happy path below.
        setUnknownCount(unknown.length);
        addLog(t("msg_no_rename_inputs_in_drop"), "error");
        return;
      }

      setRenameFiles({
        videoPaths: videos,
        videoNames: videos.map(fileNameFromPath),
        subtitlePaths: subtitles,
        subtitleNames: subtitles.map(fileNameFromPath),
      });
      setUnknownCount(unknown.length);
      if (unknown.length > 0) {
        addLog(t("msg_rename_unknown_skipped", unknown.length), "info");
      }
    },
    [isFileInUse, setRenameFiles, addLog, t]
  );

  const handlePickFiles = useCallback(async () => {
    const gen = (pickGenRef.current = pickGenRef.current + 1);
    // catch dialog IPC failures. See
    // TimingShift.handlePickFiles for the project-wide rationale.
    let paths: string[] | null;
    try {
      paths = await pickRenameInputs(t);
    } catch (e) {
      if (gen !== pickGenRef.current) return;
      addLog(t("error_prefix", sanitizeError(e)), "error");
      return;
    }
    if (gen !== pickGenRef.current) return;
    if (!paths || paths.length === 0) return;
    ingestPaths(paths);
  }, [ingestPaths, addLog, t]);

  const handleDroppedPaths = useCallback(
    (paths: string[]) => {
      pickGenRef.current = pickGenRef.current + 1;
      ingestPaths(paths);
    },
    [ingestPaths]
  );

  useFolderDrop({
    ref: dropZoneRef,
    onPaths: handleDroppedPaths,
    onActiveChange: setDropActive,
    onError: (e) => setDropError(sanitizeError(e)),
    disabled: busy,
    t,
  });

  const handleClearFiles = useCallback(() => {
    pickGenRef.current = pickGenRef.current + 1;
    chosenDirPickGenRef.current = chosenDirPickGenRef.current + 1;
    clearFile("rename");
    setLastActionResult(null);
    setChosenDir(null);
    setUnknownCount(0);
    setDropError(null);
  }, [clearFile]);

  const handlePickChosenDir = useCallback(async () => {
    const gen = (chosenDirPickGenRef.current = chosenDirPickGenRef.current + 1);
    let dir: string | null;
    try {
      dir = await pickOutputDirectory(t);
    } catch (error) {
      // A stale dialog belongs to an abandoned state and must not emit a
      // confusing late error after Clear or a newer picker launch.
      if (gen !== chosenDirPickGenRef.current) return;
      addLog(t("error_prefix", sanitizeError(error)), "error");
      return;
    }
    if (gen !== chosenDirPickGenRef.current) return;
    if (dir) setChosenDir(dir);
  }, [addLog, t]);

  const handleRunRename = useCallback(async () => {
    if (busy || actionableCount === 0) return;
    // Synchronous double-click gate — see HdrConvert::handleConvert.
    if (busyRef.current) return;
    busyRef.current = true;
    pickGenRef.current++;
    chosenDirPickGenRef.current++;
    abortRef.current = new AbortController();
    setBusy(true);
    try {
      if (outputMode === "copy_to_chosen" && !chosenDir) {
        addLog(t("msg_rename_no_chosen_dir"), "error");
        return;
      }

      // Derive output paths up front so all confirmation dialogs see
      // the final names. Catch derive errors per-row — a bad row gets
      // logged + skipped, the rest of the batch proceeds. Track the
      // skipped count so subsequent confirm dialogs can surface it
      // (otherwise the dialog says "Rename N files" using the post-skip
      // count, hiding the skips during the moment the user can't see
      // the log).
      const derivedTargets: { row: PairingRow; outputPath: string }[] = [];
      let preflightFailedCount = 0;
      for (const row of actionableRows) {
        try {
          const outputPath = deriveRenameOutputPath(
            row.video!.path,
            row.subtitle!.path,
            outputMode,
            chosenDir,
            {
              languageSuffix: multiSubtitleMode
                ? subtitleLanguageFromName(row.subtitle!.name) || undefined
                : undefined,
            }
          );
          derivedTargets.push({ row, outputPath });
        } catch (e) {
          const reason = sanitizeError(e);
          // Sanitize the subtitle name before it flows into the log
          // line. Fan-sub subtitle filenames are attacker-influenced;
          // HDR / Timing / Embed already sanitize at every
          // addLog site, BatchRename was the outlier.
          addLog(t("msg_rename_skipped", sanitizeForDialog(row.subtitle!.name), reason), "error");
          preflightFailedCount += 1;
        }
      }
      if (derivedTargets.length === 0) {
        addLog(t("msg_rename_nothing_to_do"), "error");
        return;
      }

      // Input-conflict pre-flight. Detect executable edges against every
      // loaded subtitle input, including no-op, unchecked, and unpaired rows:
      // a planned output can otherwise overwrite user data or bytes a later
      // row still needs. Mark both executable sides of every edge so swaps and
      // longer chains fail as a unit before confirmation or filesystem access.
      const inputConflictIndexes = findRenameInputConflictIndexes(
        derivedTargets.map((target) => ({
          inputPath: target.row.subtitle!.path,
          outputPath: target.outputPath,
        })),
        subtitlePaths
      );
      const inputConflictTargets = derivedTargets.filter((_, index) =>
        inputConflictIndexes.has(index)
      );
      for (const target of inputConflictTargets) {
        addLog(
          t("msg_rename_input_conflict", sanitizeForDialog(target.row.subtitle!.name)),
          "error"
        );
      }
      preflightFailedCount += inputConflictTargets.length;
      const conflictFreeTargets = derivedTargets.filter(
        (_, index) => !inputConflictIndexes.has(index)
      );

      // No-op pre-flight. When a sub is already correctly named for its
      // paired video (e.g. raw-pack external-sub releases that ship subs
      // matching `<videoBase>.<lang>.ass`), the derived output equals the
      // source path. Filtering these out BEFORE the overwrite dialog
      // avoids a spurious "N files already exist, overwrite?" prompt
      // followed by copyFile(src, src) failures. The remaining work goes
      // through the regular flow.
      const noopTargets: { row: PairingRow; outputPath: string }[] = [];
      let targets: { row: PairingRow; outputPath: string }[] = [];
      for (const tgt of conflictFreeTargets) {
        if (isNoOpRename(tgt.row.subtitle!.path, tgt.outputPath)) {
          noopTargets.push(tgt);
        } else {
          targets.push(tgt);
        }
      }
      for (const tgt of noopTargets) {
        // Sanitize attacker-influenced filename.
        addLog(t("msg_rename_already_named", sanitizeForDialog(tgt.row.subtitle!.name)), "info");
      }

      // A derived output can be a file currently loaded in HDR / Timing /
      // Fonts / Style even though none of the Rename inputs use that path.
      // Generic overwrite confirmation is not sufficient: replacing it would
      // leave the owning tab with stale cached content. Reject those rows
      // before any confirmation or filesystem operation.
      const crossTabConflictIndexes = findRenameOutputOwnershipConflictIndexes(
        targets,
        (outputPath) => isFileInUse(outputPath, "rename") !== null
      );
      const crossTabConflictTargets = targets.filter((_, index) =>
        crossTabConflictIndexes.has(index)
      );
      for (const target of crossTabConflictTargets) {
        const reason = buildConflictMessage([target.outputPath], "rename", isFileInUse, t);
        if (reason) {
          addLog(
            t("msg_rename_skipped", sanitizeForDialog(target.row.subtitle!.name), reason),
            "error"
          );
        }
      }
      if (crossTabConflictTargets.length > 0) {
        targets = targets.filter((_, index) => !crossTabConflictIndexes.has(index));
        preflightFailedCount += crossTabConflictTargets.length;
      }

      const duplicateOutputKeys = findDuplicateRenameOutputKeys(
        conflictFreeTargets.map((tgt) => tgt.outputPath)
      );
      const duplicateTargets = targets.filter((tgt) =>
        duplicateOutputKeys.has(normalizeOutputKey(tgt.outputPath))
      );
      for (const tgt of duplicateTargets) {
        addLog(t("msg_skipped_duplicate", sanitizeForDialog(tgt.row.subtitle!.name)), "error");
      }
      if (duplicateTargets.length > 0) {
        targets = targets.filter(
          (tgt) => !duplicateOutputKeys.has(normalizeOutputKey(tgt.outputPath))
        );
      }
      let canonicalInputConflicts: ReadonlySet<string> = new Set();
      if (targets.length > 0) {
        try {
          canonicalInputConflicts = await findLoadedInputConflictKeys(
            targets.map((target) => target.outputPath),
            abortRef.current?.signal
          );
          if (abortRef.current?.signal.aborted) {
            setLastActionResult("cancelled");
            return;
          }
          targets = targets.filter((target) => {
            if (!canonicalInputConflicts.has(normalizeOutputKey(target.outputPath))) return true;
            addLog(
              t("msg_rename_input_conflict", sanitizeForDialog(target.row.subtitle!.name)),
              "error"
            );
            preflightFailedCount++;
            return false;
          });
        } catch (error) {
          addLog(t("error_prefix", sanitizeError(error)), "error");
          setLastActionResult("error");
          return;
        }
      }
      if (targets.length === 0) {
        if (preflightFailedCount > 0 || duplicateTargets.length > 0) {
          addLog(
            t("msg_rename_all_failed", preflightFailedCount + duplicateTargets.length),
            "error"
          );
          setLastActionResult("error");
          return;
        }
        // Nothing was actually written. Logging this as success + green
        // footer would suggest the rename worked; route through "noop"
        // (amber pending) so the user sees that the run made no changes.
        addLog(t("msg_rename_all_already_named", noopTargets.length), "info");
        setLastActionResult("noop");
        return;
      }

      // In-place rename is destructive (source disappears). Show a
      // confirmation dialog with the first 3 sample names so the user
      // sees exactly what will happen before committing.
      //
      // Filename injection isn't a concern here: ask() from
      // @tauri-apps/plugin-dialog renders the body as plain text via
      // Windows TaskDialog, not HTML.
      // Names with embedded HTML / markup render as literal characters.
      // Suffix for the skipped-derive count. The in-place rename confirm
      // + the overwrite-existing confirm can both fire in the same run
      // (rename mode + outputs already exist), so the overwrite branch
      // suppresses the suffix only when the rename branch above already
      // showed it (previously a mutable `skippedSuffixShown` flag
      // tracked this — collapsed to a derived const at the
      // overwrite-branch callsite since the only consumer computes the
      // answer locally).
      const skippedSuffix =
        preflightFailedCount > 0
          ? "\n\n" + t("msg_rename_skipped_count", preflightFailedCount)
          : "";

      if (outputMode === "rename") {
        // Strip BiDi overrides from filenames before rendering them in the
        // OS-native dialog body. A subtitle file named with U+202E mid-
        // string can visually reverse the arrow + target name in the
        // confirmation, tricking the user into approving an unintended
        // rename (CVE-2021-42574 class). Counts and other non-filename
        // strings are unaffected.
        const samples = targets
          .slice(0, 3)
          .map(
            (t2) =>
              `${sanitizeForDialog(t2.row.subtitle!.name)} → ${sanitizeForDialog(fileNameFromPath(t2.outputPath))}`
          )
          .join("\n");
        const moreCount = targets.length - 3;
        const moreSuffix = moreCount > 0 ? "\n" + t("msg_rename_inplace_more", moreCount) : "";
        const confirmed = await ask(
          t("msg_rename_inplace_confirm", targets.length) +
            "\n\n" +
            samples +
            moreSuffix +
            skippedSuffix,
          { title: t("dialog_rename_inplace_title"), kind: "warning" }
        );
        if (!confirmed || abortRef.current?.signal.aborted) {
          addLog(t("msg_rename_cancelled"), "info");
          setLastActionResult("cancelled");
          return;
        }
      }

      // Pre-flight overwrite check — same project-wide pattern as the
      // other batch tabs. ANY existing target → single ask() with the
      // count; cancel preserves prior state, confirm proceeds. No-op
      // targets were filtered above so they don't inflate the count.
      //
      // Surface the derive-skip count here too (not just in the in-place
      // confirm above): copy modes go straight to the overwrite dialog,
      // and the user has no other way to learn that some pairings failed
      // before the dialog moment.
      // Dedup by the same output key the rename loop uses for its
      // within-batch skip — two rows resolving to the same output are
      // written once, so the overwrite-confirm count must not count both
      // (it would overstate the files actually overwritten).
      const seenProjected = new Set<string>();
      const projectedOutputs = targets
        .map((t2) => t2.outputPath)
        .filter((p) => {
          const key = normalizeOutputKey(p);
          if (seenProjected.has(key)) return false;
          seenProjected.add(key);
          return true;
        });
      let existingOutputs: ReadonlySet<string> = new Set();
      try {
        existingOutputs = await findExistingOutputKeys(projectedOutputs);
        if (abortRef.current?.signal.aborted) {
          setLastActionResult("cancelled");
          return;
        }
        const existingCount = existingOutputs.size;
        if (existingCount > 0) {
          // Show the skipped-count once per run: the rename-mode confirm
          // above already includes `skippedSuffix`, so suppress it here
          // in rename mode. Copy modes skip the rename confirm entirely
          // and need this dialog to surface the count.
          const overwriteSuffix = outputMode === "rename" ? "" : skippedSuffix;
          const confirmed = await ask(
            t("msg_overwrite_confirm", existingCount, targets.length) + overwriteSuffix,
            {
              title: t("dialog_overwrite_title"),
              kind: "warning",
            }
          );
          if (!confirmed || abortRef.current?.signal.aborted) {
            addLog(t("msg_rename_cancelled"), "info");
            setLastActionResult("cancelled");
            return;
          }
        }
      } catch (e) {
        addLog(t("error_prefix", sanitizeError(e)), "error");
        setLastActionResult("error");
        return;
      }

      setProgress({ processed: 0, total: targets.length });

      try {
        // Switch on the literal OutputMode union so adding a new value forces
        // a TypeScript exhaustiveness error here, instead of silently emitting
        // an i18n key that doesn't exist (the previous template-literal form
        // would have rendered the bare key string as fallback). Every other
        // call site in the codebase passes literal i18n keys; keep this one
        // consistent.
        let modeLabel: string;
        switch (outputMode) {
          case "rename":
            modeLabel = t("rename_mode_rename_short");
            break;
          case "copy_to_video":
            modeLabel = t("rename_mode_copy_to_video_short");
            break;
          case "copy_to_chosen":
            modeLabel = t("rename_mode_copy_to_chosen_short");
            break;
          default: {
            // Compile-time exhaustive check; the throw is runtime
            // defense-in-depth in case a type-cast bypass slips an
            // unknown value through (would otherwise be passed to t()
            // verbatim and surface as the missing-key fallback).
            const _exhaustive: never = outputMode;
            throw new Error(`unhandled output mode: ${String(_exhaustive)}`);
          }
        }
        addLog(t("msg_rename_start", targets.length, modeLabel));

        let successCount = 0;
        let processedCount = 0;
        const seenOutputs = new Set<string>();
        const successfulInPlaceTargets: { inputPath: string; outputPath: string }[] = [];

        for (const { row, outputPath } of targets) {
          if (abortRef.current?.signal.aborted) {
            addLog(t("msg_rename_cancelled"), "info");
            break;
          }

          // Hoist sanitized display names at top of per-row block.
          // Same pattern HDR / Timing / FontEmbed adopted — every
          // addLog site below reuses these. row.subtitle!.path (used
          // for the actual renamePath / copyPath calls) keeps the
          // literal bytes; only the display path is sanitized.
          const subName = sanitizeForDialog(row.subtitle!.name);
          const outName = sanitizeForDialog(fileNameFromPath(outputPath));
          addLog(t("msg_processing", subName));

          try {
            // Defense-in-depth duplicate guard. The pre-flight above
            // blocks every non-no-op duplicate participant before any
            // write; keep this loop check so future refactors cannot
            // accidentally reintroduce first-wins overwrite behavior.
            const normalizedOut = normalizeOutputKey(outputPath);
            if (canonicalInputConflicts.has(normalizedOut)) {
              throw new Error(t("msg_rename_input_conflict", subName));
            }
            if (seenOutputs.has(normalizedOut)) {
              addLog(t("msg_skipped_duplicate", subName), "error");
              continue;
            }
            seenOutputs.add(normalizedOut);

            if (abortRef.current?.signal.aborted) break;

            if (outputMode === "rename") {
              await renamePath(
                row.subtitle!.path,
                outputPath,
                existingOutputs.has(normalizeOutputKey(outputPath))
              );
            } else {
              await copyPath(
                row.subtitle!.path,
                outputPath,
                existingOutputs.has(normalizeOutputKey(outputPath))
              );
            }
            addLog(t("msg_rename_done", subName, outName), "success");
            successCount++;
            if (outputMode === "rename") {
              successfulInPlaceTargets.push({
                inputPath: row.subtitle!.path,
                outputPath,
              });
            }
          } catch (e) {
            const reason = sanitizeError(e);
            addLog(t("msg_rename_error", subName, reason), "error");
          } finally {
            processedCount++;
            setProgress({ processed: processedCount, total: targets.length });
          }
        }

        // Rename mode consumes each successful source path. Refresh the
        // shared FileContext only after the loop so partial failure/cancel
        // keeps every untouched input stable. Cross-tab-owned outputs were
        // rejected during preflight, so every produced path is safe to track.
        if (successfulInPlaceTargets.length > 0) {
          const refreshedRenameFiles = refreshRenameFilesAfterSuccessfulInPlaceRenames(
            renameFiles,
            successfulInPlaceTargets
          );
          if (refreshedRenameFiles !== renameFiles) {
            skipNextInternalBaseRowsResetRef.current = true;
            setEditedRows((rows) =>
              refreshPairingRowsAfterSuccessfulInPlaceRenames(rows, successfulInPlaceTargets)
            );
            setRenameFiles(refreshedRenameFiles);
          }
        }

        // Classify against executable + preflight-rejected rows. A batch with
        // one write and one rejected/error row is partial, not a green 1/1
        // success; a zero-write batch gets only the failure signal.
        const aborted = !!abortRef.current?.signal.aborted;
        if (aborted) {
          setLastActionResult("cancelled");
        } else {
          const summary = summarizeRenameRun(
            successCount,
            targets.length,
            preflightFailedCount + duplicateTargets.length
          );
          if (summary.kind === "success") {
            addLog(t("msg_rename_complete", successCount, summary.totalPlanned), "success");
            setLastActionResult("success");
          } else if (summary.kind === "partial") {
            addLog(t("msg_rename_complete", successCount, summary.totalPlanned), "warn");
            setLastActionResult("partial");
          } else {
            addLog(t("msg_rename_all_failed", summary.totalPlanned), "error");
            setLastActionResult("error");
          }
        }
      } catch (e) {
        // The default-branch throw inside the switch above (and any other
        // unexpected error from the rename loop) lands here. Without this
        // catch the throw bubbles past the finally to React's boundary as
        // an unhandled rejection — the Run button's spinner ends with no
        // log line, no toast, no banner. Surface it through the same log
        // path the per-row errors use.
        const reason = sanitizeError(e);
        addLog(t("error_prefix", reason), "error");
        setLastActionResult("error");
      } finally {
        setBusy(false);
        setProgress(null);
      }
    } finally {
      busyRef.current = false;
      setBusy(false);
      setProgress(null);
      abortRef.current = null;
    }
  }, [
    busy,
    actionableCount,
    actionableRows,
    outputMode,
    chosenDir,
    multiSubtitleMode,
    subtitlePaths,
    renameFiles,
    setRenameFiles,
    isFileInUse,
    findLoadedInputConflictKeys,
    addLog,
    t,
  ]);

  const tabStatus = useMemo<Status>(() => {
    if (totalCount === 0) return { kind: "idle", message: t("status_rename_idle") };
    if (busy) {
      return {
        kind: "busy",
        message: t("status_rename_busy"),
        progress: progress ?? undefined,
      };
    }
    if (lastActionResult === "success") return { kind: "done", message: t("status_rename_done") };
    if (lastActionResult === "partial") {
      return { kind: "pending", message: t("status_rename_partial") };
    }
    if (lastActionResult === "error") return { kind: "error", message: t("status_rename_error") };
    if (lastActionResult === "cancelled") {
      return { kind: "pending", message: t("status_rename_cancelled") };
    }
    if (lastActionResult === "noop") {
      return { kind: "pending", message: t("status_rename_noop") };
    }
    return {
      kind: "pending",
      message: t("status_rename_pending", videoCount, subtitleCount),
    };
  }, [totalCount, busy, progress, lastActionResult, videoCount, subtitleCount, t]);
  useTabStatus("rename", tabStatus);

  const requestRenameCancel = useCallback(() => {
    abortRef.current?.abort();
  }, []);

  useEffect(() => {
    return () => {
      abortRef.current?.abort();
    };
  }, []);

  return {
    t,
    busy,
    dropZoneRef,
    dropActive,
    dropError,
    setDropError,
    videoCount,
    subtitleCount,
    totalCount,
    unknownCount,
    outputMode,
    setOutputMode,
    chosenDir,
    multiSubtitleMode,
    setMultiSubtitleMode,
    pairingRows,
    availableSubtitles,
    activePickerRow,
    setActivePickerRow,
    toggleRow,
    assignSubtitleLocal,
    resetPairings,
    hasManualEdits,
    warningCount,
    unpairedSubtitleCount,
    actionableCount,
    handlePickFiles,
    handleClearFiles,
    handlePickChosenDir,
    handleRunRename,
    requestRenameCancel,
    logs,
    clearLogs,
    logScrollRef,
  };
}
