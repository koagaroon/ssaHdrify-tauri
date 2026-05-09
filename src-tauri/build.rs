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

    // Distinguish "missing" (the expected first-build path before
    // `npm run build:engine` runs) from "found but unreadable"
    // (permission denied, transient I/O, partial-write from a
    // concurrent build:engine). Both fall through to the stub so the
    // build still succeeds, but a non-NotFound error gets a
    // cargo:warning so the developer notices the underlying cause.
    let source = match std::fs::read_to_string(&source_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => missing_engine_stub(),
        Err(error) => {
            println!(
                "cargo:warning=CLI engine bundle at {} could not be read: {error}; falling back to stub",
                source_path.display()
            );
            missing_engine_stub()
        }
    };

    std::fs::write(output_path, source).expect("failed to write CLI engine bundle for Cargo");
}

fn missing_engine_stub() -> String {
    const MESSAGE: &str =
        "CLI engine bundle is missing. Run `npm run build:engine` before building ssahdrify-cli.";
    // Every function on globalThis.ssaHdrifyCliEngine that the Rust
    // shell calls. The CLI's cheap-first ordering reaches the
    // resolveX*OutputPath functions FIRST per file; if those aren't
    // stubbed, a missing engine.js produces an inscrutable
    // "ssaHdrifyCliEngine.resolveHdrOutputPath is not a function"
    // V8 error instead of the friendly "Run `npm run build:engine`"
    // message this stub exists to surface. Keep this list in sync
    // with the methods on `CliEngine` in `bin/cli/engine.rs`.
    const FUNCTIONS: &[&str] = &[
        "convertHdr",
        "resolveHdrOutputPath",
        "convertShift",
        "resolveShiftOutputPath",
        "planRename",
        "planFontEmbed",
        "resolveEmbedOutputPath",
        "applyFontEmbed",
        "runChain",
        "resolveChainOutputPath",
    ];

    // Use serde_json (not Rust Debug) to escape MESSAGE into a JS-safe
    // string literal. Debug's escape rules diverge from JSON in edge
    // cases (e.g. \u{xxxx} vs \uXXXX, control-char rendering); future
    // edits to MESSAGE that introduce backticks, ${...}, or surrogate
    // pairs would otherwise emit malformed JS.
    let escaped_message =
        serde_json::to_string(MESSAGE).expect("serializing &str literal to JSON never fails");

    let methods = FUNCTIONS
        .iter()
        .map(|name| format!("{name}(){{throw new Error({escaped_message})}}"))
        .collect::<Vec<_>>()
        .join(",");

    format!("globalThis.ssaHdrifyCliEngine={{{methods}}};")
}
