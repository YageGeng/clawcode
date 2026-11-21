//! MCP elicitation forms: schema parsing and validation, without a window.

use std::collections::BTreeMap;

use agent_client_protocol::v2::schema as wire;
use desktop::ui::{
    ElicitationForm, FieldError, FieldKind, FieldValue, Unrenderable,
};
use serde_json::json;

/// Parses a requested schema from its wire JSON.
fn schema(value: serde_json::Value) -> wire::ElicitationSchema {
    serde_json::from_value(value).expect("valid schema")
}

#[test]
fn schema_fields_are_parsed() {
    let form = ElicitationForm::try_from(&schema(json!({
        "type": "object",
        "properties": {
            "email": { "type": "string", "title": "Email", "format": "email" },
            "plan": { "type": "string", "enum": ["free", "pro"] },
            "seats": { "type": "integer", "minimum": 1 },
            "agree": { "type": "boolean" },
            "tags": { "type": "array", "items": { "type": "string", "enum": ["x", "y"] } }
        },
        "required": ["email"]
    })))
    .expect("renderable");

    let kinds: BTreeMap<&str, &FieldKind> = form
        .fields()
        .iter()
        .map(|field| (field.name.as_str(), &field.kind))
        .collect();
    assert!(matches!(kinds["email"], FieldKind::Text { .. }));
    assert!(matches!(kinds["plan"], FieldKind::Choice { .. }));
    assert!(matches!(
        kinds["seats"],
        FieldKind::Number { integer: true, .. }
    ));
    assert!(matches!(kinds["agree"], FieldKind::Bool));
    assert!(matches!(kinds["tags"], FieldKind::MultiChoice { .. }));
    let email = form
        .fields()
        .iter()
        .find(|field| field.name == "email")
        .expect("email");
    assert!(email.required);
    assert_eq!(email.label, "Email");
}

#[test]
fn unrenderable_required_field_fails() {
    let missing = ElicitationForm::try_from(&schema(json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name", "ghost"]
    })));

    assert!(matches!(missing, Err(Unrenderable)));
}

#[test]
fn validation_reports_required_and_lengths() {
    let form = ElicitationForm::try_from(&schema(json!({
        "type": "object",
        "properties": {
            "code": { "type": "string", "maxLength": 3 },
            "name": { "type": "string" }
        },
        "required": ["name"]
    })))
    .expect("renderable");

    let errors = form
        .validate(&BTreeMap::from([
            ("code".to_string(), FieldValue::Text("日本語x".to_string())),
            ("name".to_string(), FieldValue::Text("  ".to_string())),
        ]))
        .expect_err("invalid");

    assert_eq!(errors.get("name"), Some(&FieldError::Required));
    assert_eq!(errors.get("code"), Some(&FieldError::TooLong(3)));
}

#[test]
fn empty_optional_fields_are_omitted() {
    let form = ElicitationForm::try_from(&schema(json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "note": { "type": "string" },
            "seats": { "type": "integer" }
        },
        "required": ["name"]
    })))
    .expect("renderable");

    let content = form
        .validate(&BTreeMap::from([
            ("name".to_string(), FieldValue::Text("x".to_string())),
            ("note".to_string(), FieldValue::Text(String::new())),
            ("seats".to_string(), FieldValue::Text("3".to_string())),
        ]))
        .expect("valid");

    assert_eq!(content.len(), 2);
    assert_eq!(
        content.get("name"),
        Some(&wire::ElicitationContentValue::String("x".to_string()))
    );
    assert_eq!(
        content.get("seats"),
        Some(&wire::ElicitationContentValue::Integer(3))
    );
}

#[test]
fn numbers_must_be_finite() {
    let form = ElicitationForm::try_from(&schema(json!({
        "type": "object",
        "properties": { "ratio": { "type": "number" } }
    })))
    .expect("renderable");

    // Rust parses these as floats, but JSON has no such numbers to send.
    for text in ["NaN", "inf", "-infinity"] {
        let errors = form
            .validate(&BTreeMap::from([(
                "ratio".to_string(),
                FieldValue::Text(text.to_string()),
            )]))
            .expect_err(text);
        assert_eq!(errors.get("ratio"), Some(&FieldError::Number), "{text}");
    }
}
