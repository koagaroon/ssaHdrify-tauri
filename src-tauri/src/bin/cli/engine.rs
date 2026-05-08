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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontEmbedPlanResult {
    pub output_path: String,
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
        self.call_engine("convertHdr", "ssahdrify-cli-convert-hdr.js", "HDR", request)
    }

    pub fn resolve_hdr_output_path(&mut self, request: &HdrPathRequest) -> Result<String, String> {
        self.call_engine(
            "resolveHdrOutputPath",
            "ssahdrify-cli-resolve-hdr-output-path.js",
            "HDR",
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
            request,
        )
    }

    pub fn plan_rename(&mut self, request: &RenamePlanRequest) -> Result<RenamePlanResult, String> {
        self.call_engine(
            "planRename",
            "ssahdrify-cli-plan-rename.js",
            "Batch Rename",
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
            request,
        )
    }

    fn call_engine<Request, Response>(
        &mut self,
        function_name: &str,
        script_name: &'static str,
        label: &str,
        request: &Request,
    ) -> Result<Response, String>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        let request_json = serde_json::to_string(request)
            .map_err(|err| format!("failed to encode request: {err}"))?;
        // Build the JS call by string concatenation. Three load-bearing
        // invariants make this safe:
        //   1. serde_json::to_string emits RFC 8259 JSON, a strict
        //      subset of valid JS expression syntax (escapes ", \, and
        //      control chars; emits U+2028/U+2029 as literal bytes,
        //      legal inside JS string literals since ES2019).
        //   2. V8 14.7 (deno_core 0.400) is well past ES2019.
        //   3. function_name comes from a hardcoded &'static str at
        //      every call site — never user-controlled.
        // If any invariant shifts (downgrade to pre-ES2019, swap
        // serde_json for a non-JSON-superset writer, or accept dynamic
        // function_name), move to a v8 function-call path that takes
        // the argument as a v8::Value instead.
        let script = format!("globalThis.ssaHdrifyCliEngine.{function_name}({request_json})");

        let result = self
            .runtime
            .execute_script(script_name, script)
            .map_err(|err| format!("{label} conversion failed: {err}"))?;

        deno_core::scope!(scope, &mut self.runtime);
        let local = v8::Local::new(scope, result);
        serde_v8::from_v8(scope, local)
            .map_err(|err| format!("failed to decode {label} conversion result: {err}"))
    }
}
