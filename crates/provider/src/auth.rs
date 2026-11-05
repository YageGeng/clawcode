//! Upstream authentication assembly; credential format and refresh remain upstream-owned.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use protocol::{DeviceCode, DeviceCodeSink, LoginMethod};
use rig_core::http_client::DynHttpClient;
use rig_core::providers::chatgpt::auth::{
    AuthContext, AuthError, AuthSource, Authenticator, DeviceCodeHandler,
    FileTokenStore,
};
use tokio::sync::Notify;

use crate::factory::ProviderConnectError;

/// Upstream's only signal that a credential needs a new sign-in is this message.
///
/// A prefix rather than a phrase: refresh failures embed server-provided text.
const SIGN_IN_REQUIRED_PREFIX: &str = "ChatGPT sign-in required";
/// Longest a readiness check holds a model change or prompt back.
const SIGN_IN_CHECK_LIMIT: Duration = Duration::from_secs(10);
/// Longest a sign-in waits for upstream to issue a device code.
const DEVICE_CODE_LIMIT: Duration = Duration::from_secs(30);

/// One provider's sign-in state, shared by all of its models.
pub struct ProviderAuth {
    /// Never starts a device flow by itself, so requests cannot wait on a login.
    authenticator: Authenticator,
    method: LoginMethod,
    /// The model endpoint rejected a credential that still looks unexpired.
    invalidated: AtomicBool,
}

impl ProviderAuth {
    /// Binds an upstream store to the identity clients use to name this sign-in.
    pub(crate) fn new(
        provider: &config::LlmProvider,
        config: &config::ProviderAuthConfig,
    ) -> Self {
        let config::ProviderAuthConfig::Chatgpt { auth_dir } = config;
        Self {
            authenticator: Authenticator::new(
                AuthSource::OAuth,
                FileTokenStore::new(auth_dir),
                DeviceCodeHandler::default(),
                false,
            ),
            method: LoginMethod {
                id: provider.id.as_str().to_owned(),
                name: provider.display_name.clone(),
            },
            invalidated: AtomicBool::new(false),
        }
    }

    /// Returns the method clients use to sign in to this provider.
    pub(crate) fn method(&self) -> &LoginMethod {
        &self.method
    }

    /// Resolves the credential, refreshing it if needed but never prompting.
    pub(crate) async fn context(
        &self,
        http: &DynHttpClient,
    ) -> Result<AuthContext, ProviderConnectError> {
        if self.invalidated.load(Ordering::Acquire) {
            return Err(self.sign_in_required());
        }
        self.authenticator.auth_context(http).await.map_err(
            |error| match &error {
                AuthError::Message(message)
                    if message.starts_with(SIGN_IN_REQUIRED_PREFIX) =>
                {
                    self.sign_in_required()
                }
                _ => ProviderConnectError::Authentication(Box::new(error)),
            },
        )
    }

    /// Reports only a certain need to sign in; anything else surfaces on the request.
    ///
    /// The check runs in its own task, and the limit abandons the wait rather than
    /// the task: cancelling a refresh in flight could lose a rotated refresh token.
    pub(crate) async fn check(
        self: Arc<Self>,
        http: DynHttpClient,
    ) -> Result<(), ProviderConnectError> {
        let id = self.method.id.clone();
        let task = tokio::spawn(async move { self.context(&http).await });
        let undecided = match tokio::time::timeout(SIGN_IN_CHECK_LIMIT, task)
            .await
        {
            Ok(Ok(Ok(_context))) => return Ok(()),
            Ok(Ok(Err(
                error @ ProviderConnectError::SignInRequired { .. },
            ))) => return Err(error),
            Ok(Ok(Err(error))) => error.to_string(),
            Ok(Err(error)) => error.to_string(),
            Err(_elapsed) => {
                format!("no answer within {} s", SIGN_IN_CHECK_LIMIT.as_secs())
            }
        };
        tracing::warn!(
            "sign-in check for provider {} did not decide: {}",
            id,
            undecided
        );
        Ok(())
    }

    /// Runs one device sign-in, handing the code to `on_code` once upstream issues it.
    ///
    /// Until the code exists no client can show a way to cancel, so that wait is
    /// bounded; afterwards the user decides how long to take.
    pub(crate) async fn sign_in(
        &self,
        http: &DynHttpClient,
        on_code: DeviceCodeSink,
    ) -> Result<(), ProviderConnectError> {
        let id = &self.method.id;
        tracing::info!("started sign-in for provider {}", id);
        let issued = Arc::new(Notify::new());
        let handler = {
            let issued = Arc::clone(&issued);
            let id = id.clone();
            DeviceCodeHandler::new(move |prompt| {
                tracing::info!("received a device code for provider {}", id);
                issued.notify_one();
                on_code(DeviceCode {
                    verification_uri: prompt.verification_uri,
                    user_code: prompt.user_code,
                });
            })
        };
        let authenticator =
            self.authenticator.clone().with_device_code_handler(handler);
        let waiting_for_code = async {
            if tokio::time::timeout(DEVICE_CODE_LIMIT, issued.notified())
                .await
                .is_ok()
            {
                std::future::pending::<()>().await;
            }
        };
        let result = tokio::select! {
            result = authenticator.sign_in_with_device_code(http) => result
                .map(drop)
                .map_err(|error| ProviderConnectError::Authentication(Box::new(error))),
            () = waiting_for_code => Err(ProviderConnectError::SignInFailed(
                "timed out requesting a device code",
            )),
        };
        match result {
            Ok(()) => {
                self.invalidated.store(false, Ordering::Release);
                tracing::info!("completed sign-in for provider {}", id);
                Ok(())
            }
            Err(error) => {
                tracing::warn!("sign-in for provider {} failed: {}", id, error);
                Err(error)
            }
        }
    }

    /// Marks the credential unusable once the model endpoint answers 401.
    ///
    /// 403 is left alone because it can mean the plan lacks the model.
    pub(crate) fn note_failure_status(&self, status: Option<u16>) {
        if status == Some(401) && !self.invalidated.swap(true, Ordering::AcqRel)
        {
            tracing::warn!(
                "provider {} answered 401; its credential needs a new sign-in",
                self.method.id
            );
        }
    }

    /// Builds the typed failure naming this provider's method.
    fn sign_in_required(&self) -> ProviderConnectError {
        ProviderConnectError::SignInRequired {
            method_id: self.method.id.clone(),
        }
    }
}
