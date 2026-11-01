#[path = "support/capture.rs"]
mod capture;
use capture::SentRequest;

/// Old Codex files cannot silently become upstream token-store directories.
#[test]
fn old_codex_auth_configuration_is_rejected() {
    let error = serde_json::from_value::<config::ProviderAuthConfig>(
        serde_json::json!({
            "type":"codex", "auth_file":"/example/auth.json"
        }),
    )
    .expect_err("old auth format must be rejected");
    assert!(error.to_string().contains("codex"));
}

/// The native token-store directory is an explicit supported authentication option.
#[test]
fn upstream_auth_configuration_roundtrips() {
    let value = serde_json::json!({"type":"chatgpt","token_store_dir":"/example/credentials"});
    let auth: config::ProviderAuthConfig =
        serde_json::from_value(value.clone()).expect("new auth");
    assert_eq!(serde_json::to_value(auth).expect("encode auth"), value);
}

/// Invalid paths are rejected before the upstream store can create any files.
#[test]
fn token_store_path_must_be_an_absolute_directory() {
    let file = tempfile::NamedTempFile::new().expect("temporary file");
    for path in ["", "relative", file.path().to_str().expect("path")] {
        let provider: config::LlmProvider = serde_json::from_value(serde_json::json!({
            "id":"chatgpt","display_name":"ChatGPT","provider_type":"responses",
            "base_url":"https://provider.invalid",
            "auth":{"type":"chatgpt","token_store_dir":path}
        })).expect("parse provider");
        assert!(provider.validate_auth_source().is_err());
    }
}

/// A nonexpired credential uses the upstream file format without network or Codex files.
#[tokio::test]
async fn upstream_file_token_store_supplies_a_cached_credential() {
    use rig_core::providers::chatgpt::auth::{
        AuthSource, Authenticator, DeviceCodeHandler, FileTokenStore,
        TokenStore,
    };
    let directory = tempfile::tempdir().expect("temporary store");
    let legacy = directory.path().join("auth.json");
    std::fs::write(&legacy, "untouched codex sentinel").expect("sentinel");
    let store = FileTokenStore::new(directory.path());
    store.save("chatgpt", &serde_json::json!({
        "access_token":"synthetic-access", "account_id":"synthetic-account", "expires_at":i64::MAX
    }).to_string()).await.expect("save synthetic credential");
    let auth = Authenticator::new(
        AuthSource::OAuth,
        store,
        DeviceCodeHandler::default(),
        false,
    );
    let http = rig_core::test_utils::RecordingHttpClient::new("{}");
    let context = auth
        .auth_context(&http)
        .await
        .expect("cached authentication");
    assert_eq!(context.api_key.expose(), "synthetic-access");
    assert_eq!(context.account_id.as_deref(), Some("synthetic-account"));
    assert!(http.requests().is_empty());
    assert_eq!(
        std::fs::read_to_string(legacy).expect("read sentinel"),
        "untouched codex sentinel"
    );
}
/// Authentication diagnostics retain categories and status without upstream error bodies.
#[test]
fn authentication_diagnostics_redact_untrusted_details() {
    use provider::ProviderConnectError;
    use rig_core::providers::chatgpt::auth::AuthError;
    let sentinel = "PRIVATE_AUTH_SENTINEL";
    let cases = [
        (AuthError::Message(sentinel.into()), "authentication flow"),
        (
            AuthError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                sentinel,
            )),
            "PermissionDenied",
        ),
        (
            AuthError::Json(
                serde_json::from_value::<u64>(serde_json::json!(sentinel))
                    .unwrap_err(),
            ),
            "JSON",
        ),
        (
            AuthError::Http(
                rig_core::http_client::Error::non_success_with_details(
                    http::StatusCode::UNAUTHORIZED,
                    http::HeaderMap::new(),
                    sentinel.into(),
                ),
            ),
            "HTTP 401",
        ),
        (
            AuthError::Store(Box::new(std::io::Error::other(sentinel))),
            "token store",
        ),
    ];
    for (error, category) in cases {
        let error = ProviderConnectError::Authentication(Box::new(error));
        let summary = error.to_string();
        assert!(!summary.contains(sentinel), "{summary}");
        // Debug output must redact as strictly as Display.
        let debug = format!("{error:?}");
        assert!(!debug.contains(sentinel), "{debug}");
        assert!(summary.contains(category), "{summary}");
        assert!(std::error::Error::source(&error).is_some());
    }
}

