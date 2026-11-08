//! ACP sign-in: advertised methods, `auth_required` mapping, and `auth/login`.

use std::io;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acp::{AcpServerFactory, AcpTransportKind};
use agent_client_protocol::{
    Agent, Client, ErrorCode,
    v2::{ConnectionTo as V2ConnectionTo, Responder},
};
use agent_client_protocol::{schema::ProtocolVersion, v2::schema as wire};
use async_trait::async_trait;
use extension::StaticExtensionFactory;
use kernel::{
    Kernel, KernelFactory, Model, ModelCatalog, ModelError, ModelFactory,
    ModelStream, NanoidIdGenerator,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    DeviceCode, DeviceCodeSink, LoginMethod, ModelProfile, ModelRequest,
    SessionId, StaticExtensionRegistration,
};
use store::{JsonlStoreFactory, SystemClock};
use tokio::sync::{Notify, Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

/// Bounds every wait so a regression fails instead of hanging.
const WAIT: Duration = Duration::from_secs(5);
/// Gives work that must not happen time to happen anyway.
const SETTLE: Duration = Duration::from_millis(100);
const VERIFICATION_URI: &str = "https://auth.example.test/device";
const LOGIN_MODEL: &str = "chatgpt/astra";

type Asked = (
    wire::CreateElicitationRequest,
    Responder<wire::CreateElicitationResponse>,
);

/// Builds one small model profile.
fn profile(provider: &str, model: &str) -> ModelProfile {
    ModelProfile::builder()
        .provider_id(provider.to_owned())
        .model_id(model.to_owned())
        .display_name(model.to_owned())
        .context_tokens(64_000)
        .max_output_tokens(1_024)
        .build()
}

/// Counts one running sign-in, including when its future is dropped.
struct LiveSignIn(Arc<AtomicUsize>);

impl LiveSignIn {
    /// Marks a sign-in as running.
    fn enter(live: &Arc<AtomicUsize>) -> Self {
        live.fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(live))
    }
}

impl Drop for LiveSignIn {
    /// Marks the sign-in finished however it ended.
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A model whose sign-in the test scripts and releases.
struct LoginModel {
    profile: ModelProfile,
    signed_in: AtomicBool,
    /// Holds a sign-in before its device code exists, when set.
    before_code: Option<Arc<Notify>>,
    /// Fails a sign-in before any device code exists.
    fails_before_code: AtomicBool,
    /// One permit completes one waiting sign-in.
    release: Semaphore,
    live: Arc<AtomicUsize>,
    codes_issued: AtomicUsize,
}

impl LoginModel {
    /// Builds a signed-out model.
    fn new(before_code: Option<Arc<Notify>>) -> Arc<Self> {
        Arc::new(Self {
            profile: profile("chatgpt", "astra"),
            signed_in: AtomicBool::new(false),
            before_code,
            fails_before_code: AtomicBool::new(false),
            release: Semaphore::new(0),
            live: Arc::default(),
            codes_issued: AtomicUsize::new(0),
        })
    }

    /// Returns how many sign-ins are still running.
    fn live_sign_ins(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// Waits until no sign-in is running, failing after `WAIT`.
    async fn wait_until_idle(&self) {
        tokio::time::timeout(WAIT, async {
            while self.live_sign_ins() > 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("sign-in stopped");
    }
}

#[async_trait]
impl Model for LoginModel {
    /// Returns the deterministic identity.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Sign-in state is reported separately from readiness.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Offers the sign-in under the provider's identifier.
    fn login_method(&self) -> Option<LoginMethod> {
        Some(LoginMethod {
            id: "chatgpt".to_owned(),
            name: "ChatGPT".to_owned(),
        })
    }

    /// Requires a sign-in until one completes.
    async fn ensure_signed_in(&self) -> Result<(), ModelError> {
        if self.signed_in.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(ModelError::AuthRequired {
                method_id: "chatgpt".to_owned(),
            })
        }
    }

    /// Issues a numbered code, then waits for the test; dropping the future stops it.
    async fn sign_in(&self, on_code: DeviceCodeSink) -> Result<(), ModelError> {
        let _live = LiveSignIn::enter(&self.live);
        if self.fails_before_code.load(Ordering::SeqCst) {
            return Err(ModelError::Unavailable(
                "PRIVATE_UPSTREAM_DETAIL".to_owned(),
            ));
        }
        if let Some(before_code) = &self.before_code {
            before_code.notified().await;
        }
        let number = self.codes_issued.fetch_add(1, Ordering::SeqCst) + 1;
        on_code(DeviceCode {
            verification_uri: VERIFICATION_URI.to_owned(),
            user_code: format!("CODE-{number}"),
        });
        self.release
            .acquire()
            .await
            .expect("release stays open")
            .forget();
        self.signed_in.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// These tests never reach the model.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        Err(ModelError::Unavailable("not streamed".to_owned()))
    }
}

/// A model with no sign-in at all.
struct PlainModel(ModelProfile);

#[async_trait]
impl Model for PlainModel {
    /// Returns the deterministic identity.
    fn profile(&self) -> &ModelProfile {
        &self.0
    }

    /// Always ready.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// These tests never reach the model.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        Err(ModelError::Unavailable("not streamed".to_owned()))
    }
}

/// Offers `plain/open`, plus the sign-in model when there is one.
struct LoginModelFactory {
    login: Option<Arc<LoginModel>>,
    starts_on_login: bool,
}

impl ModelFactory for LoginModelFactory {
    /// Returns the model the catalog activates.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::new(PlainModel(profile("plain", "open"))))
    }

