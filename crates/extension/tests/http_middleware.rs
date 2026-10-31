use std::sync::{Arc, Mutex};

use bytes::Bytes;
use extension::{
    DiscardExtensionDiagnostics, ExtensionContext, ExtensionError,
    ExtensionModule, ExtensionRegistrar, ExtensionRuntime,
};
use protocol::{
    ExtensionDescriptor, ExtensionId, ExtensionInvocation, ExtensionSnapshot,
    LaneId, SessionId, SessionTreeSnapshot, ThinkingLevel, TimestampMs,
};
use rig_core::{
    http_client::{
        self, DynHttpClient, HttpClientExt,
        middleware::{Exchange, HttpMiddleware},
    },
    test_utils::RecordingHttpClient,
    wasm_compat::WasmBoxedFuture,
};

#[derive(Clone, Copy)]
enum Mode {
    Pass,
    RejectHeaders,
    RejectResponse,
    RejectRegistration,
}

struct Module {
    id: &'static str,
    events: Arc<Mutex<Vec<String>>>,
    mode: Mode,
}

impl ExtensionModule for Module {
    /// Supplies the owner whose context must reach every middleware invocation.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from(self.id)
                .expect("valid synthetic test state"),
            name: self.id.into(),
            version: "1".into(),
        }
    }

    /// Registers a native middleware factory whose mutable state belongs to a request.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        let events = Arc::clone(&self.events);
        let tag = self.id;
        let mode = self.mode;
        registrar.register_http_middleware(move |context| {
            Probe::builder()
                .context(context)
                .events(Arc::clone(&events))
                .tag(tag)
                .mode(mode)
                .build()
        })?;
        if matches!(mode, Mode::RejectRegistration) {
            return Err(ExtensionError::DuplicateFlag("rollback".into()));
        }
        Ok(())
    }
}

#[derive(typed_builder::TypedBuilder)]
struct Probe {
    context: ExtensionContext,
    events: Arc<Mutex<Vec<String>>>,
    tag: &'static str,
    mode: Mode,
}

impl Probe {
    /// Records only synthetic identities to assert order without retaining request payloads.
    fn record(&self, phase: &str) {
        self.events
            .lock()
            .expect("valid synthetic test state")
            .push(format!(
                "{}:{phase}:{}",
                self.tag, self.context.invocation.session_id
            ));
    }
}

impl HttpMiddleware for Probe {
    /// Full native headers can be replaced and contain duplicate or non-UTF-8 values.
    fn before_request_headers<'a>(
        &'a self,
        exchange: Exchange<'a>,
        headers: &'a mut http::HeaderMap,
    ) -> WasmBoxedFuture<'a, http_client::Result<()>> {
        Box::pin(async move {
            self.record("headers");
            assert_eq!(exchange.method, http::Method::POST);
            assert_eq!(exchange.uri.path(), "/audit");
            if matches!(self.mode, Mode::RejectHeaders) {
                return Err(http_client::Error::Instance(
                    std::io::Error::other("header rejected").into(),
                ));
            }
            headers.append(
                "x-order",
                self.tag.parse().expect("valid synthetic test state"),
            );
            headers.insert(
                "x-session",
                self.context
                    .invocation
                    .session_id
                    .as_str()
                    .parse()
                    .expect("valid synthetic test state"),
            );
            headers.insert(
                "authorization",
                "Bearer middleware-value"
                    .parse()
                    .expect("valid synthetic test state"),
            );
            headers.insert(
                "x-binary",
                http::HeaderValue::from_bytes(&[0x80])
                    .expect("valid synthetic test state"),
            );
            Ok(())
        })
    }

    /// The body phase sees every header mutation and may change arbitrary bytes.
    fn before_request_body<'a>(
        &'a self,
        _: Exchange<'a>,
        headers: &'a http::HeaderMap,
        body: Bytes,
    ) -> WasmBoxedFuture<'a, http_client::Result<Bytes>> {
        Box::pin(async move {
            self.record("body");
            assert_eq!(headers["authorization"], "Bearer middleware-value");
            tokio::task::yield_now().await;
            let mut bytes = body.to_vec();
            bytes.extend_from_slice(self.tag.as_bytes());
            Ok(bytes.into())
        })
    }

    /// Response failures follow the upstream error-priority contract.
    fn after_response<'a>(
        &'a self,
        _: Exchange<'a>,
        _: http::StatusCode,
        _: &'a http::HeaderMap,
    ) -> WasmBoxedFuture<'a, http_client::Result<()>> {
        Box::pin(async move {
            self.record("response");
            if matches!(self.mode, Mode::RejectResponse) {
                return Err(http_client::Error::Instance(
                    std::io::Error::other("response rejected").into(),
                ));
            }
            Ok(())
        })
    }
}

/// Creates a complete session snapshot without a production host or credentials.
fn context(id: &str) -> ExtensionContext {
    let session_id =
        SessionId::try_from(id).expect("valid synthetic test state");
    ExtensionContext::builder()
        .invocation(
            ExtensionInvocation::builder()
                .extension_id(
                    ExtensionId::try_from("host")
                        .expect("valid synthetic test state"),
                )
                .session_id(session_id.clone())
                .timestamp_ms(TimestampMs::from(1))
                .cwd("/workspace".into())
                .build(),
        )
        .snapshot(
            ExtensionSnapshot::builder()
                .thinking_level(ThinkingLevel::Off)
                .models(Vec::new())
                .messages(Vec::new())
                .tree(
                    SessionTreeSnapshot::builder()
                        .session_id(session_id)
                        .lane(
                            LaneId::try_from("main")
                                .expect("valid synthetic test state"),
                        )
                        .entries(Vec::new())
                        .build(),
                )
                .pending_steer(0)
                .pending_follow_up(0)
                .active_tools(Vec::new())
                .all_tools(Vec::new())
                .cancelled(false)
                .build(),
        )
        .build()
}

