use deno_core::{serde_v8, v8, JsRuntime, RuntimeOptions};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

const ENGINE_SOURCE: &str = include_str!(concat!(env!("OUT_DIR"), "/cli-engine.js"));

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HdrConversionRequest {
    pub input_path: String,
    pub content: String,
    pub eotf: String,
    pub brightness: u16,
    pub output_template: String,
}

/// Heavy-conversion result from `convertHdr`. The JS side also returns
/// `outputPath`, but the Rust shell now resolves that cheaply via
/// `resolve_hdr_output_path` before invoking `convert_hdr`, so the JS
/// `outputPath` is intentionally absent here — serde drops the unknown
/// field. If you re-add this field, also remove the cheap resolver
/// path in `process_hdr_file` so the two stay paired.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HdrConversionResult {
    pub content: String,
}

/// Cheap path-only resolution request. Lets the CLI shell dedup output
/// paths and skip-on-exists BEFORE invoking the heavy convert_hdr —
/// saves V8 work on batches with duplicate template-derived outputs.
/// The result must be byte-identical to convert_hdr's returned
/// `output_path`, which is guaranteed by both routing through the same
/// `resolveOutputPath` JS helper with the same defaults.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HdrPathRequest {
    pub input_path: String,
    pub eotf: String,
    pub output_template: String,
}

/// Cheap path-only resolution request for shift. Caller MUST pre-check
/// `output_template.contains("{format}")` — the JS resolver assumes
/// no `{format}` token (the value defaults to ""), and templates that
/// reference {format} need parsing the file to know the value, so the
/// Rust shell falls back to heavy-first ordering for those.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShiftPathRequest {
    pub input_path: String,
    pub output_template: String,
}

