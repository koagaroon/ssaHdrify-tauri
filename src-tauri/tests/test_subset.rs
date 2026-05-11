//! Quick integration test: subset a real CJK font from the VCB-S pack.
//!
//! This test is **ignored by default** because it needs a machine-local
//! CJK font file that we cannot check into the repo (licensing). To run it:
//!   SSAHDRIFY_TEST_CJK_FONT="<path/to/font.ttf>" cargo test --test test_subset -- --ignored
//!
//! Without the environment variable — or without the font at that path —
//! the test is skipped instead of failing. This keeps `cargo test` green
//! across every developer's machine and CI while still letting the author
//! smoke-test the real subsetting pipeline on demand.

#[test]
#[ignore = "requires SSAHDRIFY_TEST_CJK_FONT env var pointing to a CJK .ttf"]
fn subset_real_cjk_font() {
    let Ok(font_path) = std::env::var("SSAHDRIFY_TEST_CJK_FONT") else {
        eprintln!("SSAHDRIFY_TEST_CJK_FONT not set — skipping");
        return;
    };
    let font_data = match std::fs::read(&font_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Cannot read {font_path}: {e} — skipping");
            return;
        }
    };
    let original_size = font_data.len();

    // Simulate a subtitle using ~50 Chinese characters + punctuation
    let subtitle_text = "你好世界，这是一个字幕测试。中文字体子集化可以显著减小文件体积！";
    let mut codepoints: Vec<u32> = subtitle_text.chars().map(|c| c as u32).collect();

    // Add safety padding (same as our production code)
    codepoints.extend(0x0020u32..=0x007Eu32); // ASCII printable
    codepoints.extend(0xFF01u32..=0xFF5Eu32); // CJK fullwidth
    codepoints.sort();
    codepoints.dedup();

    let subsetted = fontcull::subset_font_data_unicode(&font_data, &codepoints, &[])
        .expect("Subsetting failed");
    let subset_size = subsetted.len();

    let ratio = (subset_size as f64 / original_size as f64) * 100.0;
    println!("\n=== Font Subsetting Test ===");
    println!("Font:     {font_path}");
    println!(
        "Original: {} bytes ({:.1} MB)",
        original_size,
        original_size as f64 / 1024.0 / 1024.0
    );
    println!(
        "Subsetted: {} bytes ({:.1} KB)",
        subset_size,
        subset_size as f64 / 1024.0
    );
    println!("Ratio:    {:.1}% of original", ratio);
    println!(
        "Saved:    {:.1} MB",
        (original_size - subset_size) as f64 / 1024.0 / 1024.0
    );
    println!("Codepoints kept: {}", codepoints.len());

    // Verify: subset should be significantly smaller
    assert!(
        subset_size < original_size,
        "Subset should be smaller than original"
    );
    assert!(subset_size > 0, "Subset should not be empty");
    // For ~50 Chinese chars, the bound here is `ratio < 30.0` — a 70%
    // reduction floor (Round 1 A4.N-R1-12: a previous comment claimed
    // "90% reduction" which contradicted the 30.0 threshold). 70% is
    // already a strong signal that subsetting is doing real work on
    // CJK fonts; tightening to 90% would over-pin font-specific glyph
    // density.
    assert!(
        ratio < 30.0,
        "Expected significant size reduction, got {:.1}%",
        ratio
    );
}