/// Models of one provider share one authenticator, so a resolved credential is not re-read per model.
#[tokio::test]
async fn provider_models_share_one_authenticator() {
    use rig_core::providers::chatgpt::auth::{FileTokenStore, TokenStore};
    let directory = tempfile::tempdir().expect("temporary store");
    let store = FileTokenStore::new(directory.path());
    let seed = |token: &str| {
        serde_json::json!({
            "access_token":token, "account_id":"synthetic-account", "expires_at":i64::MAX
        })
        .to_string()
    };
    store
        .save("chatgpt", &seed("first-synthetic"))
        .await
        .expect("seed first credential");
    let config: config::AppConfig = toml::from_str(&format!(
        r#"
active_model="chatgpt/a"
[[providers]]
id="chatgpt"
display_name="ChatGPT"
provider_type="responses"
base_url="https://provider.invalid"
auth={{type="chatgpt",token_store_dir={path:?}}}
[[providers.models]]
id="a"
context_tokens=32768
max_output_tokens=128
[[providers.models]]
id="b"
context_tokens=32768
max_output_tokens=128
"#,
        path = directory.path().to_str().expect("store path")
    ))
    .expect("config");
    let factory = provider::ProviderFactory::new(
        config::ConfigHandle::from_config(config),
    );
    let first = factory.resolve("chatgpt", "a").expect("model a");
    assert_eq!(
        first.sent_request().await.headers[http::header::AUTHORIZATION],
        "Bearer first-synthetic"
    );
    // A separate authenticator would read this record; a shared one serves its live credential.
    store
        .save("chatgpt", &seed("second-synthetic"))
        .await
        .expect("replace stored credential");
    let second = factory.resolve("chatgpt", "b").expect("model b");
    assert_eq!(
        second.sent_request().await.headers[http::header::AUTHORIZATION],
        "Bearer first-synthetic"
    );
}

/// ChatGPT without a protocol keeps the Responses route and its stored credential.
#[tokio::test]
async fn chatgpt_without_provider_type_uses_responses() {
    use rig_core::providers::chatgpt::auth::{FileTokenStore, TokenStore};
    let directory = tempfile::tempdir().expect("temporary store");
    FileTokenStore::new(directory.path())
        .save(
            "chatgpt",
            &serde_json::json!({
                "access_token":"fresh-synthetic", "account_id":"synthetic-account", "expires_at":i64::MAX
            })
            .to_string(),
        )
        .await
        .expect("seed credential");
    let config: config::AppConfig = toml::from_str(&format!(
        r#"
active_model="chatgpt/a"
[[providers]]
id="chatgpt"
display_name="ChatGPT"
base_url="https://provider.invalid"
auth={{type="chatgpt",token_store_dir={path:?}}}
[[providers.models]]
id="a"
context_tokens=32768
max_output_tokens=128
"#,
        path = directory.path().to_str().expect("store path")
    ))
    .expect("config");
    let factory = provider::ProviderFactory::new(
        config::ConfigHandle::from_config(config),
    );
    let configured = factory.resolve("chatgpt", "a").expect("model");
    let request = configured.sent_request().await;
    assert!(request.uri.ends_with("/responses"), "{}", request.uri);
    assert_eq!(
        request
            .headers
            .get(http::header::AUTHORIZATION)
            .expect("auth"),
        "Bearer fresh-synthetic"
    );
}