    /// Builds the two-model catalog.
    fn create_catalog(
        &self,
        _registration: &StaticExtensionRegistration,
    ) -> Result<ModelCatalog, ModelError> {
        let mut models = vec![self.create()?];
        if let Some(login) = &self.login {
            models.push(Arc::clone(login) as Arc<dyn Model>);
        }
        if self.starts_on_login {
            ModelCatalog::new("chatgpt", "astra", models)
        } else {
            ModelCatalog::new("plain", "open", models)
        }
    }
}

/// Builds a production Kernel and ACP server around scripted models.
fn server(
    root: &Path,
    factory: LoginModelFactory,
) -> (Arc<Kernel>, Arc<AcpServerFactory>) {
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    let kernel = Arc::new(
        KernelFactory::builder()
            .model_factory(Arc::new(factory))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                root,
                Arc::clone(&clock),
            )))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                root.join("config"),
                protocol::PromptPolicy::default(),
            )))
            .extension_factory(Arc::new(StaticExtensionFactory::default()))
            .clock(clock)
            .id_generator(Arc::new(NanoidIdGenerator))
            .build()
            .build()
            .expect("login kernel"),
    );
    let acp = Arc::new(AcpServerFactory::new(
        Arc::clone(&kernel),
        NonZeroUsize::new(128).expect("positive batch size"),
    ));
    (kernel, acp)
}

/// What a test client observed from the agent.
struct ClientSide {
    initialized: wire::InitializeResponse,
    asked: mpsc::UnboundedReceiver<Asked>,
    completed: mpsc::UnboundedReceiver<wire::CompleteElicitationNotification>,
}

