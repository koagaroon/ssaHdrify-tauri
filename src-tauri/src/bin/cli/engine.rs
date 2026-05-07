use deno_core::{serde_v8, v8, JsRuntime, RuntimeOptions};
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
        let request_json = serde_json::to_string(request)
            .map_err(|err| format!("failed to encode request: {err}"))?;
        let script = format!("globalThis.ssaHdrifyCliEngine.convertHdr({request_json})");

        let result = self
            .runtime
            .execute_script("ssahdrify-cli-convert-hdr.js", script)
            .map_err(|err| format!("HDR conversion failed: {err}"))?;

        deno_core::scope!(scope, &mut self.runtime);
        let local = v8::Local::new(scope, result);
        serde_v8::from_v8(scope, local)
            .map_err(|err| format!("failed to decode HDR conversion result: {err}"))
    }
}