/// Native phases stay ordered while concurrent requests share only the base transport.
#[tokio::test]
async fn native_middleware_preserves_order_and_isolates_sessions() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut registrar = ExtensionRegistrar::new();
    for id in ["A", "B"] {
        registrar
            .register_module(&Module {
                id,
                events: Arc::clone(&events),
                mode: Mode::Pass,
            })
            .expect("valid synthetic test state");
    }
    let runtime = ExtensionRuntime::new(
        Arc::new(registrar.freeze()),
        Arc::new(DiscardExtensionDiagnostics),
    );
    let capture = RecordingHttpClient::new("ok");
    let base = DynHttpClient::new(capture.clone());
    let first = runtime
        .provider_middleware(context("first"))
        .apply(base.clone());
    let second = runtime
        .provider_middleware(context("second"))
        .apply(base.clone());
    assert!(first.ptr_eq(&base) && second.ptr_eq(&base));
    let request = http::Request::post("https://provider.invalid/audit")
        .body(vec![0xff, 0])
        .expect("valid synthetic test state");
    let (a, b) = tokio::join!(
        first.send::<_, Bytes>(request.clone()),
        second.send::<_, Bytes>(request)
    );
    assert!(a.is_ok() && b.is_ok());
    let requests = capture.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(&request.body[..], &[0xff, 0, b'A', b'B']);
        assert_eq!(request.headers.get_all("x-order").iter().count(), 2);
        assert_eq!(request.headers["x-binary"].as_bytes(), &[0x80]);
    }
    for session in ["first", "second"] {
        let actual = events
            .lock()
            .expect("valid synthetic test state")
            .iter()
            .filter(|event| event.ends_with(session))
            .cloned()
            .collect::<Vec<_>>();
        let expected = [
            "A:headers",
            "B:headers",
            "A:body",
            "B:body",
            "A:response",
            "B:response",
        ]
        .map(|event| format!("{event}:{session}"));
        assert_eq!(actual, expected);
    }
}

/// Request rejection prevents sending; rejected HTTP responses keep their own error.
#[tokio::test]
async fn middleware_rejection_preserves_transport_error_priority() {
    for (mode, status) in [
        (Mode::RejectHeaders, http::StatusCode::OK),
        (Mode::RejectResponse, http::StatusCode::OK),
        (Mode::RejectResponse, http::StatusCode::TOO_MANY_REQUESTS),
    ] {
        let mut registrar = ExtensionRegistrar::new();
        registrar
            .register_module(&Module {
                id: "A",
                events: Arc::default(),
                mode,
            })
            .expect("valid synthetic test state");
        let runtime = ExtensionRuntime::new(
            Arc::new(registrar.freeze()),
            Arc::new(DiscardExtensionDiagnostics),
        );
        let capture = if status.is_success() {
            RecordingHttpClient::new("ok")
        } else {
            RecordingHttpClient::with_error_headers(
                status,
                "rate limit",
                http::HeaderMap::new(),
            )
        };
        let http = runtime
            .provider_middleware(context("first"))
            .apply(DynHttpClient::new(capture.clone()));
        let error = http
            .send::<_, Bytes>(
                http::Request::post("https://provider.invalid/audit")
                    .body(Vec::<u8>::new())
                    .expect("valid synthetic test state"),
            )
            .await
            .err()
            .expect("middleware must reject");
        if matches!(mode, Mode::RejectHeaders) {
            assert!(capture.requests().is_empty());
        }
        if !status.is_success() {
            assert_eq!(error.non_success_status(), Some(status));
        }
    }
}

/// A rejected module cannot leak its native factories into later requests.
#[tokio::test]
async fn failed_registration_rolls_back_and_multipart_skips_body_hooks() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut registrar = ExtensionRegistrar::new();
    assert!(
        registrar
            .register_module(&Module {
                id: "bad",
                events: Arc::clone(&events),
                mode: Mode::RejectRegistration
            })
            .is_err()
    );
    registrar
        .register_module(&Module {
            id: "good",
            events: Arc::clone(&events),
            mode: Mode::Pass,
        })
        .expect("valid synthetic test state");
    let runtime = ExtensionRuntime::new(
        Arc::new(registrar.freeze()),
        Arc::new(DiscardExtensionDiagnostics),
    );
    let http = runtime
        .provider_middleware(context("session"))
        .apply(DynHttpClient::new(RecordingHttpClient::new("ok")));
    let form =
        http_client::multipart::MultipartForm::new().text("model", "audit");
    http.send_multipart::<Bytes>(
        http::Request::post("https://provider.invalid/audit")
            .body(form)
            .expect("valid synthetic test state"),
    )
    .await
    .expect("valid synthetic test state");
    assert_eq!(
        *events.lock().expect("valid synthetic test state"),
        ["good:headers:session", "good:response:session"]
    );
}