/// Runs `body` on an initialized client, with or without URL elicitation.
async fn with_client<B>(factory: &Arc<AcpServerFactory>, url: bool, body: B)
where
    B: AsyncFnOnce(
        &V2ConnectionTo<Agent>,
        &mut ClientSide,
    ) -> Result<(), agent_client_protocol::Error>,
{
    let component = factory.component(AcpTransportKind::Stdio);
    let (asked_tx, asked) = mpsc::unbounded_channel();
    let (completed_tx, completed) = mpsc::unbounded_channel();
    Client
        .v2()
        .on_receive_request(
            async move |request: wire::CreateElicitationRequest,
                        responder: Responder<
                wire::CreateElicitationResponse,
            >,
                        _connection| {
                // Hand the request to the test body; answering here would stall dispatch.
                let _ignored = asked_tx.send((request, responder));
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: wire::CompleteElicitationNotification,
                        _connection| {
                let _ignored = completed_tx.send(notification);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(component, async move |connection| {
            let capabilities = url.then(|| {
                wire::ElicitationCapabilities::new()
                    .url(wire::ElicitationUrlCapabilities::new())
            });
            let initialized = connection
                .send_request(
                    wire::InitializeRequest::new(
                        ProtocolVersion::V2,
                        wire::Implementation::new("login-test", "0.1.0"),
                    )
                    .capabilities(
                        wire::ClientCapabilities::new()
                            .elicitation(capabilities),
                    ),
                )
                .block_task()
                .await?;
            let mut side = ClientSide {
                initialized,
                asked,
                completed,
            };
            body(&connection, &mut side).await
        })
        .await
        .expect("ACP connection");
}

/// Asks to switch a Session to the sign-in model.
fn switch_to_login_model(
    session_id: &wire::SessionId,
) -> wire::SetSessionConfigOptionRequest {
    wire::SetSessionConfigOptionRequest::new(
        session_id.clone(),
        "model".to_owned(),
        wire::SessionConfigOptionValue::Id {
            value: LOGIN_MODEL.to_owned().into(),
        },
    )
}

/// Asserts the standard `auth_required` error naming the ChatGPT method.
fn assert_auth_required(error: &agent_client_protocol::Error) {
    assert_eq!(error.code, ErrorCode::new(-32000), "{error:?}");
    assert_eq!(
        error.data,
        Some(serde_json::json!({ "methodId": "chatgpt" })),
        "{error:?}"
    );
}

/// A provider with a sign-in is offered as one agent-handled method.
#[tokio::test]
async fn initialize_advertises_login_methods() {
    let root = tempfile::tempdir().expect("root");
    let (_kernel, acp) = server(
        root.path(),
        LoginModelFactory {
            login: Some(LoginModel::new(None)),
            starts_on_login: false,
        },
    );
    with_client(&acp, true, async |_connection, side| {
        let [wire::AuthMethod::Agent(method)] =
            side.initialized.auth_methods.as_slice()
        else {
            return Err(agent_client_protocol::Error::internal_error()
                .data(format!("{:?}", side.initialized.auth_methods)));
        };
        assert_eq!(method.method_id.to_string(), "chatgpt");
        assert_eq!(method.name, "ChatGPT");
        Ok(())
    })
    .await;
}

/// Without a sign-in the list stays empty, so clients must not call `auth/login`.
#[tokio::test]
async fn initialize_omits_login_methods_without_oauth() {
    let root = tempfile::tempdir().expect("root");
    let (_kernel, acp) = server(
        root.path(),
        LoginModelFactory {
            login: None,
            starts_on_login: false,
        },
    );
    with_client(&acp, true, async |_connection, side| {
        assert!(side.initialized.auth_methods.is_empty());
        Ok(())
    })
    .await;
}

/// A Session opens signed out, otherwise the user could not switch to another model.
#[tokio::test]
async fn session_opens_without_sign_in() {
    let root = tempfile::tempdir().expect("root");
    let (_kernel, acp) = server(
        root.path(),
        LoginModelFactory {
            login: Some(LoginModel::new(None)),
            starts_on_login: true,
        },
    );
    let workspace = root.path().to_path_buf();
    with_client(&acp, true, async move |connection, _side| {
        connection
            .send_request(wire::NewSessionRequest::new(workspace))
            .block_task()
            .await?;
        Ok(())
    })
    .await;
}

/// A refused switch names the method and leaves the Session on its old model.
#[tokio::test]
async fn model_switch_requires_sign_in() {
    let root = tempfile::tempdir().expect("root");
    let (kernel, acp) = server(
        root.path(),
        LoginModelFactory {
            login: Some(LoginModel::new(None)),
            starts_on_login: false,
        },
    );
    let workspace = root.path().to_path_buf();
    with_client(&acp, true, async move |connection, _side| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(workspace))
            .block_task()
            .await?;
        let error = connection
            .send_request(switch_to_login_model(&created.session_id))
            .block_task()
            .await
            .expect_err("sign-in required");
        assert_auth_required(&error);
        let session = SessionId::try_from(created.session_id.to_string())
            .expect("session id");
        let settings = kernel.session_settings(&session).expect("settings");
        assert_eq!(settings.model.provider_id, "plain");
        Ok(())
    })
    .await;
}

/// A refused prompt names the method, enters no history, and starts no Run.
#[tokio::test]
async fn prompt_requires_sign_in() {
    let root = tempfile::tempdir().expect("root");
    let (kernel, acp) = server(
        root.path(),
        LoginModelFactory {
            login: Some(LoginModel::new(None)),
            starts_on_login: true,
        },
    );
    let workspace = root.path().to_path_buf();
    with_client(&acp, true, async move |connection, _side| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(workspace))
            .block_task()
            .await?;
        let error = connection
            .send_request(wire::PromptRequest::new(
                created.session_id.clone(),
                vec![wire::ContentBlock::Text(wire::TextContent::new("hello"))],
            ))
            .block_task()
            .await
            .expect_err("sign-in required");
        assert_auth_required(&error);
        let session = SessionId::try_from(created.session_id.to_string())
            .expect("session id");
        assert!(
            kernel
                .session_transcript(&session)
                .expect("history")
                .is_empty()
        );
        tokio::time::sleep(SETTLE).await;
        assert!(!kernel.session_runtime(&session).expect("runtime").running);
        Ok(())
    })
    .await;
}

