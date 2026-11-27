use deno_core::{serde_v8, v8, JsRuntime, RuntimeOptions};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize, Serializer};

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
///
/// `skipped_count` carries the count of captions whose text exceeded
/// MAX_CAPTION_TEXT_LEN (64 KB) and were emitted as skipped
/// placeholders. The shell stderr-surfaces this when > 0 to mirror
/// the GUI's `msg_oversized_skipped` warning.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HdrConversionResult {
    pub content: String,
    pub skipped_count: usize,
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
    // Keep absent optionals absent on the JS wire. The TS boundary also
    // normalizes nullish values defensively for direct/future callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing_map_rules: Option<Vec<TimingMapRule>>,
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
    /// Captions whose text exceeded MAX_CAPTION_TEXT_LEN (64 KB) and
    /// were emitted as skipped placeholders (see also
    /// `HdrConversionResult::skipped_count`).
    pub skipped_count: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingMapRule {
    pub start_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<i64>,
    pub offset_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingMapParseRequest {
    pub content: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingMapParseResult {
    pub rules: Vec<TimingMapRule>,
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
    pub source: String,
    pub no_op: bool,
    pub input_conflict: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FontEmbedPlanRequest {
    pub input_path: String,
    pub content: String,
    pub output_template: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FontDiagnosticsPlanRequest {
    pub content: String,
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

fn serialize_bytes_base64<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    use base64::Engine as _;
    serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// One font's subset bytes for the standalone `embed` flow's
/// `FontEmbedApplyRequest`. Rust keeps raw bytes here so chain mode can
/// still apply its raw-byte cumulative cap before encoding, but the
/// standalone V8 payload serializes the bytes as `{ dataB64: "..." }`.
/// This matches chain mode and avoids the ~4-5× heap expansion caused
/// by the old JSON number-array form.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FontSubsetPayload {
    pub font_name: String,
    #[serde(rename = "dataB64", serialize_with = "serialize_bytes_base64")]
    pub data: Vec<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontEmbedApplyResult {
    pub content: String,
    pub embedded_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StyleEditOperationsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_size: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StyleEditOperationsAck {
    pub valid: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StyleEditPathRequest {
    pub input_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StyleEditCliRequest {
    pub input_path: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_size: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StyleEditCliResult {
    pub content: String,
    pub changed_style_count: usize,
}

/// Result returned from the TS-side `runChain`. Single source of
/// truth for the chain output: `content` is what gets written,
/// `output_path` is where, `notes` is the per-step diagnostic
/// summary surfaced in the Rust shell's per-file report.
///
/// `skipped_count` is the largest per-step count of source captions whose
/// text exceeded MAX_CAPTION_TEXT_LEN (64 KB). The maximum avoids counting
/// a preserved cue again in each later shift step. The shell routes it through
/// the oversized-caption warning path (stderr + FileReport.warnings) to mirror
/// the standalone HDR / Shift CLI paths. Previously the
/// chain side relied on a string-suffix-in-note shape the Rust
/// shell never parsed; the typed field closes that gap.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainRunResult {
    pub content: String,
    pub output_path: String,
    pub notes: Vec<String>,
    pub skipped_count: usize,
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
    // deno_core captures this runtime's handle when constructing the
    // isolate; keep it alive so delayed V8 foreground tasks can run.
    // This field stays after `runtime` so the isolate unregisters first.
    _task_runtime: tokio::runtime::Runtime,
}

pub(super) const REQUIRED_ENGINE_METHODS: &[&str] = &[
    "convertHdr",
    "convertShift",
    "parseTimingMap",
    "planFontEmbed",
    "planFontDiagnostics",
    "applyFontEmbed",
    "planRename",
    "runChain",
    "resolveHdrOutputPath",
    "resolveShiftOutputPath",
    "resolveEmbedOutputPath",
    "validateStyleEditCliOperations",
    "resolveStyleEditOutputPath",
    "applyCliStyleEdit",
];

impl CliEngine {
    pub fn new() -> Result<Self, String> {
        let task_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .build()
            .map_err(|err| format!("failed to create CLI engine task runtime: {err}"))?;
        let task_runtime_guard = task_runtime.enter();

        // (scope tightening): only the `extensions`
        // field is explicitly pinned to empty — the CLI engine runs
        // only pure text transformations (no fs / net / timer ops).
        // The rest of `RuntimeOptions` (`module_loader`,
        // `startup_snapshot`, `op_state`, etc.) uses
        // `Default::default()` and DOES inherit future deno_core
        // additions. A future deno_core upgrade that adds a new
        // field defaulting to something privilege-bearing (a real op
        // surface, an unconstrained module loader) would silently
        // become available here. Re-audit `RuntimeOptions`'s field
        // list on every deno_core bump alongside the API-binding
        // audit pinned in Cargo.toml's deno_core pin WHY.
        // In 0.413, both residual lazy-source tables default to empty,
        // and the default module loader remains capability-free.
        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: vec![],
            ..Default::default()
        });
        drop(task_runtime_guard);

        // Silence V8's default console binding so shared TS library
        // calls (`console.debug` / `console.warn` / `console.log`) can
        // never write directly to stdout (corrupting `--json`) or
        // bypass `--quiet` on stderr. Structured diagnostics return as
        // typed `warnings` / `notes` fields instead.
        runtime
            .execute_script(
                "ssahdrify-cli-console-guard.js",
                "if (typeof globalThis.console === 'object' && globalThis.console !== null) {\
                    const noop = () => {};\
                    globalThis.console.log = noop;\
                    globalThis.console.info = noop;\
                    globalThis.console.debug = noop;\
                    globalThis.console.warn = noop;\
                    globalThis.console.error = noop;\
                 }",
            )
            .map_err(|err| format!("failed to configure CLI engine console guard: {err}"))?;

        runtime
            .execute_script("ssahdrify-cli-engine.js", ENGINE_SOURCE)
            .map_err(|err| format!("failed to initialize CLI engine: {err}"))?;

        // Probe that the bundle wired up every expected engine method.
        let mut probe_checks = vec![
            "typeof globalThis.ssaHdrifyCliEngine === 'object'".to_string(),
            "globalThis.ssaHdrifyCliEngine !== null".to_string(),
        ];
        for method in REQUIRED_ENGINE_METHODS {
            probe_checks.push(format!(
                "typeof globalThis.ssaHdrifyCliEngine.{method} === 'function'"
            ));
        }
        let probe_source = probe_checks.join(" && ");
        let probe = runtime
            .execute_script("ssahdrify-cli-engine-probe.js", probe_source)
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
        Ok(Self {
            runtime,
            _task_runtime: task_runtime,
        })
    }

    pub fn convert_hdr(
        &mut self,
        request: &HdrConversionRequest,
    ) -> Result<HdrConversionResult, String> {
        self.call_engine("convertHdr", "HDR", "conversion", request)
    }

    pub fn resolve_hdr_output_path(&mut self, request: &HdrPathRequest) -> Result<String, String> {
        self.call_engine("resolveHdrOutputPath", "HDR", "path resolution", request)
    }

    pub fn resolve_shift_output_path(
        &mut self,
        request: &ShiftPathRequest,
    ) -> Result<String, String> {
        self.call_engine(
            "resolveShiftOutputPath",
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
            "Font Embed",
            "path resolution",
            request,
        )
    }

    pub fn convert_shift(
        &mut self,
        request: &ShiftConversionRequest,
    ) -> Result<ShiftConversionResult, String> {
        self.call_engine("convertShift", "Time Shift", "conversion", request)
    }

    pub fn parse_timing_map(
        &mut self,
        request: &TimingMapParseRequest,
    ) -> Result<TimingMapParseResult, String> {
        self.call_engine("parseTimingMap", "Timing map", "parse", request)
    }

    pub fn plan_rename(&mut self, request: &RenamePlanRequest) -> Result<RenamePlanResult, String> {
        self.call_engine("planRename", "Batch Rename", "plan", request)
    }

    pub fn plan_font_embed(
        &mut self,
        request: &FontEmbedPlanRequest,
    ) -> Result<FontEmbedPlanResult, String> {
        self.call_engine("planFontEmbed", "Font Embed", "plan", request)
    }

    pub fn plan_font_diagnostics(
        &mut self,
        request: &FontDiagnosticsPlanRequest,
    ) -> Result<FontEmbedPlanResult, String> {
        self.call_engine("planFontDiagnostics", "Font Diagnostics", "plan", request)
    }

    pub fn apply_font_embed(
        &mut self,
        request: &FontEmbedApplyRequest,
    ) -> Result<FontEmbedApplyResult, String> {
        self.call_engine("applyFontEmbed", "Font Embed", "embed", request)
    }

    pub fn run_chain(&mut self, request: &ChainRunRequest) -> Result<ChainRunResult, String> {
        self.call_engine("runChain", "Chain", "execution", request)
    }

    pub fn validate_style_edit_operations(
        &mut self,
        request: &StyleEditOperationsRequest,
    ) -> Result<(), String> {
        let ack: StyleEditOperationsAck = self.call_engine(
            "validateStyleEditCliOperations",
            "Style Edit",
            "validation",
            request,
        )?;
        if !ack.valid {
            return Err("Style Edit operation validation did not pass".to_string());
        }
        Ok(())
    }

    pub fn resolve_style_edit_output_path(
        &mut self,
        request: &StyleEditPathRequest,
    ) -> Result<String, String> {
        self.call_engine(
            "resolveStyleEditOutputPath",
            "Style Edit",
            "path resolution",
            request,
        )
    }

    pub fn apply_style_edit(
        &mut self,
        request: &StyleEditCliRequest,
    ) -> Result<StyleEditCliResult, String> {
        self.call_engine("applyCliStyleEdit", "Style Edit", "execution", request)
    }

    fn call_engine<Request, Response>(
        &mut self,
        function_name: &'static str,
        label: &str,
        step: &str,
        request: &Request,
    ) -> Result<Response, String>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        // Serialize via serde_json and parse with V8's C++ RFC 8259 JSON
        // parser (`v8::json::parse`), then invoke the target method directly
        // via `v8::Function::call`. This preserves 100% identical wire
        // semantics (`#[serde(flatten)]`, `skip_serializing_if`, base64
        // custom serializers) while avoiding per-file JS script compilation,
        // source-line echo hazards, and `globalThis` state mutation.
        let request_json = serde_json::to_string(request)
            .map_err(|err| format!("failed to encode {label} request: {err}"))?;

        deno_core::scope!(scope, &mut self.runtime);
        let json_v8_str = v8::String::new(scope, &request_json)
            .ok_or_else(|| format!("failed to allocate V8 string for {label} request"))?;
        let arg_val = v8::json::parse(scope, json_v8_str)
            .ok_or_else(|| format!("failed to parse {label} JSON request in V8"))?;

        let global = scope.get_current_context().global(scope);
        let engine_key = v8::String::new(scope, "ssaHdrifyCliEngine")
            .ok_or_else(|| "failed to allocate engine key".to_string())?;
        let engine_val = global
            .get(scope, engine_key.into())
            .ok_or_else(|| "globalThis.ssaHdrifyCliEngine is unavailable".to_string())?;
        let engine_obj = v8::Local::<v8::Object>::try_from(engine_val)
            .map_err(|_| "globalThis.ssaHdrifyCliEngine is not an object".to_string())?;

        let fn_key = v8::String::new(scope, function_name)
            .ok_or_else(|| format!("failed to allocate method key '{function_name}'"))?;
        let fn_val = engine_obj
            .get(scope, fn_key.into())
            .ok_or_else(|| format!("ssaHdrifyCliEngine.{function_name} is unavailable"))?;
        let func = v8::Local::<v8::Function>::try_from(fn_val)
            .map_err(|_| format!("ssaHdrifyCliEngine.{function_name} is not a function"))?;

        v8::tc_scope!(let tc_scope, scope);
        let Some(result) = func.call(tc_scope, engine_obj.into(), &[arg_val]) else {
            let err_msg = tc_scope
                .exception()
                .map(|exc| deno_core::error::JsError::from_v8_exception(tc_scope, exc).to_string())
                .unwrap_or_else(|| "unknown JavaScript exception".to_string());
            return Err(format!("{label} {step} failed: {err_msg}"));
        };

        serde_v8::from_v8(tc_scope, result)
            .map_err(|err| format!("failed to decode {label} {step} result: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{FontEmbedApplyRequest, FontSubsetPayload, ShiftConversionRequest};

    #[test]
    fn shift_request_omits_both_absent_optionals_from_json_wire_shape() {
        let request = ShiftConversionRequest {
            input_path: r"C:\subs\episode.ass".to_string(),
            content: "[Script Info]\n".to_string(),
            offset_ms: 1000,
            threshold_ms: None,
            timing_map_rules: None,
            output_template: "{name}.shifted{ext}".to_string(),
        };

        let json = serde_json::to_value(request).expect("request should serialize");
        assert!(
            json.get("thresholdMs").is_none(),
            "None threshold must be omitted rather than serialized as null"
        );
        assert!(
            json.get("timingMapRules").is_none(),
            "None timing map must be omitted rather than serialized as null"
        );
    }

    #[test]
    fn font_subset_payload_serializes_bytes_as_base64() {
        const CASES: &[(&[u8], &str)] = &[(&[0], "AA=="), (&[0, 1], "AAE="), (&[0, 1, 2], "AAEC")];

        for &(data, expected) in CASES {
            let request = FontEmbedApplyRequest {
                content: "[Script Info]\nScriptType: v4.00+\n".to_string(),
                fonts: vec![FontSubsetPayload {
                    font_name: "arial.ttf".to_string(),
                    data: data.to_vec(),
                }],
            };

            let json = serde_json::to_value(&request).expect("serialize request");
            let font = &json["fonts"][0];
            assert_eq!(font["fontName"], "arial.ttf");
            assert_eq!(font["dataB64"], expected);
            assert!(font.get("data").is_none());
        }
    }
}
