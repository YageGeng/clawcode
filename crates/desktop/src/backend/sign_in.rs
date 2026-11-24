//! Provider sign-ins on one connection: the device code a sign-in shows, and
//! the sign-ins in progress, so everyone asking for the same one shares it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use agent_client_protocol::RequestCancellationHandle;
use agent_client_protocol::v2::schema as wire;
use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use protocol::{DeviceCode, ProductIdentity};

use super::{AcpClient, BackendError, SessionId};

/// How one sign-in ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInOutcome {
    /// The provider is usable now.
    SignedIn,
    /// The user gave up.
    Cancelled,
    /// It did not complete; the reason is logged.
    Failed,
}

impl SignInOutcome {
    /// `Ok` once signed in, else why a request that needed it cannot go on.
    pub fn signed_in(self) -> Result<(), BackendError> {
        match self {
            Self::SignedIn => Ok(()),
            Self::Cancelled => Err(BackendError::Cancelled),
            Self::Failed => Err(BackendError::SignInFailed),
        }
    }
}

/// A sign-in's outcome, shared by everyone waiting for it.
pub type SignIn = Shared<BoxFuture<'static, SignInOutcome>>;

/// The device code a sign-in asks the user to enter; `Debug` hides the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInPrompt {
    /// Provider name, as people know it.
    pub provider: String,
    /// The code and the page it goes to.
    pub device: DeviceCode,
}

impl SignInPrompt {
    /// Reads the prompt of a sign-in: a URL elicitation outside any session
    /// that carries a device code. Anything else is not one.
    pub fn of(request: &wire::CreateElicitationRequest) -> Option<Self> {
        let wire::ElicitationMode::Url(mode) = &request.mode else {
            return None;
        };
        if !matches!(mode.scope, wire::ElicitationScope::Request(_)) {
            return None;
        }
        let product =
            request.meta.as_ref()?.get(ProductIdentity::ACP_NAMESPACE)?;
        let field = |name: &str| Some(product.get(name)?.as_str()?.to_owned());
        Some(Self {
            provider: field("methodName")?,
            device: DeviceCode {
                verification_uri: mode.url.clone(),
                user_code: field("userCode")?,
            },
        })
    }
}

impl SignInPrompt {
    /// The page to open in the browser; `None` for an address that is not
    /// `https`, which is only shown.
    pub fn page(&self) -> Option<&str> {
        let address = self.device.verification_uri.as_str();
        let (scheme, _) = address.split_once("://")?;
        scheme.eq_ignore_ascii_case("https").then_some(address)
    }
}

/// One sign-in the agent was asked for.
struct Asked {
    outcome: SignIn,
    cancel: RequestCancellationHandle,
}

/// The last sign-in of each method, which may still be in progress.
#[derive(Clone, Default)]
pub(super) struct SignIns(Arc<Mutex<HashMap<String, Asked>>>);

impl SignIns {
    /// The table; a panic elsewhere never makes it unusable.
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Asked>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl AcpClient {
    /// Signs in through `method`, joining a sign-in already in progress.
    ///
    /// There is no timeout: the user may take minutes to authorize. The
    /// device code arrives as [`super::BackendEvent::SignIn`].
    pub fn sign_in(&self, method: &str) -> SignIn {
        let mut asked = self.sign_ins.lock();
        if let Some(running) = asked
            .get(method)
            .filter(|asked| asked.outcome.peek().is_none())
        {
            return running.outcome.clone();
        }
        let sent = self
            .connection
            .send_request(wire::LoginAuthRequest::new(method.to_owned()));
        let cancel = sent.cancellation_handle();
        let name = method.to_owned();
        let outcome = async move {
            match sent.block_task().await.map_err(BackendError::from) {
                Ok(_) => SignInOutcome::SignedIn,
                Err(BackendError::Cancelled) => SignInOutcome::Cancelled,
                Err(error) => {
                    tracing::warn!(
                        "sign-in through {} did not complete: {}",
                        name,
                        error
                    );
                    SignInOutcome::Failed
                }
            }
        }
        .boxed()
        .shared();
        // Driven here, so it settles whether or not its callers keep waiting.
        self.runtime.spawn(outcome.clone());
        asked.insert(
            method.to_owned(),
            Asked {
                outcome: outcome.clone(),
                cancel,
            },
        );
        outcome
    }

    /// Changes a configuration option; when that needs a sign-in, signs in
    /// and asks once more.
    ///
    /// A sign-in the user gave up is [`BackendError::Cancelled`].
    pub async fn set_config_signing_in(
        &self,
        id: &SessionId,
        config_id: &str,
        value: &str,
    ) -> Result<Vec<wire::SessionConfigOption>, BackendError> {
        match self.set_config_option(id, config_id, value).await {
            Err(BackendError::AuthRequired(method)) => {
                self.sign_in(&method).await.signed_in()?;
                self.set_config_option(id, config_id, value).await
            }
            answered => answered,
        }
    }

    /// Gives up the sign-in through `method`; nothing happens without one.
    pub fn cancel_sign_in(&self, method: &str) {
        if let Some(asked) = self.sign_ins.lock().get(method)
            && let Err(error) = asked.cancel.cancel()
        {
            tracing::warn!("failed to cancel sign-in {}: {}", method, error);
        }
    }
}