/// Builds a server that starts on `plain/open` and offers `model`'s sign-in.
fn login_server(
    root: &Path,
    model: &Arc<LoginModel>,
) -> (Arc<Kernel>, Arc<AcpServerFactory>) {
    server(
        root,
        LoginModelFactory {
            login: Some(Arc::clone(model)),
            starts_on_login: false,
        },
    )
}

/// Waits for the next `elicitation/create`, failing after `WAIT`.
async fn next_asked(side: &mut ClientSide) -> Asked {
    tokio::time::timeout(WAIT, side.asked.recv())
        .await
        .expect("elicitation arrives")
        .expect("client stays open")
}

/// Asserts the agent has shown no device code so far.
fn assert_not_asked(side: &mut ClientSide) {
    if let Ok((request, _responder)) = side.asked.try_recv() {
        unreachable!("unexpected elicitation: {request:?}");
    }
}

/// Waits until the agent withdraws one `elicitation/create`.
async fn assert_withdrawn(
    responder: &Responder<wire::CreateElicitationResponse>,
) {
    let withdrawn = responder.cancellation();
    tokio::time::timeout(WAIT, withdrawn.cancelled())
        .await
        .expect("elicitation withdrawn");
}

/// Returns the URL mode the agent must use for a device code.
fn url_mode(
    request: &wire::CreateElicitationRequest,
) -> &wire::ElicitationUrlMode {
    match &request.mode {
        wire::ElicitationMode::Url(mode) => mode,
        other => unreachable!("device codes use URL mode, got {other:?}"),
    }
}

/// Returns the `auth/login` request.
fn login_request() -> wire::LoginAuthRequest {
    wire::LoginAuthRequest::new("chatgpt")
}

/// Asserts the standard cancellation error.
fn assert_cancelled(error: &agent_client_protocol::Error) {
    assert_eq!(error.code, ErrorCode::new(-32800), "{error:?}");
}

/// A sign-in shows its code on its own request, then withdraws it and unlocks the switch.
#[tokio::test]
async fn login_elicits_with_request_scope_then_withdraws() {
    let root = tempfile::tempdir().expect("root");
    let model = LoginModel::new(None);
    let (_kernel, acp) = login_server(root.path(), &model);
    let workspace = root.path().to_path_buf();
    with_client(&acp, true, async |connection, side| {
        let created = connection
            .send_request(wire::NewSessionRequest::new(workspace))
            .block_task()
            .await?;
        let login = connection.send_request(login_request());
        let login_id = login.id().clone();
        let (request, responder) = next_asked(side).await;
        let mode = url_mode(&request);
        assert_eq!(mode.url, VERIFICATION_URI);
        assert_eq!(
            mode.scope,
            wire::ElicitationScope::Request(
                wire::ElicitationRequestScope::new(login_id)
            )
        );
        assert!(request.message.contains("CODE-1"), "{}", request.message);
        let product = request
            .meta
            .as_ref()
            .and_then(|meta| meta.get(protocol::ProductIdentity::ACP_NAMESPACE))
            .expect("product metadata");
        assert_eq!(product["userCode"], "CODE-1");
        assert_eq!(product["methodName"], "ChatGPT");
        assert!(
            !mode.elicitation_id.to_string().contains('"'),
            "{}",
            mode.elicitation_id
        );

        model.release.add_permits(1);
        assert_withdrawn(&responder).await;
        login.block_task().await?;
        // The sign-in is what the switch was waiting for.
        connection
            .send_request(switch_to_login_model(&created.session_id))
            .block_task()
            .await?;
        Ok(())
    })
    .await;
}

