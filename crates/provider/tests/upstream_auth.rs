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

/// The previous field name fails loudly instead of being silently ignored.
#[test]
fn old_token_store_dir_name_is_rejected() {
    let error = serde_json::from_value::<config::ProviderAuthConfig>(
        serde_json::json!({
            "type":"chatgpt", "token_store_dir":"/example/credentials"
        }),
    )
    .expect_err("old field name must be rejected");
    assert!(error.to_string().contains("token_store_dir"), "{error}");
}

/// The native credential directory is an explicit supported authentication option.
#[test]
fn upstream_auth_configuration_roundtrips() {
    let value =
        serde_json::json!({"type":"chatgpt","auth_dir":"/example/credentials"});
    let auth: config::ProviderAuthConfig =
        serde_json::from_value(value.clone()).expect("new auth");
    assert_eq!(serde_json::to_value(auth).expect("encode auth"), value);
}

/// Invalid paths are rejected before the upstream store can create any files.
#[test]
fn auth_dir_must_be_an_absolute_directory() {
    let file = tempfile::NamedTempFile::new().expect("temporary file");
    let provider = |path: &str| -> config::LlmProvider {
        serde_json::from_value(serde_json::json!({
            "id":"chatgpt","display_name":"ChatGPT","provider_type":"responses",
            "base_url":"https://provider.invalid",
            "auth":{"type":"chatgpt","auth_dir":path}
        }))
        .expect("parse provider")
    };
    for path in [
        "",
        "relative",
        "~other/auth",
        "./~/auth",
        file.path().to_str().expect("path"),
    ] {
        assert!(provider(path).validate_auth_source().is_err(), "{path}");
    }
    // A home-relative directory is as explicit as an absolute one.
    provider("~/.clawcode-absent/chatgpt")
        .validate_auth_source()
        .expect("home-relative directory");
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
auth={{type="chatgpt",auth_dir={path:?}}}
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
auth={{type="chatgpt",auth_dir={path:?}}}
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

mod sign_in {
    use std::future::Future;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use http::StatusCode;
    use protocol::{DeviceCode, DeviceCodeSink};
    use provider::{ConfiguredModel, ProviderConnectError, ProviderFactory};
    use rig_core::http_client::{
        self, Bytes, DynHttpClient, HttpClientExt, LazyBody, MultipartForm,
        Request, Response, StreamingResponse,
    };
    use rig_core::providers::chatgpt::auth::{FileTokenStore, TokenStore};
    use rig_core::test_utils::{
        MockHttpResponse, RecordingHttpClient, SequencedHttpClient,
    };
    use rig_core::wasm_compat::WasmCompatSend;
    use tokio::sync::Semaphore;

    /// Holds requests after the first `free` until the gate opens, then answers in order.
    #[derive(Clone)]
    struct GatedHttpClient {
        gate: Arc<Semaphore>,
        free: Arc<AtomicUsize>,
        inner: SequencedHttpClient,
    }

    impl GatedHttpClient {
        /// Lets `free` requests through at once and scripts `responses` for all of them.
        fn new(
            free: usize,
            responses: impl IntoIterator<Item = MockHttpResponse>,
        ) -> Self {
            Self {
                gate: Arc::new(Semaphore::new(0)),
                free: Arc::new(AtomicUsize::new(free)),
                inner: SequencedHttpClient::new(responses),
            }
        }

        /// Reports whether this request passes without waiting for the gate.
        fn takes_a_free_pass(&self) -> bool {
            self.free
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |free| {
                    free.checked_sub(1)
                })
                .is_ok()
        }
    }

    impl HttpClientExt for GatedHttpClient {
        /// Takes the scripted reply now, then holds it until the gate opens.
        fn send<T, U>(
            &self,
            req: Request<T>,
        ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>>
        + WasmCompatSend
        + 'static
        where
            T: Into<Bytes> + WasmCompatSend,
            U: From<Bytes> + WasmCompatSend + 'static,
        {
            let gate =
                (!self.takes_a_free_pass()).then(|| Arc::clone(&self.gate));
            let response = self.inner.send::<T, U>(req);
            async move {
                if let Some(gate) = gate {
                    // A closed gate stands for a network that never answers.
                    let _permit = gate.acquire().await.map_err(|_closed| {
                        http_client::Error::Instance("offline".into())
                    })?;
                }
                response.await
            }
        }

        /// Delegates, because sign-in traffic is never multipart.
        fn send_multipart<U>(
            &self,
            req: Request<MultipartForm>,
        ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>>
        + WasmCompatSend
        + 'static
        where
            U: From<Bytes> + WasmCompatSend + 'static,
        {
            self.inner.send_multipart::<U>(req)
        }

        /// Delegates, because sign-in traffic is never streamed.
        fn send_streaming<T>(
            &self,
            req: Request<T>,
        ) -> impl Future<Output = http_client::Result<StreamingResponse>>
        + WasmCompatSend
        where
            T: Into<Bytes> + WasmCompatSend,
        {
            self.inner.send_streaming::<T>(req)
        }
    }

    /// Builds a ChatGPT provider with models `a` and `b` over one injected client.
    fn chatgpt_factory(dir: &Path, http: DynHttpClient) -> ProviderFactory {
        let config: config::AppConfig = toml::from_str(&format!(
            r#"
active_model="chatgpt/a"
[[providers]]
id="chatgpt"
display_name="ChatGPT"
base_url="https://provider.invalid"
auth={{type="chatgpt",auth_dir={path:?}}}
[[providers.models]]
id="a"
context_tokens=32768
max_output_tokens=128
[[providers.models]]
id="b"
context_tokens=32768
max_output_tokens=128
"#,
            path = dir.to_str().expect("store path")
        ))
        .expect("config");
        ProviderFactory::with_http(
            config::ConfigHandle::from_config(config),
            http,
        )
    }

    /// Resolves model `a` of a ChatGPT provider over one injected client.
    fn chatgpt_model(
        dir: &Path,
        http: impl HttpClientExt + 'static,
    ) -> Arc<ConfiguredModel> {
        chatgpt_factory(dir, DynHttpClient::new(http))
            .resolve("chatgpt", "a")
            .expect("model")
    }

    /// Writes one upstream credential record.
    async fn seed(dir: &Path, record: &serde_json::Value) {
        FileTokenStore::new(dir)
            .save("chatgpt", &record.to_string())
            .await
            .expect("seed credential");
    }

    /// Returns an upstream record whose access token cannot be used.
    fn expired_record() -> serde_json::Value {
        serde_json::json!({"refresh_token":"r","expires_at":0})
    }

    /// Returns an upstream record that needs no network.
    fn live_record() -> serde_json::Value {
        serde_json::json!({
            "access_token":"t", "account_id":"a", "expires_at":i64::MAX
        })
    }

    /// Asserts the typed sign-in failure naming the ChatGPT method.
    fn assert_sign_in(error: &ProviderConnectError) {
        assert!(
            matches!(
                error,
                ProviderConnectError::SignInRequired { method_id }
                    if method_id == "chatgpt"
            ),
            "{error}"
        );
    }

    /// Connects over a throwaway client and returns only the failure.
    async fn connect_error(
        model: &ConfiguredModel,
    ) -> Option<ProviderConnectError> {
        model
            .connect(DynHttpClient::new(RecordingHttpClient::new("")))
            .await
            .err()
    }

    /// No credential asks for a sign-in on both paths and never starts a device flow.
    #[tokio::test]
    async fn missing_credential_requires_sign_in_without_device_flow() {
        let directory = tempfile::tempdir().expect("store");
        let http = RecordingHttpClient::new("");
        let model = chatgpt_model(directory.path(), http.clone());
        assert_sign_in(&model.ensure_signed_in().await.expect_err("check"));
        assert_sign_in(&connect_error(&model).await.expect("connect"));
        assert!(http.requests().is_empty(), "{:?}", http.requests());
    }

    /// An unexpired credential passes without any request.
    #[tokio::test]
    async fn live_credential_passes_without_network() {
        let directory = tempfile::tempdir().expect("store");
        seed(directory.path(), &live_record()).await;
        let http = RecordingHttpClient::new("");
        let model = chatgpt_model(directory.path(), http.clone());
        model.ensure_signed_in().await.expect("signed in");
        assert!(http.requests().is_empty(), "{:?}", http.requests());
    }

    /// A refresh token the server rejects means the user must sign in again.
    #[tokio::test]
    async fn refresh_invalid_grant_requires_sign_in() {
        let directory = tempfile::tempdir().expect("store");
        seed(directory.path(), &expired_record()).await;
        let http = RecordingHttpClient::with_error(
            StatusCode::BAD_REQUEST,
            r#"{"error":"invalid_grant"}"#,
        );
        let model = chatgpt_model(directory.path(), http.clone());
        assert_sign_in(&model.ensure_signed_in().await.expect_err("check"));
        assert_eq!(http.requests().len(), 1, "one refresh attempt");
    }

    /// A refresh outage is not a sign-in problem, so only the request reports it.
    #[tokio::test]
    async fn refresh_server_error_does_not_intercept() {
        let directory = tempfile::tempdir().expect("store");
        seed(directory.path(), &expired_record()).await;
        let http = RecordingHttpClient::with_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "down",
        );
        let model = chatgpt_model(directory.path(), http);
        model
            .ensure_signed_in()
            .await
            .expect("not a sign-in failure");
        assert!(matches!(
            connect_error(&model).await,
            Some(ProviderConnectError::Authentication(_))
        ));
    }

    /// A network failure during refresh is not a sign-in problem either.
    #[tokio::test]
    async fn refresh_transport_error_does_not_intercept() {
        let directory = tempfile::tempdir().expect("store");
        seed(directory.path(), &expired_record()).await;
        let http = GatedHttpClient::new(0, []);
        http.gate.close();
        let model = chatgpt_model(directory.path(), http);
        model
            .ensure_signed_in()
            .await
            .expect("not a sign-in failure");
    }

    /// Server text that merely contains the phrase cannot fake the upstream signal.
    #[tokio::test]
    async fn refresh_error_mentioning_the_phrase_is_not_sign_in() {
        let directory = tempfile::tempdir().expect("store");
        seed(directory.path(), &expired_record()).await;
        let http = RecordingHttpClient::with_error(
            StatusCode::BAD_REQUEST,
            r#"{"error":"server_error","error_description":"sign-in required upstream"}"#,
        );
        let model = chatgpt_model(directory.path(), http);
        model
            .ensure_signed_in()
            .await
            .expect("not a sign-in failure");
    }

    /// The check stops waiting at its limit while the refresh still finishes and is kept.
    #[tokio::test(start_paused = true)]
    async fn slow_refresh_survives_the_check_limit() {
        let directory = tempfile::tempdir().expect("store");
        seed(directory.path(), &expired_record()).await;
        let http = GatedHttpClient::new(
            0,
            [MockHttpResponse::success(
                r#"{"access_token":"rotated","refresh_token":"rotated-refresh"}"#,
            )],
        );
        let model = chatgpt_model(directory.path(), http.clone());
        model.ensure_signed_in().await.expect("timed out quietly");

        // The check gave up waiting, not refreshing: the rotated token still lands.
        http.gate.add_permits(1);
        let stored = directory.path().join("chatgpt.json");
        for _attempt in 0..100 {
            let record = std::fs::read_to_string(&stored).expect("record");
            if record.contains("rotated-refresh") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let record = std::fs::read_to_string(&stored).expect("record");
        assert!(record.contains("rotated-refresh"), "{record}");
    }

    /// A 401 makes every model of the provider ask for a sign-in despite an unexpired token.
    #[tokio::test]
    async fn unauthorized_response_requires_sign_in() {
        let directory = tempfile::tempdir().expect("store");
        seed(directory.path(), &live_record()).await;
        let factory = chatgpt_factory(
            directory.path(),
            DynHttpClient::new(RecordingHttpClient::new("")),
        );
        let first = factory.resolve("chatgpt", "a").expect("model a");
        let second = factory.resolve("chatgpt", "b").expect("model b");
        first.ensure_signed_in().await.expect("signed in");
        first.note_failure_status(Some(401));
        for model in [&first, &second] {
            assert_sign_in(&model.ensure_signed_in().await.expect_err("check"));
            assert_sign_in(&connect_error(model).await.expect("connect"));
        }
    }

    /// A 403 can mean the plan lacks the model, so the credential stays valid.
    #[tokio::test]
    async fn forbidden_response_keeps_the_credential() {
        let directory = tempfile::tempdir().expect("store");
        seed(directory.path(), &live_record()).await;
        let model =
            chatgpt_model(directory.path(), RecordingHttpClient::new(""));
        model.note_failure_status(Some(403));
        model.note_failure_status(None);
        model.ensure_signed_in().await.expect("signed in");
    }

    /// Builds a provider that authenticates with a key rather than a sign-in.
    fn api_key_factory() -> ProviderFactory {
        let config: config::AppConfig = toml::from_str(
            r#"
active_model="corp/a"
[[providers]]
id="corp"
display_name="Corp"
provider_type="openai-completions"
base_url="https://provider.invalid"
api_key="synthetic"
[[providers.models]]
id="a"
context_tokens=32768
max_output_tokens=128
"#,
        )
        .expect("config");
        ProviderFactory::with_http(
            config::ConfigHandle::from_config(config),
            DynHttpClient::new(RecordingHttpClient::new("")),
        )
    }

    /// A keyed provider offers no sign-in and ignores 401 marks.
    #[tokio::test]
    async fn api_key_provider_has_no_sign_in() {
        let factory = api_key_factory();
        let model = factory.resolve("corp", "a").expect("model");
        assert_eq!(model.login_method(), None);
        model.note_failure_status(Some(401));
        model.ensure_signed_in().await.expect("no sign-in");
    }

    /// The method uses the provider identifier and display name.
    #[tokio::test]
    async fn login_method_names_the_provider() {
        let directory = tempfile::tempdir().expect("store");
        let model =
            chatgpt_model(directory.path(), RecordingHttpClient::new(""));
        let method = model.login_method().expect("method");
        assert_eq!(method.id, "chatgpt");
        assert_eq!(method.name, "ChatGPT");
    }

    /// A fresh install asks for a sign-in instead of failing, and stores no credential.
    #[tokio::test]
    async fn absent_auth_dir_requires_sign_in() {
        let directory = tempfile::tempdir().expect("store");
        let absent = directory.path().join("not-yet").join("created");
        let model = chatgpt_model(&absent, RecordingHttpClient::new(""));
        assert_sign_in(&model.ensure_signed_in().await.expect_err("check"));
        assert!(!absent.join("chatgpt.json").exists());
    }

    /// A broken directory is reported by the request, not disguised as a missing sign-in.
    #[tokio::test]
    async fn unusable_auth_dir_does_not_intercept() {
        let file = tempfile::NamedTempFile::new().expect("file");
        let model = chatgpt_model(
            &file.path().join("below-a-file"),
            RecordingHttpClient::new(""),
        );
        model
            .ensure_signed_in()
            .await
            .expect("not a sign-in failure");
        assert!(matches!(
            connect_error(&model).await,
            Some(ProviderConnectError::Authentication(_))
        ));
    }

    /// A record without tokens is the same as having none.
    #[tokio::test]
    async fn partial_record_requires_sign_in() {
        let directory = tempfile::tempdir().expect("store");
        seed(directory.path(), &serde_json::json!({"account_id":"a"})).await;
        let model =
            chatgpt_model(directory.path(), RecordingHttpClient::new(""));
        assert_sign_in(&model.ensure_signed_in().await.expect_err("check"));
    }

    /// Returns the replies of an approved sign-in: code, authorization, tokens.
    fn device_sign_in_replies() -> [MockHttpResponse; 3] {
        [
            MockHttpResponse::success(
                r#"{"device_auth_id":"d1","user_code":"UC-1","interval":"1"}"#,
            ),
            MockHttpResponse::success(
                r#"{"authorization_code":"code","code_verifier":"verifier"}"#,
            ),
            MockHttpResponse::success(
                r#"{"access_token":"fresh","refresh_token":"fresh-refresh"}"#,
            ),
        ]
    }

    /// Returns a sink plus the codes it has received.
    fn code_sink() -> (DeviceCodeSink, Arc<Mutex<Vec<DeviceCode>>>) {
        let codes = Arc::new(Mutex::new(Vec::new()));
        let received = Arc::clone(&codes);
        let sink: DeviceCodeSink = Arc::new(move |code| {
            received.lock().expect("codes").push(code);
        });
        (sink, codes)
    }

    /// One sign-in shows its code, stores the tokens, and serves the whole provider.
    #[tokio::test]
    async fn device_sign_in_serves_every_model_of_the_provider() {
        let directory = tempfile::tempdir().expect("store");
        let factory = chatgpt_factory(
            directory.path(),
            DynHttpClient::new(SequencedHttpClient::new(
                device_sign_in_replies(),
            )),
        );
        let (sink, codes) = code_sink();
        let first = factory.resolve("chatgpt", "a").expect("model a");
        first.sign_in(sink).await.expect("signed in");
        assert_eq!(
            *codes.lock().expect("codes"),
            [DeviceCode {
                verification_uri: "https://auth.openai.com/codex/device".into(),
                user_code: "UC-1".into(),
            }]
        );
        let record =
            std::fs::read_to_string(directory.path().join("chatgpt.json"))
                .expect("record");
        assert!(record.contains("fresh-refresh"), "{record}");
        let second = factory.resolve("chatgpt", "b").expect("model b");
        second.ensure_signed_in().await.expect("shared credential");
    }

    /// A new sign-in replaces a credential the model endpoint rejected.
    #[tokio::test]
    async fn sign_in_clears_the_invalidated_mark() {
        let directory = tempfile::tempdir().expect("store");
        let model = chatgpt_model(
            directory.path(),
            SequencedHttpClient::new(device_sign_in_replies()),
        );
        model.note_failure_status(Some(401));
        let (sink, _codes) = code_sink();
        model.sign_in(sink).await.expect("signed in");
        model.ensure_signed_in().await.expect("mark cleared");
    }

    /// A failed code request reports an error, shows nothing, and stores nothing.
    #[tokio::test]
    async fn failed_device_request_reports_authentication() {
        let directory = tempfile::tempdir().expect("store");
        let (sink, codes) = code_sink();
        let model = chatgpt_model(
            directory.path(),
            SequencedHttpClient::new([MockHttpResponse::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "down",
            )]),
        );
        assert!(matches!(
            model.sign_in(sink).await,
            Err(ProviderConnectError::Authentication(_))
        ));
        assert!(codes.lock().expect("codes").is_empty());
        assert!(!directory.path().join("chatgpt.json").exists());
    }

    /// A stalled code request ends, because no client can cancel before a code exists.
    #[tokio::test(start_paused = true)]
    async fn stalled_device_request_times_out() {
        let directory = tempfile::tempdir().expect("store");
        let (sink, codes) = code_sink();
        let model = chatgpt_model(
            directory.path(),
            GatedHttpClient::new(0, device_sign_in_replies()),
        );
        let error = model.sign_in(sink).await.expect_err("timed out");
        assert!(error.to_string().contains("timed out"), "{error}");
        assert!(codes.lock().expect("codes").is_empty());
    }

    /// Once the code exists the user may take as long as they need.
    #[tokio::test(start_paused = true)]
    async fn slow_authorization_is_not_limited_after_the_code() {
        let directory = tempfile::tempdir().expect("store");
        let http = GatedHttpClient::new(1, device_sign_in_replies());
        let (sink, codes) = code_sink();
        let model = chatgpt_model(directory.path(), http.clone());
        let sign_in = tokio::spawn(async move { model.sign_in(sink).await });
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(!sign_in.is_finished());
        assert_eq!(codes.lock().expect("codes").len(), 1);
        http.gate.add_permits(1);
        sign_in.await.expect("join").expect("signed in");
    }

    /// A keyed provider has nothing to sign in to.
    #[tokio::test]
    async fn sign_in_without_auth_fails() {
        let factory = api_key_factory();
        let (sink, _codes) = code_sink();
        let model = factory.resolve("corp", "a").expect("model");
        let error = model.sign_in(sink).await.expect_err("no sign-in");
        assert!(error.to_string().contains("no sign-in"), "{error}");
    }

    /// Formatting a device code for a log cannot reveal it.
    #[test]
    fn device_code_debug_hides_the_code() {
        let code = DeviceCode {
            verification_uri: "https://example.test/device".into(),
            user_code: "SECRET-CODE".into(),
        };
        assert!(!format!("{code:?}").contains("SECRET-CODE"));
    }
}
