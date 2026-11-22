//! Display formatting: compact token counts and run usage lines.

use desktop::projection::ModelUsage;
use desktop::transcript::RunUsage;
use desktop::ui::TokenCount;

#[test]
fn token_counts_are_compact() {
    assert_eq!(TokenCount(999).label(), "999");
    assert_eq!(TokenCount(12_400).label(), "12.4k");
    assert_eq!(TokenCount(1_250_000).label(), "1.2M");
}

#[test]
fn run_usage_label_skips_missing_parts() {
    rust_i18n::set_locale("en");
    let usage = RunUsage {
        model: Some(("fixture".to_string(), "scripted".to_string())),
        usage: ModelUsage::builder().input(Some(1)).output(Some(1)).build(),
        elapsed_ms: Some(3_000),
    };

    assert_eq!(
        usage.label(Some("Scripted")),
        "Scripted · in 1 · out 1 · 3s"
    );
    assert_eq!(usage.label(None), "scripted · in 1 · out 1 · 3s");

    let cached = RunUsage {
        usage: ModelUsage::builder()
            .input(Some(12_400))
            .output(Some(1_800))
            .cache_read(Some(8_804))
            .build(),
        ..usage
    };
    assert_eq!(
        cached.label(Some("Scripted")),
        "Scripted · in 12.4k · out 1.8k · cache 71% · 3s"
    );
}

#[test]
fn precise_elapsed_shows_tenths_below_ten_seconds() {
    rust_i18n::set_locale("en");
    assert_eq!(desktop::ui::Elapsed(800).precise(), "0.8s");
    assert_eq!(desktop::ui::Elapsed(9_960).precise(), "9.9s");
    assert_eq!(
        desktop::ui::Elapsed(12_000).precise(),
        desktop::ui::Elapsed(12_000).label()
    );
}
