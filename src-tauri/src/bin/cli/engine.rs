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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HdrConversionResult {
    pub output_path: String,
    pub content: String,
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

pub struct CliEngine {
    runtime: JsRuntime,
}

impl CliEngine {
    pub fn new() -> Result<Self, String> {
        let mut runtime = JsRuntime::new(RuntimeOptions::default());
        runtime
            .execute_script("ssahdrify-cli-engine.js", ENGINE_SOURCE)
            .map_err(|err| format!("failed to initialize CLI engine: {err}"))?;
        Ok(Self { runtime })
    }

    pub fn convert_hdr(
        &mut self,
        request: &HdrConversionRequest,
    ) -> Result<HdrConversionResult, String> {
        self.call_engine("convertHdr", "ssahdrify-cli-convert-hdr.js", "HDR", request)
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
