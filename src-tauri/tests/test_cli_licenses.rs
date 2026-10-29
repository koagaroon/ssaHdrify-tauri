//! The shipped CLI must expose its bundled notices without external files or services.

use sha2::{Digest, Sha256};
use std::fs;
use std::process::Command;

#[test]
fn licenses_prints_complete_embedded_notices_without_touching_cache_or_output_paths() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "ssahdrify-cli-licenses-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    let cache_path = root.join("missing-cache-directory/cache.sqlite3");
    let output_path = root.join("missing-output-directory");
    let output = Command::new(env!("CARGO_BIN_EXE_ssahdrify-cli"))
        .current_dir(&root)
        .args(["--cache-file"])
        .arg(&cache_path)
        .arg("--output-dir")
        .arg(&output_path)
        .arg("licenses")
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout)
        .unwrap()
        .replace("\r\n", "\n");
    assert_eq!(
        text,
        include_str!("../../dist-engine/third-party-notices.txt").replace("\r\n", "\n")
    );
    let package_lock: serde_json::Value =
        serde_json::from_str(include_str!("../../package-lock.json")).unwrap();
    let entities_version = package_lock["packages"]["node_modules/entities"]["version"]
        .as_str()
        .unwrap();
    let entity_notice = text
        .split("\n\n========================================================================\n\n")
        .find(|section| section.starts_with(&format!("entities {entities_version}\n")))
        .expect("entities version must have its own notice section");
    for required in [
        "License: BSD-2-Clause",
        "Copyright (c) Felix Böhm",
        "Redistributions of source code must retain the above copyright notice, this list of conditions and the following disclaimer.",
        "Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the following disclaimer in the documentation and/or other materials provided with the distribution.",
        "THIS IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS \"AS IS\"",
        "EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.",
    ] {
        assert!(entity_notice.contains(required), "missing notice text: {required}");
    }
    for required in [
        "Native dependency notices — x86_64-pc-windows-msvc",
        "deno_core 0.413.0",
        "v8 150.4.0",
        "Copyright Joyent, Inc. and other Node contributors.",
        "Copyright notices for The Rust Standard Library",
        "WebView2 SDK 1.0.3800.47",
    ] {
        assert!(text.contains(required), "missing native notice: {required}");
    }
    let bundle: serde_json::Value =
        serde_json::from_str(include_str!("../sqlite-bundle.json")).unwrap();
    let version = bundle["crate"]["version"].as_str().unwrap();
    let bindings = text
        .split("\n\n========================================================================\n\n")
        .find(|section| section.starts_with(&format!("libsqlite3-sys {version}\n")))
        .expect("the locked SQLite bindings must have their own notice section");
    assert!(bindings.contains(&format!(
        "Source: https://crates.io/crates/libsqlite3-sys/{version}"
    )));
    let license = bindings
        .split_once("\n\nLICENSE\n\n")
        .expect("the complete binding license must be embedded")
        .1;
    let license = format!("{}\n", license.trim_end());
    let license_hash: String = Sha256::digest(license.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(license_hash, bundle["files"]["LICENSE"].as_str().unwrap());
    assert!(text.contains("SQLite's deliverable source is in the public domain."));
    assert!(!cache_path.exists());
    assert!(!output_path.exists());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    fs::remove_dir_all(root).unwrap();
}
