use protocol::{ContentBlock, EmbeddedResourceContent};

/// MCP-compatible content blocks round-trip without converting typed payloads into text.
#[test]
fn mcp_content_blocks_round_trip_without_loss() {
    let blocks = vec![
        ContentBlock::Text {
            text: "plain".to_string(),
        },
        ContentBlock::Image {
            data: "aW1hZ2U=".to_string(),
            mime_type: "image/png".to_string(),
        },
        ContentBlock::Audio {
            data: "YXVkaW8=".to_string(),
            mime_type: "audio/wav".to_string(),
        },
        ContentBlock::EmbeddedResource {
            uri: "file:///workspace/readme.md".to_string(),
            mime_type: Some("text/markdown".to_string()),
            content: EmbeddedResourceContent::Text {
                text: "# Readme".to_string(),
            },
        },
        ContentBlock::EmbeddedResource {
            uri: "file:///workspace/archive.bin".to_string(),
            mime_type: Some("application/octet-stream".to_string()),
            content: EmbeddedResourceContent::Blob {
                data: "YmluYXJ5".to_string(),
            },
        },
        ContentBlock::ResourceLink {
            uri: "file:///workspace/report.pdf".to_string(),
            name: "report".to_string(),
            title: Some("Report".to_string()),
            description: Some("Generated report".to_string()),
            mime_type: Some("application/pdf".to_string()),
            size: Some(4_096),
        },
        ContentBlock::Structured {
            value: serde_json::json!({
                "matches": [{ "path": "src/lib.rs", "line": 42 }]
            }),
        },
    ];

    let encoded = serde_json::to_value(&blocks).expect("serialize content");
    assert_eq!(encoded[2]["type"], "audio");
    assert_eq!(encoded[3]["type"], "embedded_resource");
    assert_eq!(encoded[3]["content"]["type"], "text");
    assert_eq!(encoded[4]["content"]["type"], "blob");
    assert_eq!(encoded[5]["type"], "resource_link");
    assert_eq!(encoded[6]["type"], "structured");
    assert_eq!(
        serde_json::from_value::<Vec<ContentBlock>>(encoded)
            .expect("deserialize content"),
        blocks
    );
}

/// Text projection remains limited to actual Text blocks.
#[test]
fn text_accessor_does_not_flatten_mcp_content() {
    let structured = ContentBlock::Structured {
        value: serde_json::json!({ "ok": true }),
    };
    let resource = ContentBlock::EmbeddedResource {
        uri: "file:///workspace/data.json".to_string(),
        mime_type: Some("application/json".to_string()),
        content: EmbeddedResourceContent::Text {
            text: "{\"ok\":true}".to_string(),
        },
    };

    assert!(structured.text().is_none());
    assert!(resource.text().is_none());
}
