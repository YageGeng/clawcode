//! Provider sign-in over the in-process ACP connection.

mod support;

use agent_client_protocol::v2::schema as wire;
use desktop::backend::{BackendError, SignIn, SignInOutcome, SignInPrompt};
use protocol::{DeviceCode, LoginMethod, ProviderSignIn, SignInStatus};
use serde_json::json;
use support::login::{LOGIN_MODEL, LoginModel, VERIFICATION_URI};
use support::{ScriptedBackend, WAIT};

/// Waits for a sign-in to end; fails after `WAIT`.
fn settled(backend: &ScriptedBackend, sign_in: SignIn) -> SignInOutcome {
    backend
        .block_on(async { tokio::time::timeout(WAIT, sign_in).await })
        .expect("sign-in settled")
}

/// The sign-in methods come from `initialize`.
#[test]
fn login_methods_come_from_initialize() {
    let backend = ScriptedBackend::start_with_login(&LoginModel::new());
    assert_eq!(
        backend.client.login_methods(),
        [LoginMethod {
            id: "gate".to_owned(),
            name: "Gate".to_owned(),
        }]
    );
    assert!(ScriptedBackend::start().client.login_methods().is_empty());
}

/// A sign-in shows its code; once the user authorized it, the prompt is
/// withdrawn and the model unlocks.
#[test]
fn a_completed_sign_in_unlocks_the_model() {
    let model = LoginModel::new();
    let mut backend = ScriptedBackend::start_with_login(&model);
    let session = backend.new_session();
    let switch = |backend: &ScriptedBackend| {
        backend.block_on(backend.client.set_config_option(
            &session,
            "model",
            LOGIN_MODEL,
        ))
    };
    assert!(matches!(
        switch(&backend),
        Err(BackendError::AuthRequired(method)) if method == "gate"
    ));

    let outcome = backend.client.sign_in("gate");
    let (id, prompt) = backend.wait_sign_in();
    assert_eq!(
        prompt,
        SignInPrompt {
            provider: "Gate".to_owned(),
            device: DeviceCode {
                verification_uri: VERIFICATION_URI.to_owned(),
                user_code: "CODE-1".to_owned(),
            },
        }
    );
    model.complete();
    assert_eq!(settled(&backend, outcome), SignInOutcome::SignedIn);
    backend.wait_request_cancelled(id);
    switch(&backend).expect("unlocked");
}

/// Dismissing the prompt cancels the sign-in.
#[test]
fn a_dismissed_prompt_cancels_the_sign_in() {
    let mut backend = ScriptedBackend::start_with_login(&LoginModel::new());
    let outcome = backend.client.sign_in("gate");
    let (id, _prompt) = backend.wait_sign_in();
    backend
        .client
        .answer_elicitation(
            id,
            wire::CreateElicitationResponse::new(
                wire::ElicitationAction::Cancel,
            ),
        )
        .expect("dismiss");
    assert_eq!(settled(&backend, outcome), SignInOutcome::Cancelled);
}

/// A sign-in can be given up from the client side too.
#[test]
fn cancel_sign_in_stops_it() {
    let mut backend = ScriptedBackend::start_with_login(&LoginModel::new());
    let outcome = backend.client.sign_in("gate");
    let (id, _prompt) = backend.wait_sign_in();
    backend.client.cancel_sign_in("gate");
    assert_eq!(settled(&backend, outcome), SignInOutcome::Cancelled);
    backend.wait_request_cancelled(id);
}

/// Everyone asking meanwhile shares one sign-in; a later one starts afresh.
#[test]
fn concurrent_sign_ins_share_one_code() {
    let model = LoginModel::new();
    let mut backend = ScriptedBackend::start_with_login(&model);
    let first = backend.client.sign_in("gate");
    let second = backend.client.clone().sign_in("gate");
    let _prompt = backend.wait_sign_in();
    model.complete();
    assert_eq!(settled(&backend, first), SignInOutcome::SignedIn);
    assert_eq!(settled(&backend, second), SignInOutcome::SignedIn);
    assert_eq!(model.codes(), 1);

    model.sign_out();
    let again = backend.client.sign_in("gate");
    let (_, prompt) = backend.wait_sign_in();
    assert_eq!(prompt.device.user_code, "CODE-2");
    model.complete();
    assert_eq!(settled(&backend, again), SignInOutcome::SignedIn);
}

/// A method the agent does not know fails without a prompt.
#[test]
fn an_unknown_method_fails() {
    let backend = ScriptedBackend::start_with_login(&LoginModel::new());
    assert_eq!(
        settled(&backend, backend.client.sign_in("missing")),
        SignInOutcome::Failed
    );
}

/// The status follows the sign-in.
#[test]
fn auth_status_follows_the_sign_in() {
    let model = LoginModel::new();
    let mut backend = ScriptedBackend::start_with_login(&model);
    let status = |backend: &ScriptedBackend| {
        backend
            .block_on(backend.client.auth_status())
            .expect("status")
    };
    let gate = |status| {
        vec![ProviderSignIn {
            method_id: "gate".to_owned(),
            status,
        }]
    };
    assert_eq!(status(&backend), gate(SignInStatus::SignInRequired));
    let outcome = backend.client.sign_in("gate");
    let _prompt = backend.wait_sign_in();
    model.complete();
    settled(&backend, outcome);
    assert_eq!(status(&backend), gate(SignInStatus::SignedIn));
}

/// Only a URL request outside any session that carries a code is a sign-in.
#[test]
fn only_device_code_requests_are_sign_ins() {
    let request = |scope: serde_json::Value, meta: serde_json::Value| {
        let mut request = json!({
            "mode": "url",
            "elicitationId": "e1",
            "url": VERIFICATION_URI,
            "message": "Open the page.",
            "_meta": meta,
        });
        let fields = request.as_object_mut().expect("object");
        fields.extend(scope.as_object().expect("scope").clone());
        serde_json::from_value::<wire::CreateElicitationRequest>(request)
            .expect("elicitation")
    };
    let code =
        json!({ "clawcode": { "userCode": "AB-CD", "methodName": "Gate" } });

    let sign_in =
        SignInPrompt::of(&request(json!({ "requestId": 1 }), code.clone()))
            .expect("sign-in");
    assert_eq!(sign_in.device.user_code, "AB-CD");
    // The code never reaches a log through this value.
    assert!(!format!("{sign_in:?}").contains("AB-CD"));

    assert_eq!(
        SignInPrompt::of(&request(json!({ "requestId": 1 }), json!({}))),
        None
    );
    assert_eq!(
        SignInPrompt::of(&request(json!({ "sessionId": "s1" }), code)),
        None
    );
}