/// Cheap path-only resolution request for embed. No format dependency,
/// so it always works for any template.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbedPathRequest {
    pub input_path: String,
    pub output_template: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShiftConversionRequest {
    pub input_path: String,
    pub content: String,
    pub offset_ms: i64,
    pub threshold_ms: Option<i64>,
    pub output_template: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShiftConversionResult {
    pub output_path: String,
    pub content: String,
    pub format: String,
    pub caption_count: usize,
    pub shifted_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenamePlanRequest {
    pub paths: Vec<String>,
    pub mode: String,
    pub output_dir: Option<String>,
    pub langs: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenamePlanResult {
    pub video_count: usize,
    pub subtitle_count: usize,
    pub unknown_count: usize,
    pub ignored_count: usize,
    pub pairings: Vec<RenamePlanRow>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenamePlanRow {
    pub input_path: String,
    pub output_path: String,
    pub video_path: String,
    pub no_op: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FontEmbedPlanRequest {
    pub input_path: String,
    pub content: String,
    pub output_template: String,
}

/// Plan result from `planFontEmbed`. The JS side also returns
/// `outputPath`, but the Rust shell now resolves that cheaply via
/// `resolve_embed_output_path` before invoking `plan_font_embed`, so
/// the JS `outputPath` is intentionally absent here — serde drops the
/// unknown field. If you re-add this field, also remove the cheap
/// resolver path in `process_embed_file` so the two stay paired.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontEmbedPlanResult {
    pub fonts: Vec<FontEmbedUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontEmbedUsage {
    pub family: String,
    pub bold: bool,
    pub italic: bool,
    pub label: String,
    pub font_name: String,
    pub glyph_count: usize,
    pub codepoints: Vec<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FontEmbedApplyRequest {
    pub content: String,
    pub fonts: Vec<FontSubsetPayload>,
}

/// One font's subset bytes for the standalone `embed` flow's
/// `FontEmbedApplyRequest`. Serialized as `{ "fontName": ..., "data":
/// [byte, byte, ...] }` (JSON number-array form). Chain mode does NOT
/// use this struct on the wire — `process_one_chain_input` builds an
/// inline base64 payload (`{ "fontName": ..., "dataB64": "..." }`)
/// directly to dodge the ~4-5× expansion JSON-array form would impose
/// on the worst-case CUMULATIVE_FALLBACK_BYTES path. The two wire
/// formats are intentional, not a sign of unfinished migration.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FontSubsetPayload {
    pub font_name: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontEmbedApplyResult {
    pub content: String,
    pub embedded_count: usize,
}

/// Result returned from the TS-side `runChain`. Single source of
/// truth for the chain output: `content` is what gets written,
/// `output_path` is where, `notes` is the per-step diagnostic
/// summary surfaced in the Rust shell's per-file report.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainRunResult {
    pub content: String,
    pub output_path: String,
    pub notes: Vec<String>,
}

/// Wrapper struct exists only because the existing `call_engine`
/// helper takes a `Request: Serialize` typed value. The chain plan
/// is built in Rust as a `serde_json::Value` (via per-Args
/// `to_chain_step` methods), so we wrap the already-serialized
/// payload in this struct that just passes through. `flatten`
/// inlines the wrapped Value's keys into the JSON output, producing
/// the `{ plan, inputPath, content }` shape the TS runtime expects.
#[derive(Debug, Serialize)]
pub struct ChainRunRequest {
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

pub struct CliEngine {
    runtime: JsRuntime,
}

impl CliEngine {
    pub fn new() -> Result<Self, String> {
        // Empty extensions list — the CLI engine runs only pure text
        // transformations. No fs / net / timer ops are needed; an
        // explicit empty list documents that intent and hardens
        // against future deno_core default-set drift.
        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: vec![],
            ..Default::default()
        });

        // Inject platform globals BEFORE the engine bundle runs (Codex
        // 47c58c78): `src/lib/platform.ts` defaults to POSIX /
        // case-sensitive when neither Node's `process` nor a browser
        // `navigator` is available. The bare deno_core JsRuntime
        // provides neither, so without this injection the CLI would
        // run as if it were on Linux even on Windows / macOS —
        // breaking `C:\...` path normalization and collapsing case-
        // only-rename safety on case-insensitive volumes. Bundling
        // these into a single small bootstrap keeps the surface area
        // minimal; the bundled `platform.ts` IIFE reads the global at
        // module init time, before any consumer.
        //
        // Serialize via serde_json — matches the payload_setup pattern
        // below for `__ssahdrifyCliPayload`. Bool's `Display` produces
        // the same JS-valid `true`/`false`, but routing every Rust→JS
        // injection through one serializer keeps the encoding rules
        // consistent and avoids surprise if a future field is added
        // that isn't safe to format!-inject.
        let platform_json = serde_json::json!({
            "isWindows": cfg!(target_os = "windows"),
            "isCaseInsensitiveFs": cfg!(target_os = "windows") || cfg!(target_os = "macos"),
        });
        let platform_bootstrap = format!("globalThis.__ssahdrifyPlatform = {platform_json};");
        runtime
            .execute_script("ssahdrify-cli-platform-bootstrap.js", platform_bootstrap)
            .map_err(|err| format!("failed to bootstrap CLI engine platform globals: {err}"))?;

        runtime
            .execute_script("ssahdrify-cli-engine.js", ENGINE_SOURCE)
            .map_err(|err| format!("failed to initialize CLI engine: {err}"))?;

        // Probe that the bundle wired up the expected global. A
        // 0-byte or syntactically-valid-but-empty engine.js executes
        // without error but leaves the global undefined; per-method
        // calls then fail with the unhelpful "Cannot read properties
        // of undefined (reading 'convertHdr')". Surfacing a build-step
        // pointer here is more useful.
        let probe = runtime
            .execute_script(
                "ssahdrify-cli-engine-probe.js",
                "typeof globalThis.ssaHdrifyCliEngine === 'object' && globalThis.ssaHdrifyCliEngine !== null",
            )
            .map_err(|err| format!("failed to probe CLI engine global: {err}"))?;
        let probe_ok = {
            deno_core::scope!(scope, &mut runtime);
            let local = v8::Local::new(scope, probe);
            local.boolean_value(scope)
        };
        if !probe_ok {
            return Err(
                "CLI engine bundle did not define globalThis.ssaHdrifyCliEngine. \
                 Run `npm run build:engine` and rebuild ssahdrify-cli."
                    .to_string(),
            );
        }
        Ok(Self { runtime })
    }

    pub fn convert_hdr(
        &mut self,
        request: &HdrConversionRequest,
    ) -> Result<HdrConversionResult, String> {
        self.call_engine(
            "convertHdr",
            "ssahdrify-cli-convert-hdr.js",
            "HDR",
            "conversion",
            request,
        )
    }

    pub fn resolve_hdr_output_path(&mut self, request: &HdrPathRequest) -> Result<String, String> {
        self.call_engine(
            "resolveHdrOutputPath",
            "ssahdrify-cli-resolve-hdr-output-path.js",
            "HDR",
            "path resolution",
            request,
        )
    }

    pub fn resolve_shift_output_path(
        &mut self,
        request: &ShiftPathRequest,
    ) -> Result<String, String> {
        self.call_engine(
            "resolveShiftOutputPath",
            "ssahdrify-cli-resolve-shift-output-path.js",
            "Time Shift",
            "path resolution",
            request,
        )
    }

    pub fn resolve_embed_output_path(
        &mut self,
        request: &EmbedPathRequest,
    ) -> Result<String, String> {
        self.call_engine(
            "resolveEmbedOutputPath",
            "ssahdrify-cli-resolve-embed-output-path.js",
            "Font Embed",
            "path resolution",
            request,
        )
    }

    pub fn convert_shift(
        &mut self,
        request: &ShiftConversionRequest,
    ) -> Result<ShiftConversionResult, String> {
        self.call_engine(
            "convertShift",
            "ssahdrify-cli-convert-shift.js",
            "Time Shift",
            "conversion",
            request,
        )
    }

    pub fn plan_rename(&mut self, request: &RenamePlanRequest) -> Result<RenamePlanResult, String> {
        self.call_engine(
            "planRename",
            "ssahdrify-cli-plan-rename.js",
            "Batch Rename",
            "plan",
            request,
        )
    }

    pub fn plan_font_embed(
        &mut self,
        request: &FontEmbedPlanRequest,
    ) -> Result<FontEmbedPlanResult, String> {
        self.call_engine(
            "planFontEmbed",
            "ssahdrify-cli-plan-font-embed.js",
            "Font Embed",
            "plan",
            request,
        )
    }

    pub fn apply_font_embed(
        &mut self,
        request: &FontEmbedApplyRequest,
    ) -> Result<FontEmbedApplyResult, String> {
        self.call_engine(
            "applyFontEmbed",
            "ssahdrify-cli-apply-font-embed.js",
            "Font Embed",
            "embed",
            request,
        )
    }

    pub fn run_chain(&mut self, request: &ChainRunRequest) -> Result<ChainRunResult, String> {
        self.call_engine(
            "runChain",
            "ssahdrify-cli-run-chain.js",
            "Chain",
            "execution",
            request,
        )
    }

    fn call_engine<Request, Response>(
        &mut self,
        function_name: &'static str,
        script_name: &'static str,
        label: &str,
        step: &str,
        request: &Request,
    ) -> Result<Response, String>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        let request_json = serde_json::to_string(request)
            .map_err(|err| format!("failed to encode {label} request: {err}"))?;
        // Stash payload on globalThis instead of inlining it into the
        // call script. V8's stack-trace formatter echoes the source
        // line verbatim in JsError display — inlining a 10 MB ASS body
        // into the call script would flood stderr (potentially MB) on
        // any JS-side exception. A short call referencing
        // `globalThis.__ssahdrifyCliPayload` keeps stack traces readable.
        //
        // Three load-bearing invariants for the JSON-into-source
        // construction below:
        //   1. serde_json::to_string emits RFC 8259 JSON, a strict
        //      subset of valid JS expression syntax (escapes ", \, and
        //      control chars; emits U+2028/U+2029 as literal bytes,
        //      legal inside JS string literals since ES2019).
        //   2. V8 14.7 (deno_core 0.400) is well past ES2019.
        //   3. function_name comes from a hardcoded &'static str at
        //      every call site — never user-controlled.
        // If any invariant shifts, move to a v8 function-call path
        // that takes the argument as a v8::Value instead.
        //
        // Future-field hazard: any new request struct field with f64
        // (NaN/∞ surface as serialization errors — fail-safe), i128 /
        // u128 (silent truncate to JS Number), or i64 / u64 outside
        // ±2^53 (silent precision loss to JS Number) requires
        // re-evaluating this path. Today's fields (i64 offsets bounded
        // by MAX_SHIFT_OFFSET_MS, u16 brightness, bool, String) are
        // all within JS Number range. Audit before adding new fields.
        let payload_setup = format!("globalThis.__ssahdrifyCliPayload = {request_json};");
        self.runtime
            .execute_script("ssahdrify-cli-payload.js", payload_setup)
            .map_err(|err| format!("{label} {step} failed: {err}"))?;

        let script = format!(
            "globalThis.ssaHdrifyCliEngine.{function_name}(globalThis.__ssahdrifyCliPayload)"
        );
        let call_result = self.runtime.execute_script(script_name, script);

        // Clear the global UNCONDITIONALLY (whether the call succeeded
        // or threw). Running cleanup only on the success path leaves
        // the previous payload accessible in V8's heap if the engine
        // bundle ever holds a reference to globalThis.__ssahdrifyCliPayload.
        // Best-effort: cleanup failures aren't surfaced to the user.
        let _ = self.runtime.execute_script(
            "ssahdrify-cli-payload-cleanup.js",
            "globalThis.__ssahdrifyCliPayload = undefined;",
        );

        let result = call_result.map_err(|err| format!("{label} {step} failed: {err}"))?;

        deno_core::scope!(scope, &mut self.runtime);
        let local = v8::Local::new(scope, result);
        serde_v8::from_v8(scope, local)
            .map_err(|err| format!("failed to decode {label} {step} result: {err}"))
    }
}
