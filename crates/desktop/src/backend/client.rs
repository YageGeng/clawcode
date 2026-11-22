//! The ACP client: the connection to the in-process agent and every request
//! the UI sends through it.

use std::path::Path;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v2::{ConnectionTo, Responder, schema as wire};
use agent_client_protocol::{
    Agent, Client, ConnectTo, JsonRpcMessage, JsonRpcRequest, UntypedMessage,
};
use futures::channel::mpsc::UnboundedSender;
use futures::channel::oneshot;
use protocol::{
    AcpExtensionMethod, CheckpointDiffParameters, CheckpointList,
    CheckpointsParameters, ContentHash, EntryId, PendingMessages, QueueId,
    RequestContent, RunId, SessionRuntimeSnapshot, SessionTreeSnapshot,
    TrajectoryCursor, TrajectoryPage, WorkspaceChange, WorkspaceChanges,
    WorkspaceChangesParameters, WorkspaceDiff, WorkspaceDiffParameters,
    WorkspaceDiffScope, WorkspaceDirListing, WorkspaceFileContent,
    WorkspaceGitStatus, WorkspaceListDirParameters, WorkspacePathParameters,
    WorkspaceRootParameters,
};

use super::requests::{PendingRequests, Waiting};
use super::{
    AgentRequest, BackendError, BackendEvent, QuestionKey, RequestId,
    SessionId, SessionStart,
};

/// Handle to the in-process ACP connection; cheap to clone and usable from any executor.
#[derive(Clone)]
pub struct AcpClient {
    connection: ConnectionTo<Agent>,
    runtime: tokio::runtime::Handle,
    pending: PendingRequests,
}

/// Keeps the ACP connection open until [`AcpConnection::close`] or drop.
pub struct AcpConnection {
    shutdown: oneshot::Sender<()>,
    /// The connection driver; `None` once it finished and was awaited.
    task: Option<tokio::task::JoinHandle<()>>,
}

impl AcpConnection {
    /// Resolves when the connection ends on its own, for example on a protocol error.
    pub async fn ended(&mut self) {
        if let Some(task) = &mut self.task {
            if let Err(error) = task.await {
                tracing::warn!("desktop ACP connection task failed: {}", error);
            }
            // A finished JoinHandle must not be polled again.
            self.task = None;
        }
    }

    /// Ends the connection and waits for its driver task to finish.
    pub async fn close(self) {
        let AcpConnection { shutdown, task } = self;
        // The driver may already have stopped on its own; either way it is finishing now.
        let _ = shutdown.send(());
        if let Some(task) = task
            && let Err(error) = task.await
        {
            tracing::warn!("desktop ACP connection task failed: {}", error);
        }
    }
}

/// Untyped request for one product extension method, answered with raw JSON.
#[derive(Debug, Clone)]
struct ExtRequest {
    method: &'static str,
    params: serde_json::Value,
}

impl JsonRpcMessage for ExtRequest {
    /// Product extension methods share one namespace prefix.
    fn matches_method(method: &str) -> bool {
        method.starts_with("_clawcode/")
    }

    /// Returns the extension method name.
    fn method(&self) -> &str {
        self.method
    }

    /// Serializes the parameters through the untyped ACP envelope.
    fn to_untyped_message(
        &self,
    ) -> Result<UntypedMessage, agent_client_protocol::Error> {
        UntypedMessage::new(self.method, &self.params)
    }

    /// The desktop only sends these requests; it never receives them.
    fn parse_message(
        _method: &str,
        _params: &serde_json::Value,
    ) -> Result<Self, agent_client_protocol::Error> {
        Err(agent_client_protocol::Error::method_not_found())
    }
}

impl JsonRpcRequest for ExtRequest {
    type Response = serde_json::Value;
}

