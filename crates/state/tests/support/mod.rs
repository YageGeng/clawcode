//! Shared fixtures that write real JSONL Sessions and read the databases externally.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use protocol::{
    AgentMessage, AgentName, AssistantMetadata, ContentBlock, EntryId, LaneId,
    MessageContent, MessageId, MessageIdentity, MessageTiming, ModelUsage,
    RecordId, RunId, SessionId, SessionOwnership, SlashCommandExpansion,
    SlashCommandInvocation, SlashCommandSource, StopReason, TimestampMs,
    ToolCallId, TurnId,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use state::{SqliteConfig, StateRuntime};
use store::{
    Clock, EntryKind, JsonlStoreFactory, NewEntry, NewRecord,
    OwnedSessionCreateOptions, RecordKind, SessionCreateOptions, SessionStore,
    StoreFactory,
};

/// Clock that advances by one millisecond per reading, so timestamps are distinct.
#[derive(Default)]
pub struct TickClock(AtomicU64);

impl Clock for TickClock {
    /// Returns the next millisecond starting at 1000.
    fn now(&self) -> TimestampMs {
        TimestampMs::from(1000 + self.0.fetch_add(1, Ordering::SeqCst))
    }
}

/// Temporary data home with a JSONL factory and an initialized state runtime.
#[derive(typed_builder::TypedBuilder)]
pub struct Fixture {
    pub home: tempfile::TempDir,
    pub factory: Arc<JsonlStoreFactory>,
    pub runtime: Arc<StateRuntime>,
    pub config: SqliteConfig,
}

impl Fixture {
    /// Creates `sessions/` and the state databases under one temporary home.
    pub async fn new() -> Self {
        let home = tempfile::tempdir().expect("home");
        let factory = Arc::new(JsonlStoreFactory::new(
            home.path().join("sessions"),
            Arc::new(TickClock::default()),
        ));
        let config = SqliteConfig::new(home.path().join("state"));
        let runtime = StateRuntime::init(&config).await.expect("init");
        Self::builder()
            .home(home)
            .factory(factory)
            .runtime(runtime)
            .config(config)
            .build()
    }

    /// Returns default creation options for `id` in the `project` cwd.
    pub fn options(&self, id: &str) -> SessionCreateOptions {
        SessionCreateOptions::builder()
            .session_id(SessionId::try_from(id).expect("id"))
            .cwd(self.cwd("project"))
            .build()
    }

    /// Returns a cwd path under the temporary home.
    pub fn cwd(&self, name: &str) -> PathBuf {
        self.home.path().join(name)
    }

    /// Starts a plain Session log.
    pub fn session(&self, options: SessionCreateOptions) -> SessionLog {
        SessionLog::new(self.factory.create(options).expect("create"))
    }

    /// Starts an owned Session log.
    pub fn owned_session(
        &self,
        options: SessionCreateOptions,
        owner: &str,
        agent: &str,
        nickname: &str,
    ) -> SessionLog {
        let store = self
            .factory
            .create_owned(OwnedSessionCreateOptions {
                create: options,
                ownership: SessionOwnership {
                    session_id: SessionId::try_from(owner).expect("owner"),
                    name: AgentName::try_from(agent).expect("agent"),
                    nickname: nickname.into(),
                },
            })
            .expect("create owned");
        SessionLog::new(store)
    }

    /// Opens an external pool on the metadata database.
    pub async fn state_pool(&self) -> SqlitePool {
        external(&self.config.state_db_path()).await
    }

    /// Opens an external pool on the history database.
    pub async fn history_pool(&self) -> SqlitePool {
        external(&self.config.session_history_db_path()).await
    }
}

/// Opens an independent read pool, as a GUI in another process would.
pub async fn external(path: &Path) -> SqlitePool {
    SqlitePool::connect_with(SqliteConnectOptions::new().filename(path))
        .await
        .expect("external pool")
}