/// Refusing the code stops the sign-in and leaves the user signed out.
#[tokio::test]
async fn declined_login_reports_cancelled() {
    for action in [
        wire::ElicitationAction::Decline,
        wire::ElicitationAction::Cancel,
    ] {
        let root = tempfile::tempdir().expect("root");
        let model = LoginModel::new(None);
        let (_kernel, acp) = login_server(root.path(), &model);
        let workspace = root.path().to_path_buf();
        with_client(&acp, true, async |connection, side| {
            let created = connection
                .send_request(wire::NewSessionRequest::new(workspace))
                .block_task()
                .await?;
            let login = connection.send_request(login_request());
            let (_request, responder) = next_asked(side).await;
            responder.respond(wire::CreateElicitationResponse::new(action))?;
            assert_cancelled(&login.block_task().await.expect_err("cancelled"));
            model.wait_until_idle().await;
            let error = connection
                .send_request(switch_to_login_model(&created.session_id))
                .block_task()
                .await
                .expect_err("still signed out");
            assert_auth_required(&error);
            Ok(())
        })
        .await;
    }
}

/// Accepting only opens the page, so the sign-in continues and later reports completion.
#[tokio::test]
async fn accepted_elicitation_keeps_waiting_and_completes() {
    let root = tempfile::tempdir().expect("root");
    let model = LoginModel::new(None);
    let (_kernel, acp) = login_server(root.path(), &model);
    with_client(&acp, true, async |connection, side| {
        let login = connection.send_request(login_request());
        let (request, responder) = next_asked(side).await;
        let elicitation_id = url_mode(&request).elicitation_id.clone();
        responder.respond(wire::CreateElicitationResponse::new(
            wire::ElicitationAction::Accept(
                wire::ElicitationAcceptAction::new(),
            ),
        ))?;
        // Accepting only means the page was opened, not that the user finished there.
        tokio::time::sleep(SETTLE).await;
        assert_eq!(model.live_sign_ins(), 1);

        model.release.add_permits(1);
        let completed = tokio::time::timeout(WAIT, side.completed.recv())
            .await
            .expect("completion arrives")
            .expect("client stays open");
        assert_eq!(completed.elicitation_id, elicitation_id);
        login.block_task().await?;
        Ok(())
    })
    .await;
}

/// A client cancel stops the sign-in and withdraws its code.
#[tokio::test]
async fn cancel_request_stops_login() {
    let root = tempfile::tempdir().expect("root");
    let model = LoginModel::new(None);
    let (_kernel, acp) = login_server(root.path(), &model);
    with_client(&acp, true, async |connection, side| {
        let login = connection.send_request(login_request());
        let handle = login.cancellation_handle();
        let (_request, responder) = next_asked(side).await;
        handle.cancel()?;
        assert_cancelled(&login.block_task().await.expect_err("cancelled"));
        assert_withdrawn(&responder).await;
        model.wait_until_idle().await;
        Ok(())
    })
    .await;
}

/// Closing the connection stops the sign-in without any cancel message.
#[tokio::test]
async fn disconnect_stops_login() {
    let root = tempfile::tempdir().expect("root");
    let model = LoginModel::new(None);
    let (_kernel, acp) = login_server(root.path(), &model);
    with_client(&acp, true, async |connection, side| {
        let login = connection.send_request(login_request());
        let _asked = next_asked(side).await;
        assert_eq!(model.live_sign_ins(), 1);
        // Dropping the request could send `$/cancel_request`; only the close may stop this.
        let _leaked = Box::leak(Box::new(login));
        Ok(())
    })
    .await;
    model.wait_until_idle().await;
}

/// An unadvertised method is rejected and starts nothing.
#[tokio::test]
async fn unknown_method_is_invalid_params() {
    let root = tempfile::tempdir().expect("root");
    let model = LoginModel::new(None);
    let (_kernel, acp) = login_server(root.path(), &model);
    with_client(&acp, true, async |connection, _side| {
        let error = connection
            .send_request(wire::LoginAuthRequest::new("missing"))
            .block_task()
            .await
            .expect_err("unknown method");
        assert_eq!(error.code, ErrorCode::new(-32602), "{error:?}");
        Ok(())
    })
    .await;
    assert_eq!(model.codes_issued.load(Ordering::SeqCst), 0);
}

