//! Integration tests for shared Prompt protocol types.

use std::path::PathBuf;

use protocol::PromptContentSource;

/// Prompt content sources preserve path and text variants across JSON boundaries.
#[test]
fn prompt_content_source_round_trips_supported_variants() {
    let cases = [
        PromptContentSource::Path(PathBuf::from("SYSTEM.custom.md")),
        PromptContentSource::Text("Keep responses concise.".to_string()),
    ];

    for source in cases {
        let encoded = serde_json::to_string(&source).expect("serialize source");
        let decoded: PromptContentSource =
            serde_json::from_str(&encoded).expect("deserialize source");

        assert_eq!(decoded, source);
    }
}

/// Prompt content sources reject incomplete tagged representations.
#[test]
fn prompt_content_source_rejects_missing_tag_or_value() {
    for malformed in [r#"{"value":"SYSTEM.md"}"#, r#"{"type":"path"}"#] {
        let result = serde_json::from_str::<PromptContentSource>(malformed);

        assert!(result.is_err(), "accepted malformed source: {malformed}");
    }
}
