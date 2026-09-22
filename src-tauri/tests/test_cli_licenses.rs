//! The shipped CLI must expose its bundled notices without external files or services.

use std::fs;
use std::process::Command;

#[test]
fn licenses_prints_entities_bsd_notice_without_touching_cache_or_output_paths() {
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
    let entity_notice = text
        .split("\n\n========================================================================\n\n")
        .find(|section| section.starts_with("entities 8.1.0\n"))
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
    assert!(text.contains("this is not a complete native dependency license inventory"));
    assert!(text.contains(include_str!("../vendor/libsqlite3-sys/LICENSE")));
    assert!(text.contains("Source: https://github.com/rusqlite/rusqlite"));
    assert!(text.contains("SQLite's deliverable source is in the public domain."));
    assert!(!cache_path.exists());
    assert!(!output_path.exists());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    fs::remove_dir_all(root).unwrap();
}