/// Writes kernel-shaped entries and records into one Session log.
pub struct SessionLog {
    pub store: Box<dyn SessionStore>,
    lane: LaneId,
    counter: u64,
}

impl SessionLog {
    /// Wraps a freshly created store appending on lane `main`.
    pub fn new(store: Box<dyn SessionStore>) -> Self {
        Self {
            store,
            lane: LaneId::try_from("main").expect("lane"),
            counter: 0,
        }
    }

    /// Returns the Session identity.
    pub fn id(&self) -> SessionId {
        self.store.session_id().clone()
    }

    /// Returns the log path.
    pub fn path(&self) -> PathBuf {
        self.store.path().to_path_buf()
    }

    /// Allocates a unique suffix for generated identifiers.
    fn next(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    /// Switches appends to `lane`, creating it at `main`'s leaf when missing.
    pub fn on_lane(&mut self, lane: &str) -> &mut Self {
        let lane = LaneId::try_from(lane).expect("lane");
        let main = LaneId::try_from("main").expect("lane");
        if self.store.lane(&lane).is_none() && lane != main {
            let leaf = self.store.lane(&main).cloned();
            self.store
                .create_lane(lane.clone(), leaf)
                .expect("create lane");
        }
        self.lane = lane;
        self
    }

    /// Appends one entry with a raw payload and returns its id.
    pub fn entry(
        &mut self,
        kind: EntryKind,
        payload: serde_json::Value,
    ) -> EntryId {
        let id =
            EntryId::try_from(format!("entry-{}", self.next())).expect("entry");
        self.store
            .append_entry(
                &self.lane.clone(),
                NewEntry {
                    id: id.clone(),
                    kind,
                    payload,
                },
            )
            .expect("append entry");
        id
    }

    /// Appends one kernel-shaped message entry.
    pub fn message(&mut self, content: MessageContent) -> EntryId {
        let n = self.next();
        let at = TimestampMs::from(500 + n);
        let message = AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from(format!("message-{n}"))
                    .expect("message"),
                turn_id: TurnId::try_from("turn-1").expect("turn"),
            },
            timing: MessageTiming::try_from((at, at, at)).expect("timing"),
            content,
        };
        let payload = serde_json::to_value(&message).expect("message json");
        self.entry(EntryKind::Message, payload)
    }

    /// Appends a user message with one text block.
    pub fn user(&mut self, text: &str) -> EntryId {
        self.message(MessageContent::User {
            blocks: vec![ContentBlock::Text { text: text.into() }],
        })
    }

    /// Appends a Slash Command invocation expanded into model content.
    pub fn expanded_user(&mut self, original: &str) -> EntryId {
        self.message(MessageContent::ExpandedUser {
            expansion: SlashCommandExpansion {
                invocation: SlashCommandInvocation {
                    original: original.into(),
                    name: "review".into(),
                    arguments: String::new(),
                },
                source: SlashCommandSource::PromptTemplate,
                model_blocks: vec![ContentBlock::Text {
                    text: "expanded".into(),
                }],
            },
        })
    }

    /// Appends an assistant message reporting `total_tokens`.
    pub fn assistant(
        &mut self,
        provider: &str,
        model: &str,
        total_tokens: u64,
    ) -> EntryId {
        let usage: ModelUsage = serde_json::from_value(serde_json::json!({
            "format": "rig-core-v1",
            "usage": {"input_tokens": total_tokens, "output_tokens": 0, "total_tokens": total_tokens}
        }))
        .expect("usage");
        self.message(MessageContent::Assistant {
            blocks: vec![ContentBlock::Text { text: "ok".into() }],
            metadata: AssistantMetadata::builder()
                .provider_id(provider.into())
                .model_id(model.into())
                .stop_reason(StopReason::EndTurn)
                .usage(usage)
                .build(),
            model_message: None,
        })
    }

    /// Appends a successful tool result.
    pub fn tool_result(&mut self, call_id: &str) -> EntryId {
        self.message(MessageContent::ToolResult {
            tool_call_id: ToolCallId::try_from(call_id).expect("call"),
            blocks: vec![ContentBlock::Text {
                text: "done".into(),
            }],
            is_error: false,
            details: None,
            output: None,
            cancelled: false,
        })
    }

    /// Appends a model selection change.
    pub fn model_change(&mut self, provider: &str, model: &str) -> EntryId {
        self.entry(
            EntryKind::ModelChange,
            serde_json::json!({"provider": provider, "modelId": model}),
        )
    }

    /// Appends a thinking-level change.
    pub fn thinking(&mut self, level: &str) -> EntryId {
        self.entry(
            EntryKind::ThinkingLevelChange,
            serde_json::json!({"thinkingLevel": level}),
        )
    }

    /// Appends a custom entry in the pi v4 extension shape.
    pub fn custom(
        &mut self,
        extension_id: &str,
        custom_type: &str,
        data: serde_json::Value,
    ) -> EntryId {
        self.entry(
            EntryKind::Custom,
            serde_json::json!({"extensionId": extension_id, "customType": custom_type, "data": data}),
        )
    }

    /// Appends one operation record on the current lane.
    fn record(
        &mut self,
        run_id: &str,
        kind: RecordKind,
        payload: serde_json::Value,
    ) {
        let n = self.next();
        self.store
            .append_record(
                NewRecord::builder()
                    .id(RecordId::try_from(format!("record-{n}"))
                        .expect("record"))
                    .lane(self.lane.clone())
                    .run_id(Some(RunId::try_from(run_id).expect("run")))
                    .kind(kind)
                    .payload(payload)
                    .build(),
            )
            .expect("append record");
    }

    /// Opens a Run as `crates/kernel/src/runtime/run.rs` does.
    pub fn run_started(&mut self, run_id: &str, operation: &str) {
        self.record(
            run_id,
            RecordKind::OperationStarted,
            serde_json::json!({"operation": operation}),
        );
    }

    /// Closes a Run as `crates/kernel/src/runtime/run/settlement.rs` does.
    pub fn run_finished(
        &mut self,
        run_id: &str,
        status: &str,
        error: Option<&str>,
    ) {
        let payload = match error {
            Some(error) => {
                serde_json::json!({"status": status, "error": error})
            }
            None => serde_json::json!({"status": status}),
        };
        self.record(run_id, RecordKind::OperationFinished, payload);
    }

    /// Opens a compaction Run as `crates/kernel/src/runtime/compaction.rs` does.
    pub fn compaction_started(&mut self, run_id: &str) {
        let payload = protocol::CompactionOperationStarted {
            source_leaf_id: None,
            intent: protocol::CompactionOperationIntent::new(
                None,
                EntryId::try_from("entry-compaction").expect("entry id"),
            ),
        };
        self.record(
            run_id,
            RecordKind::OperationStarted,
            serde_json::to_value(payload).expect("payload"),
        );
    }

    /// Closes a compaction Run as `crates/kernel/src/runtime/compaction/lifecycle.rs` does.
    pub fn compaction_finished(
        &mut self,
        run_id: &str,
        outcome: protocol::CompactionOperationOutcome,
    ) {
        let payload = protocol::CompactionOperationFinished {
            outcome,
            error: None,
        };
        self.record(
            run_id,
            RecordKind::OperationFinished,
            serde_json::to_value(payload).expect("payload"),
        );
    }

    /// Writes a name fact.
    pub fn name(&mut self, name: Option<&str>) {
        self.store
            .set_name(name.map(ToOwned::to_owned))
            .expect("name");
    }

    /// Writes a label fact.
    pub fn label(&mut self, entry: &EntryId, label: Option<&str>) {
        self.store
            .set_label(entry.clone(), label.map(ToOwned::to_owned))
            .expect("label");
    }

    /// Makes everything written so far durable.
    pub fn sync(&mut self) {
        self.store.sync().expect("sync");
    }
}