/// A client without URL mode gets no elicitation and can still finish.
#[tokio::test]
async fn client_without_url_elicitation_still_signs_in() {
    let root = tempfile::tempdir().expect("root");
    let model = LoginModel::new(None);
    let (_kernel, acp) = login_server(root.path(), &model);
    with_client(&acp, false, async |connection, side| {
        let login = connection.send_request(login_request());
        tokio::time::timeout(WAIT, async {
            while model.codes_issued.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("code issued");
        tokio::time::sleep(SETTLE).await;
        assert_not_asked(side);
        model.release.add_permits(1);
        login.block_task().await?;
        Ok(())
    })
    .await;
}

/// A cancel works even before a code exists, and then nothing is shown.
#[tokio::test]
async fn cancel_before_device_code_stops_login() {
    let root = tempfile::tempdir().expect("root");
    let model = LoginModel::new(Some(Arc::new(Notify::new())));
    let (_kernel, acp) = login_server(root.path(), &model);
    with_client(&acp, true, async |connection, side| {
        let login = connection.send_request(login_request());
        let handle = login.cancellation_handle();
        tokio::time::timeout(WAIT, async {
            while model.live_sign_ins() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("sign-in started");
        handle.cancel()?;
        assert_cancelled(&login.block_task().await.expect_err("cancelled"));
        model.wait_until_idle().await;
        assert_not_asked(side);
        Ok(())
    })
    .await;
}

/// A failed sign-in reports a fixed error that keeps upstream text from the client.
#[tokio::test]
async fn failed_sign_in_reports_an_error_without_elicitation() {
    let root = tempfile::tempdir().expect("root");
    let model = LoginModel::new(None);
    model.fails_before_code.store(true, Ordering::SeqCst);
    let (_kernel, acp) = login_server(root.path(), &model);
    with_client(&acp, true, async |connection, side| {
        let error = connection
            .send_request(login_request())
            .block_task()
            .await
            .expect_err("failed sign-in");
        assert_eq!(error.code, ErrorCode::new(-32603), "{error:?}");
        assert!(
            !format!("{error:?}").contains("PRIVATE_UPSTREAM_DETAIL"),
            "{error:?}"
        );
        assert_not_asked(side);
        Ok(())
    })
    .await;
}

/// Two sign-ins keep their own codes and scopes, and cancelling one spares the other.
#[tokio::test]
async fn concurrent_logins_keep_their_own_elicitation() {
    let root = tempfile::tempdir().expect("root");
    let model = LoginModel::new(None);
    let (_kernel, acp) = login_server(root.path(), &model);
    with_client(&acp, true, async |connection, side| {
        let first = connection.send_request(login_request());
        let first_id = first.id().clone();
        let first_handle = first.cancellation_handle();
        let (first_asked, first_responder) = next_asked(side).await;
        let second = connection.send_request(login_request());
        let second_id = second.id().clone();
        let (second_asked, second_responder) = next_asked(side).await;

        assert_ne!(first_id, second_id);
        assert_eq!(
            url_mode(&first_asked).scope,
            wire::ElicitationScope::Request(
                wire::ElicitationRequestScope::new(first_id)
            )
        );
        assert_eq!(
            url_mode(&second_asked).scope,
            wire::ElicitationScope::Request(
                wire::ElicitationRequestScope::new(second_id)
            )
        );
        assert!(first_asked.message.contains("CODE-1"));
        assert!(second_asked.message.contains("CODE-2"));

        first_handle.cancel()?;
        assert_cancelled(&first.block_task().await.expect_err("cancelled"));
        assert_withdrawn(&first_responder).await;
        tokio::time::sleep(SETTLE).await;
        assert!(!second_responder.cancellation().is_cancelled());
        assert_eq!(model.live_sign_ins(), 1);

        model.release.add_permits(1);
        second.block_task().await?;
        Ok(())
    })
    .await;
}

/// Collects formatted log output for one test.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    /// Returns everything logged so far.
    fn content(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("logs")).into_owned()
    }
}

impl io::Write for CapturedLogs {
    /// Appends one formatted line.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("logs").extend_from_slice(buffer);
        Ok(buffer.len())
    }

    /// Nothing is buffered here.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedLogs {
    type Writer = Self;

    /// Shares the one buffer.
    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

/// The code stays out of the logs even at debug level.
#[tokio::test]
async fn login_logs_omit_device_code() {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::new("debug"))
        .with_writer(logs.clone())
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let root = tempfile::tempdir().expect("root");
    let model = LoginModel::new(None);
    let (_kernel, acp) = login_server(root.path(), &model);
    with_client(&acp, true, async |connection, side| {
        let login = connection.send_request(login_request());
        let _asked = next_asked(side).await;
        model.release.add_permits(1);
        login.block_task().await?;
        Ok(())
    })
    .await;
    let output = logs.content();
    assert!(output.contains("auth/login"), "{output}");
    assert!(!output.contains("CODE-1"), "{output}");
}