impl AcpClient {
    /// Connects to an in-process ACP agent and completes `initialize`; call inside a tokio runtime.
    ///
    /// Every `session/update` is forwarded to `events`; when the connection
    /// ends, one [`BackendEvent::Stopped`] follows.
    pub async fn connect(
        component: impl ConnectTo<Client> + 'static,
        events: UnboundedSender<BackendEvent>,
    ) -> Result<(AcpClient, AcpConnection), BackendError> {
        let (ready_tx, ready_rx) =
            oneshot::channel::<Result<ConnectionTo<Agent>, String>>();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let updates = events.clone();
        let pending = PendingRequests::default();
        let (permissions, permission_events) =
            (pending.clone(), events.clone());
        let (elicitations, elicitation_events) =
            (pending.clone(), events.clone());
        let task = tokio::spawn(async move {
            let result = Client
                .v2()
                .name("clawcode-desktop")
                .on_receive_notification(
                    async move |notification: wire::UpdateSessionNotification, _connection| {
                        // A closed receiver means the UI is gone; nothing is left to notify.
                        let _ = updates.unbounded_send(BackendEvent::Update(Box::new(notification)));
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                // Requests are parked at once; the user answers them later from the UI.
                .on_receive_request(
                    async move |request: wire::RequestPermissionRequest,
                                responder: Responder<wire::RequestPermissionResponse>,
                                _connection| {
                        let cancellation = responder.cancellation();
                        let session = request.session_id.clone();
                        let id = permissions.park(
                            Waiting::Permission(responder),
                            cancellation,
                            permission_events.clone(),
                            |id| AgentRequest::Permission { id, request },
                        );
                        tracing::info!("permission request {} for session {}", id.0, session);
                        Ok(())
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    async move |request: wire::CreateElicitationRequest,
                                responder: Responder<wire::CreateElicitationResponse>,
                                _connection| {
                        // Request-scoped elicitations come from provider sign-in, which arrives in M4.
                        if !matches!(request.scope(), wire::ElicitationScope::Session(_)) {
                            tracing::info!("declining a request-scoped elicitation; sign-in is not supported yet");
                            return responder.respond(wire::CreateElicitationResponse::new(
                                wire::ElicitationAction::Cancel,
                            ));
                        }
                        let cancellation = responder.cancellation();
                        let id = elicitations.park(
                            Waiting::Elicitation(responder),
                            cancellation,
                            elicitation_events.clone(),
                            |id| AgentRequest::Elicitation { id, request },
                        );
                        tracing::info!("elicitation request {}", id.0);
                        Ok(())
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(component, async move |connection| {
                    let initialize = connection
                        .send_request(
                            wire::InitializeRequest::new(
                                ProtocolVersion::V2,
                                wire::Implementation::new(
                                    "clawcode-desktop",
                                    env!("CARGO_PKG_VERSION"),
                                ),
                            )
                            // Without these the agent never sends MCP elicitations.
                            .capabilities(wire::ClientCapabilities::new().elicitation(
                                wire::ElicitationCapabilities::new()
                                    .form(wire::ElicitationFormCapabilities::new())
                                    .url(wire::ElicitationUrlCapabilities::new()),
                            )),
                        )
                        .block_task()
                        .await;
                    if let Err(error) = initialize {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return Err(error);
                    }
                    let _ = ready_tx.send(Ok(connection.clone()));
                    // Either an explicit close or a dropped `AcpConnection` ends the connection.
                    let _ = shutdown_rx.await;
                    Ok(())
                })
                .await;
            let reason = result.err().map(|error| error.to_string());
            match &reason {
                Some(error) => tracing::warn!(
                    "desktop ACP connection ended with an error: {}",
                    error
                ),
                None => tracing::info!("desktop ACP connection closed"),
            }
            let _ = events.unbounded_send(BackendEvent::Stopped(reason));
        });
        let connection = ready_rx
            .await
            .map_err(|error| {
                BackendError::Startup(format!(
                    "ACP connection ended during initialize: {error}"
                ))
            })?
            .map_err(BackendError::Startup)?;
        Ok((
            AcpClient {
                connection,
                runtime: tokio::runtime::Handle::current(),
                pending,
            },
            AcpConnection {
                shutdown: shutdown_tx,
                task: Some(task),
            },
        ))
    }

    /// Sends one request on the backend runtime and waits for its response.
    async fn request<R>(&self, request: R) -> Result<R::Response, BackendError>
    where
        R: JsonRpcRequest + Send + 'static,
        R::Response: Send + 'static,
    {
        let connection = self.connection.clone();
        self.runtime
            .spawn(async move {
                connection.send_request(request).block_task().await
            })
            .await
            .map_err(|error| {
                BackendError::Request(format!("backend task failed: {error}"))
            })?
            .map_err(BackendError::from)
    }

    /// Lists every persisted session.
    pub async fn list_sessions(
        &self,
    ) -> Result<Vec<wire::SessionInfo>, BackendError> {
        Ok(self
            .request(wire::ListSessionsRequest::new())
            .await?
            .sessions)
    }

    /// Creates a session whose working directory is `cwd`.
    pub async fn new_session(
        &self,
        cwd: &Path,
    ) -> Result<SessionStart, BackendError> {
        let response = self
            .request(wire::NewSessionRequest::new(cwd.to_path_buf()))
            .await?;
        Ok(SessionStart {
            id: response.session_id,
            config_options: response.config_options,
        })
    }

    /// Attaches to a session and replays its whole retained transcript as updates.
    pub async fn resume_with_replay(
        &self,
        id: &SessionId,
        cwd: &Path,
    ) -> Result<SessionStart, BackendError> {
        let response = self
            .request(
                wire::ResumeSessionRequest::new(id.clone(), cwd.to_path_buf())
                    .replay_from(wire::ReplayFrom::Start(
                        wire::ReplayFromStart::new(),
                    )),
            )
            .await?;
        Ok(SessionStart {
            id: id.clone(),
            config_options: response.config_options,
        })
    }

    /// Releases a session's live resources in the backend.
    pub async fn close_session(
        &self,
        id: &SessionId,
    ) -> Result<(), BackendError> {
        self.request(wire::CloseSessionRequest::new(id.clone()))
            .await?;
        Ok(())
    }

    /// Deletes a session and its history.
    pub async fn delete_session(
        &self,
        id: &SessionId,
    ) -> Result<(), BackendError> {
        self.request(wire::DeleteSessionRequest::new(id.clone()))
            .await?;
        Ok(())
    }

    /// Renames a session.
    pub async fn set_title(
        &self,
        id: &SessionId,
        title: &str,
    ) -> Result<(), BackendError> {
        self.request(wire::SetSessionTitleRequest::new(id.clone(), title))
            .await?;
        Ok(())
    }

    /// Reports whether the session currently owns an active run.
    pub async fn session_running(
        &self,
        id: &SessionId,
    ) -> Result<bool, BackendError> {
        let snapshot = self
            .request(ExtRequest {
                method: AcpExtensionMethod::SessionRuntime.as_str(),
                params: serde_json::json!({ "sessionId": id }),
            })
            .await?;
        serde_json::from_value::<SessionRuntimeSnapshot>(snapshot)
            .map(|snapshot| snapshot.running)
            .map_err(|error| {
                BackendError::Request(format!(
                    "invalid session runtime snapshot: {error}"
                ))
            })
    }

    /// Sends one prompt made of content blocks; the reply arrives as session updates.
    pub async fn prompt(
        &self,
        id: &SessionId,
        blocks: Vec<wire::ContentBlock>,
    ) -> Result<(), BackendError> {
        self.request(wire::PromptRequest::new(id.clone(), blocks))
            .await?;
        Ok(())
    }

    /// Sends one product extension request and returns its raw JSON answer.
    async fn extension(
        &self,
        method: AcpExtensionMethod,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, BackendError> {
        self.request(ExtRequest {
            method: method.as_str(),
            params,
        })
        .await
    }

    /// Sends one extension request and decodes its typed answer.
    async fn typed<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: AcpExtensionMethod,
        params: &P,
    ) -> Result<R, BackendError> {
        let params = serde_json::to_value(params).map_err(|error| {
            BackendError::Request(format!(
                "invalid {method} parameters: {error}"
            ))
        })?;
        let value = self.extension(method, params).await?;
        serde_json::from_value(value).map_err(|error| {
            BackendError::Request(format!("invalid {method} answer: {error}"))
        })
    }

    /// Lists one workspace directory; `path` is root-relative, empty for the root.
    pub async fn list_dir(
        &self,
        root: &Path,
        path: &str,
        show_hidden: bool,
    ) -> Result<WorkspaceDirListing, BackendError> {
        let params = WorkspaceListDirParameters {
            root: root.to_path_buf(),
            path: path.to_owned(),
            show_hidden,
        };
        self.typed(AcpExtensionMethod::WorkspaceListDir, &params)
            .await
    }

    /// Reads one workspace file for preview.
    pub async fn read_file(
        &self,
        root: &Path,
        path: &str,
    ) -> Result<WorkspaceFileContent, BackendError> {
        let params = WorkspacePathParameters {
            root: root.to_path_buf(),
            path: path.to_owned(),
        };
        self.typed(AcpExtensionMethod::WorkspaceReadFile, &params)
            .await
    }

    /// Reads a workspace's Git branch and changed files.
    pub async fn git_status(
        &self,
        root: &Path,
    ) -> Result<WorkspaceGitStatus, BackendError> {
        let params = WorkspaceRootParameters {
            root: root.to_path_buf(),
        };
        self.typed(AcpExtensionMethod::WorkspaceGitStatus, &params)
            .await
    }

    /// Reads the diff of one changed workspace file: by its raw name when
    /// that is not UTF-8, and with its old path when it was renamed.
    pub async fn git_diff(
        &self,
        root: &Path,
        change: &WorkspaceChange,
        scope: WorkspaceDiffScope,
    ) -> Result<WorkspaceDiff, BackendError> {
        let params = WorkspaceDiffParameters::builder()
            .root(root.to_path_buf())
            .path(change.path.clone())
            .original_path(change.original_path.clone())
            .path_bytes(change.path_bytes.clone())
            .scope(scope)
            .build();
        self.typed(AcpExtensionMethod::WorkspaceGitDiff, &params)
            .await
    }

    /// Lists a workspace's changed files of one scope with line counts.
    pub async fn git_changes(
        &self,
        root: &Path,
        scope: WorkspaceDiffScope,
    ) -> Result<WorkspaceChanges, BackendError> {
        let params = WorkspaceChangesParameters {
            root: root.to_path_buf(),
            scope,
        };
        self.typed(AcpExtensionMethod::WorkspaceGitChanges, &params)
            .await
    }

    /// Lists the session's runs that changed `root`, its working directory.
    pub async fn checkpoints(
        &self,
        root: &Path,
        id: &SessionId,
    ) -> Result<CheckpointList, BackendError> {
        let params = CheckpointsParameters {
            root: root.to_path_buf(),
            session_id: protocol::SessionId::try_from(id.to_string())
                .map_err(|error| BackendError::Request(error.to_string()))?,
        };
        self.typed(AcpExtensionMethod::Checkpoints, &params).await
    }

    /// Reads one changed file's change within one run of the session, by
    /// its raw name and old path like [`Self::git_diff`].
    pub async fn checkpoint_diff(
        &self,
        root: &Path,
        id: &SessionId,
        run: &RunId,
        change: &WorkspaceChange,
    ) -> Result<WorkspaceDiff, BackendError> {
        let params = CheckpointDiffParameters::builder()
            .root(root.to_path_buf())
            .session_id(
                protocol::SessionId::try_from(id.to_string()).map_err(
                    |error| BackendError::Request(error.to_string()),
                )?,
            )
            .run_id(run.clone())
            .path(change.path.clone())
            .original_path(change.original_path.clone())
            .path_bytes(change.path_bytes.clone())
            .build();
        self.typed(AcpExtensionMethod::CheckpointDiff, &params)
            .await
    }

    /// Reads the session's whole persisted tree and its active leaf.
    pub async fn tree(
        &self,
        id: &SessionId,
    ) -> Result<SessionTreeSnapshot, BackendError> {
        let params = serde_json::json!({ "sessionId": id });
        self.typed(AcpExtensionMethod::Tree, &params).await
    }

    /// The tree entry of message `message_id` written by `author`; an error
    /// when the session has no record of it.
    pub async fn message_entry(
        &self,
        id: &SessionId,
        message_id: &str,
        author: crate::branch::MessageAuthor,
    ) -> Result<protocol::SessionTreeEntry, BackendError> {
        use crate::branch::MessageEntries as _;

        self.tree(id)
            .await?
            .message_entry(message_id, author)
            .cloned()
            .ok_or_else(|| {
                BackendError::Request(
                    rust_i18n::t!("Message.entry_missing").into_owned(),
                )
            })
    }

    /// Moves the session's active branch to `entry`, the root when `None`,
    /// without summarizing the branch it leaves. A user message moves it to
    /// that message's parent, so the message can be asked again.
    pub async fn branch(
        &self,
        id: &SessionId,
        entry: Option<&EntryId>,
    ) -> Result<(), BackendError> {
        let params = serde_json::json!({
            "sessionId": id,
            "entryId": entry,
            "summarize": false,
        });
        let _: serde_json::Value =
            self.typed(AcpExtensionMethod::Branch, &params).await?;
        Ok(())
    }

    /// Forks the session's history up to `entry` into a new session in
    /// `cwd`; returns the new session's id.
    pub async fn fork(
        &self,
        id: &SessionId,
        entry: &EntryId,
        cwd: &Path,
    ) -> Result<SessionId, BackendError> {
        /// The fork's answer: only the new session's id.
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Forked {
            session_id: SessionId,
        }
        let params = serde_json::json!({
            "sessionId": id,
            "entryId": entry,
            "cwd": cwd,
        });
        let forked: Forked =
            self.typed(AcpExtensionMethod::Fork, &params).await?;
        Ok(forked.session_id)
    }

    /// Reads one page of the session's active-branch trajectory.
    pub async fn trajectory(
        &self,
        id: &SessionId,
        cursor: TrajectoryCursor,
    ) -> Result<TrajectoryPage, BackendError> {
        let (before, after) = match cursor {
            TrajectoryCursor::Tail => (None, None),
            TrajectoryCursor::Before(sequence) => (Some(sequence), None),
            TrajectoryCursor::After(sequence) => (None, Some(sequence)),
        };
        let params = serde_json::json!({
            "sessionId": id,
            "before": before,
            "after": after,
        });
        self.typed(AcpExtensionMethod::Trajectory, &params).await
    }

    /// Reads one request content (system prompt or tools) by hash; `None`
    /// when the session's log does not hold it.
    pub async fn request_content(
        &self,
        id: &SessionId,
        hash: &ContentHash,
    ) -> Result<Option<RequestContent>, BackendError> {
        let params = serde_json::json!({ "sessionId": id, "hash": hash });
        self.typed(AcpExtensionMethod::TrajectoryContent, &params)
            .await
    }

    /// Reads the session's queued steering and follow-up messages.
    pub async fn pending_messages(
        &self,
        id: &SessionId,
    ) -> Result<PendingMessages, BackendError> {
        let value = self
            .extension(
                AcpExtensionMethod::PendingMessages,
                serde_json::json!({ "sessionId": id }),
            )
            .await?;
        serde_json::from_value(value).map_err(|error| {
            BackendError::Request(format!("invalid pending messages: {error}"))
        })
    }

    /// Moves a queued message to the steering lane; returns whether it moved.
    pub async fn promote_pending(
        &self,
        id: &SessionId,
        queue_id: &QueueId,
    ) -> Result<bool, BackendError> {
        let value = self
            .extension(
                AcpExtensionMethod::PendingMessagePromote,
                serde_json::json!({ "sessionId": id, "queueId": queue_id }),
            )
            .await?;
        value
            .get("promoted")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| {
                BackendError::Request(format!(
                    "invalid promote response: {value}"
                ))
            })
    }

    /// Removes a queued message.
    pub async fn remove_pending(
        &self,
        id: &SessionId,
        queue_id: &QueueId,
    ) -> Result<(), BackendError> {
        self.extension(
            AcpExtensionMethod::PendingMessageRemove,
            serde_json::json!({ "sessionId": id, "queueId": queue_id }),
        )
        .await?;
        Ok(())
    }

    /// Changes one configuration option and returns the complete new option list.
    pub async fn set_config_option(
        &self,
        id: &SessionId,
        config_id: &str,
        value: &str,
    ) -> Result<Vec<wire::SessionConfigOption>, BackendError> {
        Ok(self
            .request(wire::SetSessionConfigOptionRequest::new(
                id.clone(),
                config_id.to_owned(),
                wire::SessionConfigOptionValue::id(value.to_owned()),
            ))
            .await?
            .config_options)
    }

    /// The config options a new session starts with, for a draft to offer
    /// before its session exists.
    pub async fn default_config(
        &self,
    ) -> Result<Vec<wire::SessionConfigOption>, BackendError> {
        self.typed(AcpExtensionMethod::DefaultConfig, &serde_json::json!({}))
            .await
    }

    /// Compacts the session's context now.
    pub async fn compact(&self, id: &SessionId) -> Result<(), BackendError> {
        self.extension(
            AcpExtensionMethod::Compact,
            serde_json::json!({ "sessionId": id }),
        )
        .await?;
        Ok(())
    }

    /// Skips open questions; already settled ones are ignored by the agent.
    pub async fn dismiss_questions(
        &self,
        id: &SessionId,
        keys: &[QuestionKey],
    ) -> Result<(), BackendError> {
        let items: Vec<serde_json::Value> = keys
            .iter()
            .map(|key| {
                serde_json::json!({ "requestId": key.request_id, "index": key.index })
            })
            .collect();
        self.extension(
            AcpExtensionMethod::QuestionDismiss,
            serde_json::json!({ "sessionId": id, "items": items }),
        )
        .await?;
        Ok(())
    }

    /// Answers one asynchronous question; the answer steers into the running run or starts one.
    pub async fn answer_question(
        &self,
        id: &SessionId,
        key: &QuestionKey,
        text: &str,
    ) -> Result<(), BackendError> {
        let mut meta = wire::Meta::new();
        meta.insert(
            protocol::ProductIdentity::ACP_NAMESPACE.to_owned(),
            serde_json::json!({
                "questionReply": { "requestId": key.request_id, "index": key.index }
            }),
        );
        self.request(
            wire::PromptRequest::new(
                id.clone(),
                vec![wire::ContentBlock::Text(wire::TextContent::new(text))],
            )
            .meta(meta),
        )
        .await?;
        Ok(())
    }

    /// Answers a permission request with one of its options, or `None` to cancel it.
    ///
    /// A request the agent already withdrew is answered by nobody: this is then a no-op.
    pub fn answer_permission(
        &self,
        id: RequestId,
        option_id: Option<&str>,
    ) -> Result<(), BackendError> {
        self.pending.answer_permission(id, option_id)
    }

    /// Answers an elicitation; a request the agent already withdrew makes this a no-op.
    pub fn answer_elicitation(
        &self,
        id: RequestId,
        response: wire::CreateElicitationResponse,
    ) -> Result<(), BackendError> {
        self.pending.answer_elicitation(id, response)
    }

    /// Asks the agent to stop the session's current run.
    pub fn cancel(&self, id: &SessionId) -> Result<(), BackendError> {
        self.connection
            .send_notification(wire::CancelSessionNotification::new(id.clone()))
            .map_err(|error| BackendError::Request(error.to_string()))
    }
}
