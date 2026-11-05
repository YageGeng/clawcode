//! `auth/login`: runs one provider sign-in and shows its device code to the client.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use agent_client_protocol::v2::schema as wire;
use agent_client_protocol::{
    Client, Error, RequestCancellation, RequestCancellationHandle,
    v2::{ConnectionTo as V2ConnectionTo, Responder},
};
use kernel::{Kernel, KernelError};
use protocol::{DeviceCode, DeviceCodeSink, LoginMethod, ProductIdentity};
use tokio::sync::mpsc;

/// How one sign-in ended.
enum LoginOutcome {
    SignedIn,
    /// The user or client gave up; the sign-in was dropped.
    Cancelled,
    Failed(KernelError),
}

/// A device-code elicitation the client has not answered yet.
struct PendingElicitation {
    answer: Pin<
        Box<
            dyn Future<Output = Result<wire::CreateElicitationResponse, Error>>
                + Send,
        >,
    >,
    withdraw: RequestCancellationHandle,
    /// Kept so the code still reaches the user if the client cannot show it.
    code: DeviceCode,
}

/// The device-code elicitation while the sign-in runs.
#[derive(Default)]
struct DevicePrompt {
    unanswered: Option<PendingElicitation>,
    /// Set once the client accepted, which only means it opened the page.
    accepted: Option<wire::ElicitationId>,
}

/// One `auth/login` request from a client.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct LoginTask {
    kernel: Arc<Kernel>,
    connection: V2ConnectionTo<Client>,
    url_elicitation: bool,
    method: LoginMethod,
    /// The `auth/login` request the elicitation is scoped to.
    request_id: wire::RequestId,
}

impl LoginTask {
    /// Runs the sign-in to its end, settles the elicitation, then answers the request.
    ///
    /// The responder stays out of `self` because it is not `Sync`, and `drive` borrows
    /// `self` across its awaits.
    pub(crate) async fn run(
        self,
        responder: Responder<wire::LoginAuthResponse>,
    ) -> Result<(), Error> {
        tracing::info!("started auth/login for method {}", self.method.id);
        let mut prompt = DevicePrompt::default();
        let outcome = self.drive(responder.cancellation(), &mut prompt).await;
        // The sign-in future is gone by now, so nothing can still be polling upstream.
        if let Some(pending) = prompt.unanswered
            && let Err(error) = pending.withdraw.cancel()
        {
            tracing::warn!(
                "failed to withdraw the device code for method {}: {}",
                self.method.id,
                error
            );
        }
        if let Some(elicitation_id) = prompt.accepted
            && let Err(error) = self.connection.send_notification(
                wire::CompleteElicitationNotification::new(elicitation_id),
            )
        {
            tracing::warn!(
                "failed to complete the device code for method {}: {}",
                self.method.id,
                error
            );
        }
        let response = match outcome {
            LoginOutcome::SignedIn => {
                tracing::info!(
                    "completed auth/login for method {}",
                    self.method.id
                );
                Ok(wire::LoginAuthResponse::new())
            }
            LoginOutcome::Cancelled => {
                tracing::info!(
                    "cancelled auth/login for method {}",
                    self.method.id
                );
                Err(Error::request_cancelled())
            }
            LoginOutcome::Failed(error) => {
                // Upstream text can carry response bodies, so it never reaches the client.
                tracing::warn!(
                    "auth/login for method {} failed: {}",
                    self.method.id,
                    error
                );
                Err(Error::internal_error().data("sign-in did not complete"))
            }
        };
        responder.respond_with_result(response)
    }

    /// Waits on the sign-in, the client's cancellation, and the elicitation together.
    ///
    /// Returning drops the sign-in future, which is what stops it.
    async fn drive(
        &self,
        client_cancelled: RequestCancellation,
        prompt: &mut DevicePrompt,
    ) -> LoginOutcome {
        let (code_tx, mut code_rx) = mpsc::unbounded_channel();
        let on_code: DeviceCodeSink = Arc::new(move |code| {
            // Only a finished sign-in closes the receiver, and it wants no more codes.
            let _ignored = code_tx.send(code);
        });
        let sign_in = self.kernel.sign_in(&self.method.id, on_code);
        tokio::pin!(sign_in);
        loop {
            tokio::select! {
                result = &mut sign_in => {
                    return result.map_or_else(
                        LoginOutcome::Failed,
                        |()| LoginOutcome::SignedIn,
                    );
                }
                () = client_cancelled.cancelled() => {
                    return LoginOutcome::Cancelled;
                }
                Some(code) = code_rx.recv() => self.show(code, prompt),
                answer = async {
                    match prompt.unanswered.as_mut() {
                        Some(pending) => (&mut pending.answer).await,
                        None => std::future::pending().await,
                    }
                } => {
                    let Some(pending) = prompt.unanswered.take() else {
                        continue;
                    };
                    match answer {
                        Ok(response) => match response.action {
                            wire::ElicitationAction::Accept(_) => {
                                prompt.accepted = Some(self.elicitation_id());
                            }
                            // An unknown action is a refusal, never a silent wait.
                            _ => return LoginOutcome::Cancelled,
                        },
                        Err(error) => {
                            tracing::warn!(
                                "client could not show the device code for method {}: {}",
                                self.method.id,
                                error
                            );
                            self.print(&pending.code);
                        }
                    }
                }
            }
        }
    }

    /// Hands the code to the client, or to stderr when it cannot follow a URL.
    fn show(&self, code: DeviceCode, prompt: &mut DevicePrompt) {
        if !self.url_elicitation {
            self.print(&code);
            return;
        }
        let sent = self.connection.send_request(self.elicitation(&code));
        prompt.unanswered = Some(PendingElicitation {
            withdraw: sent.cancellation_handle(),
            answer: Box::pin(sent.block_task()),
            code,
        });
    }

    /// Prints the code where an interactive ACP user can see it; stdout carries protocol frames.
    fn print(&self, code: &DeviceCode) {
        // This is an interactive sign-in prompt, not a credential log event.
        eprintln!(
            "{} sign-in: {} (code: {})",
            self.method.name, code.verification_uri, code.user_code
        );
    }

    /// Builds the request scoped to this `auth/login`, which has no Session.
    fn elicitation(&self, code: &DeviceCode) -> wire::CreateElicitationRequest {
        wire::CreateElicitationRequest::new(
            wire::ElicitationUrlMode::new(
                wire::ElicitationRequestScope::new(self.request_id.clone()),
                self.elicitation_id(),
                code.verification_uri.clone(),
            ),
            format!(
                "Open {} and enter code {} to sign in to {}.",
                code.verification_uri, code.user_code, self.method.name
            ),
        )
        .meta(wire::Meta::from_iter([(
            ProductIdentity::ACP_NAMESPACE.to_string(),
            serde_json::json!({
                "userCode": code.user_code,
                "methodName": self.method.name,
            }),
        )]))
    }

    /// Derives the elicitation's identity from the request it belongs to.
    fn elicitation_id(&self) -> wire::ElicitationId {
        match &self.request_id {
            wire::RequestId::Number(id) => format!("login-{id}"),
            wire::RequestId::Str(id) => format!("login-{id}"),
        }
        .into()
    }
}
