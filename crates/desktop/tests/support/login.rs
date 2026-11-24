//! A model whose provider needs a sign-in the test completes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use kernel::{Model, ModelError, ModelStream};
use protocol::{
    DeviceCode, DeviceCodeSink, LoginMethod, ModelProfile, ModelRequest,
    ModelStreamEvent, StopReason,
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use typed_builder::TypedBuilder;

/// Model option value that needs the sign-in.
pub const LOGIN_MODEL: &str = "gate/locked";
/// Page the device code is entered on.
pub const VERIFICATION_URI: &str = "https://auth.example.test/device";

/// Offers the `gate` sign-in and refuses requests until it completes.
#[derive(TypedBuilder)]
pub struct LoginModel {
    profile: ModelProfile,
    #[builder(default)]
    signed_in: AtomicBool,
    /// One permit completes one waiting sign-in.
    #[builder(default = Semaphore::new(0))]
    release: Semaphore,
    /// How many device codes were issued.
    #[builder(default)]
    codes: AtomicUsize,
}

impl LoginModel {
    /// A signed-out model.
    pub fn new() -> Arc<Self> {
        let profile = ModelProfile::builder()
            .provider_id("gate".to_owned())
            .model_id("locked".to_owned())
            .display_name("Locked".to_owned())
            .context_tokens(64_000)
            .max_output_tokens(1_024)
            .build();
        Arc::new(Self::builder().profile(profile).build())
    }

    /// Completes the sign-in that waits, or the next one.
    pub fn complete(&self) {
        self.release.add_permits(1);
    }

    /// Forgets the credential.
    pub fn sign_out(&self) {
        self.signed_in.store(false, Ordering::SeqCst);
    }

    /// How many device codes were issued.
    pub fn codes(&self) -> usize {
        self.codes.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Model for LoginModel {
    /// Returns the fixture profile.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Sign-in is reported separately from readiness.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Offers one sign-in under the provider's identifier.
    fn login_method(&self) -> Option<LoginMethod> {
        Some(LoginMethod {
            id: "gate".to_owned(),
            name: "Gate".to_owned(),
        })
    }

    /// Requires a sign-in until one completes.
    async fn ensure_signed_in(&self) -> Result<(), ModelError> {
        if self.signed_in.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(ModelError::AuthRequired {
                method_id: "gate".to_owned(),
            })
        }
    }

    /// Issues a numbered code, then waits for [`LoginModel::complete`].
    async fn sign_in(&self, on_code: DeviceCodeSink) -> Result<(), ModelError> {
        let number = self.codes.fetch_add(1, Ordering::SeqCst) + 1;
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

    /// Answers every request with `unlocked`.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(ModelStreamEvent::TextDelta("unlocked".to_owned())),
            Ok(super::ScriptedModel::finished(StopReason::EndTurn)),
        ])))
    }
}
