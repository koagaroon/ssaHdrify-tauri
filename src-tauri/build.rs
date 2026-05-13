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
        Ok(content) if content.is_empty() => {
            // Round 7 Wave 7.6 (A2-R7-1): 0-byte engine.js is the
            // signature of a partial-write from a still-running
            // `npm run build:engine`, or a manually-truncated bundle
            // from a botched edit. Pre-W7.6 we'd embed the empty
            // string and let V8 fail at runtime with an inscrutable
            // "ssaHdrifyCliEngine is undefined" — the cargo:warning
            // surfaces the underlying cause at build time. Fall
            // through to the stub so the build still succeeds.
            println!(
                "cargo:warning=CLI engine bundle at {} is 0 bytes (partial write or truncation?); falling back to stub",
                source_path.display()
            );
            missing_engine_stub()
        }
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
    // shell might call OR might be visible from a call path (CliEngine
    // wraps some functions that are reachable but not directly invoked
    // — N-R5-RUSTCLI-03). The CLI's cheap-first ordering reaches the
    // resolveX*OutputPath functions FIRST per file; if those aren't
    // stubbed, a missing engine.js produces an inscrutable
    // "ssaHdrifyCliEngine.resolveHdrOutputPath is not a function"
    // V8 error instead of the friendly "Run `npm run build:engine`"
    // message this stub exists to surface. Keep this list a superset
    // of the methods on `CliEngine` in `bin/cli/engine.rs` —
    // over-stubbing one function costs ~30 bytes; under-stubbing leaks
    // the inscrutable V8 error.
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
        // resolveChainOutputPath is currently unused — chain output
        // prediction lives Rust-side in `predict_chain_output_path`.
        // Kept here in case a future refactor delegates back to JS
        // (cheap to keep, prevents the V8 inscrutable-error
        // regression class).
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
