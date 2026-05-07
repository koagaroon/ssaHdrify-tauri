fn main() {
    copy_cli_engine_bundle();
    tauri_build::build()
}

fn copy_cli_engine_bundle() {
    let manifest_dir = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    let source_path = manifest_dir.join("../dist-engine/engine.js");
    let out_dir =
        std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let output_path = out_dir.join("cli-engine.js");

    println!("cargo:rerun-if-changed={}", source_path.display());

    let source = std::fs::read_to_string(&source_path).unwrap_or_else(|_| missing_engine_stub());

    std::fs::write(output_path, source).expect("failed to write CLI engine bundle for Cargo");
}

fn missing_engine_stub() -> String {
    const MESSAGE: &str =
        "CLI engine bundle is missing. Run `npm run build:engine` before building ssahdrify-cli.";
    const FUNCTIONS: &[&str] = &[
        "convertHdr",
        "convertShift",
        "planRename",
        "planFontEmbed",
        "applyFontEmbed",
    ];

    let methods = FUNCTIONS
        .iter()
        .map(|name| format!("{name}(){{throw new Error({MESSAGE:?})}}"))
        .collect::<Vec<_>>()
        .join(",");

    format!("globalThis.ssaHdrifyCliEngine={{{methods}}};")
}
