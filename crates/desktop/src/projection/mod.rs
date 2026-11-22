//! One session's transcript, folded from ACP `session/update`s into ordered rows.
//!
//! Mirrors the WebUI decoder (`web/src/workspace/updateDecoder.ts`) for the
//! rows M1 renders: messages, thinking, tool calls, run state, and the title.
//! Rows are ordered by kernel sequence; arrival order breaks ties.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use agent_client_protocol::v2::schema as wire;
use base64::Engine as _;
use protocol::{AgentEventPayload, ProductIdentity};
use typed_builder::TypedBuilder;

use events::ProductEvent;

mod controls;
mod events;
mod meta;
mod rows;

pub use controls::{
    AskedQuestion, CommandSource, CompactionStatus, ConfigChoice, ConfigOption,
    ContextUsage, ModelUsage, RetryState, SessionControls, SlashCommand, Todo,
    TodoStatus,
};
pub use meta::{
    AssistantMeta, CommandKind, CommandMeta, CommandStatus, ProductMeta,
};
pub use rows::{
    CompactionReason, CompactionRow, ErrorRow, ImageBlock, MailRow,
    MessageOrigin, MessageRow, Role, RowKey, ToolCategory, ToolRow, ToolStatus,
    TranscriptRow,
};

/// Whether the session's foreground work is active.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunState {
    /// Nothing is running.
    #[default]
    Idle,
    /// A run is in progress.
    Running,
    /// The run waits for the user.
    RequiresAction,
}

/// What one or more applied updates changed, by row identity.
///
/// Keys rather than indexes, because a later insertion in the same batch
/// shifts earlier indexes; consumers resolve keys with
/// [`SessionProjection::index_of`] once the batch is applied.
#[derive(Debug, Clone, Default, PartialEq, Eq, TypedBuilder)]
pub struct ProjectionChange {
    /// Rows that did not exist before.
    #[builder(default)]
    pub inserted: HashSet<RowKey>,
    /// Existing rows whose content changed; never also in `inserted`.
    #[builder(default)]
    pub updated: HashSet<RowKey>,
    /// Whether the run state changed.
    #[builder(default)]
    pub run_state: bool,
    /// Whether the title changed.
    #[builder(default)]
    pub title: bool,
    /// Whether the pending queue changed.
    #[builder(default)]
    pub queue: bool,
    /// Whether the session controls changed (options, usage, plan, questions, requests).
    #[builder(default)]
    pub controls: bool,
    /// Whether the transcript layout may have changed: a row appeared, a
    /// message's visibility, a tool's fields, or the run state changed.
    ///
    /// `false` means only the content of the `updated` rows grew, such as
    /// streamed text or tool output, so their positions are unchanged.
    #[builder(default)]
    pub structural: bool,
    /// Whether the history database gained something the trajectory shows:
    /// a finished message or tool call, or the end of a turn or run.
    #[builder(default)]
    pub history: bool,
    /// Whether a subagent was spawned, so the session list now holds a new
    /// child of this session.
    #[builder(default)]
    pub agents: bool,
}

impl ProjectionChange {
    /// Whether nothing changed.
    pub fn is_empty(&self) -> bool {
        self.inserted.is_empty()
            && self.updated.is_empty()
            && !self.run_state
            && !self.title
            && !self.queue
            && !self.controls
            && !self.structural
            && !self.history
            && !self.agents
    }

    /// Folds a later change into this one; an inserted row is never also reported as updated.
    pub fn extend(&mut self, other: ProjectionChange) {
        for key in other.inserted {
            self.mark_inserted(key);
        }
        for key in other.updated {
            self.mark_updated(key);
        }
        self.run_state |= other.run_state;
        self.title |= other.title;
        self.queue |= other.queue;
        self.controls |= other.controls;
        self.agents |= other.agents;
        self.structural |= other.structural;
        self.history |= other.history;
    }

    /// Records a new row; it supersedes an earlier update of the same key.
    pub fn mark_inserted(&mut self, key: RowKey) {
        self.updated.remove(&key);
        self.inserted.insert(key);
        self.structural = true;
    }

    /// Records an update unless the row is already reported as inserted.
    fn mark_updated(&mut self, key: RowKey) {
        if !self.inserted.contains(&key) {
            self.updated.insert(key);
        }
    }
}

/// A row with its sort key: kernel sequence, then arrival.
#[derive(Debug, Clone)]
struct Entry {
    order: (u64, u64),
    row: TranscriptRow,
}

/// Which text of a message a chunk extends.
#[derive(Debug, Clone, Copy)]
enum ChunkTarget {
    Text,
    Reasoning,
}

/// The transcript of one session.
///
/// The builder is private: rows and their index must stay consistent, so
/// only [`Default`] constructs a projection from outside.
#[derive(Debug, TypedBuilder)]
#[builder(
    builder_method(vis = ""),
    builder_type(vis = ""),
    build_method(vis = "")
)]
pub struct SessionProjection {
    /// Always starts empty; rows only arrive through [`SessionProjection::apply`].
    #[builder(default, setter(skip))]
    entries: Vec<Entry>,
    #[builder(default)]
    index: HashMap<RowKey, usize>,
    #[builder(default)]
    run_state: RunState,
    #[builder(default)]
    title: Option<String>,
    /// Counts applied updates; the tie-breaker of the sort key.
    #[builder(default)]
    arrivals: u64,
    /// Highest kernel sequence seen; rows without one sort after it.
    #[builder(default)]
    max_sequence: u64,
    /// Messages waiting behind the active run.
    ///
    /// `PendingMessages` has no `Default`, so the empty queue is spelled out.
    #[builder(default = protocol::PendingMessages::builder()
        .steering(Vec::new())
        .follow_up(Vec::new())
        .build())]
    pending: protocol::PendingMessages,
    /// When the current run started, in Unix milliseconds.
    #[builder(default)]
    run_started_ms: Option<u64>,
    /// Options, commands, usage, plan, questions, and pending requests.
    #[builder(default)]
    controls: SessionControls,
}

impl Default for SessionProjection {
    /// An empty, idle transcript with nothing queued.
    fn default() -> Self {
        Self::builder().build()
    }
}

impl SessionProjection {
    /// Session-wide control state.
    pub fn controls(&self) -> &SessionControls {
        &self.controls
    }

    /// Mutable control state, for configuration lists, requests, and question settling.
    pub fn controls_mut(&mut self) -> &mut SessionControls {
        &mut self.controls
    }

    /// Questions still waiting for the user, excluding ones a queued message already answers.
    pub fn open_questions(&self) -> Vec<&AskedQuestion> {
        self.controls.open_questions()
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no rows.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The row at `index`.
    pub fn row(&self, index: usize) -> Option<&TranscriptRow> {
        self.entries.get(index).map(|entry| &entry.row)
    }

    /// Current position of a row.
    pub fn index_of(&self, key: &RowKey) -> Option<usize> {
        self.index.get(key).copied()
    }

    /// Whether foreground work is active.
    pub fn run_state(&self) -> RunState {
        self.run_state
    }

    /// Session title, when one is set.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Messages queued behind the active run.
    pub fn pending(&self) -> &protocol::PendingMessages {
        &self.pending
    }

    /// Replaces the pending queue; returns whether it changed.
    pub fn set_pending(&mut self, pending: protocol::PendingMessages) -> bool {
        let changed = self.pending != pending;
        if changed {
            self.controls.set_queued(&pending);
        }
        self.pending = pending;
        changed
    }

    /// When the current run started; `None` while idle.
    ///
    /// Attaching to a run already in progress records no start, so this falls
    /// back to the newest prompt the user sent.
    pub fn run_started_ms(&self) -> Option<u64> {
        if self.run_state != RunState::Running {
            return None;
        }
        self.run_started_ms.or_else(|| {
            self.entries
                .iter()
                .rev()
                .find_map(|entry| match &entry.row {
                    TranscriptRow::Message(row)
                        if row.role == Role::User
                            && row.origin == MessageOrigin::Prompt =>
                    {
                        row.timestamp_ms
                    }
                    _ => None,
                })
        })
    }

    /// Whether some tool is still executing, so its elapsed time must advance.
    pub fn has_running_tools(&self) -> bool {
        self.running_tools().next().is_some()
    }

    /// The tools still executing, in transcript order.
    pub fn running_tools(&self) -> impl Iterator<Item = &ToolRow> {
        self.entries.iter().filter_map(|entry| match &entry.row {
            TranscriptRow::Tool(tool) if tool.status == ToolStatus::Running => {
                Some(tool)
            }
            _ => None,
        })
    }

    /// Sets the run state read from the backend before a replay; returns whether it changed.
    pub fn set_run_state(&mut self, state: RunState) -> bool {
        let changed = self.run_state != state;
        self.run_state = state;
        changed
    }

    /// Drops every row before a full replay; run state and title are kept.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.index.clear();
        self.max_sequence = 0;
    }

    /// Whether `update` reports something finished that the history database
    /// now holds. Only the event name is read; nothing is decoded.
    fn ends_history_item(update: &wire::SessionUpdate) -> bool {
        match update {
            wire::SessionUpdate::Other(other) => {
                other.session_update
                    == protocol::ProductIdentity::ACP_EVENT_UPDATE
                    && matches!(
                        other
                            .fields
                            .get("payload")
                            .and_then(|payload| payload.get("event"))
                            .and_then(serde_json::Value::as_str),
                        Some("message_end" | "turn_end" | "run_end")
                    )
            }
            // Finished assistant and user messages arrive whole; a settings
            // change is logged as it is announced.
            wire::SessionUpdate::AgentMessage(_)
            | wire::SessionUpdate::UserMessage(_)
            | wire::SessionUpdate::ConfigOptionUpdate(_) => true,
            wire::SessionUpdate::ToolCallUpdate(tool) => matches!(
                tool.status.value(),
                Some(
                    wire::ToolCallStatus::Completed
                        | wire::ToolCallStatus::Failed
                        | wire::ToolCallStatus::Cancelled
                )
            ),
            _ => false,
        }
    }

    /// Folds one update into the transcript and reports what changed.
    pub fn apply(&mut self, update: &wire::SessionUpdate) -> ProjectionChange {
        self.arrivals += 1;
        let event = match update {
            wire::SessionUpdate::Other(other) => ProductEvent::decode(other),
            _ => None,
        };
        let mut change = ProjectionChange::builder()
            .controls(self.controls.apply(update, event.as_ref()))
            .history(Self::ends_history_item(update))
            .build();
        if let Some(event) = &event {
            self.apply_event(event, &mut change);
        }
        match update {
            wire::SessionUpdate::UserMessageChunk(chunk) => {
                self.append_chunk(
                    Role::User,
                    chunk,
                    ChunkTarget::Text,
                    &mut change,
                );
            }
            wire::SessionUpdate::AgentMessageChunk(chunk) => {
                self.append_chunk(
                    Role::Assistant,
                    chunk,
                    ChunkTarget::Text,
                    &mut change,
                );
            }
            wire::SessionUpdate::AgentThoughtChunk(chunk) => {
                self.append_chunk(
                    Role::Assistant,
                    chunk,
                    ChunkTarget::Reasoning,
                    &mut change,
                );
            }
            wire::SessionUpdate::UserMessage(message) => self.upsert_message(
                Role::User,
                &message.message_id,
                message.content.value(),
                message.meta.value(),
                &mut change,
            ),
            wire::SessionUpdate::AgentMessage(message) => self.upsert_message(
                Role::Assistant,
                &message.message_id,
                message.content.value(),
                message.meta.value(),
                &mut change,
            ),
            wire::SessionUpdate::AgentThought(thought) => {
                let meta = Self::product(thought.meta.value());
                let row = self.message_mut(
                    &thought.message_id,
                    Role::Assistant,
                    &meta,
                    &mut change,
                );
                if let Some(content) = thought.content.value() {
                    row.reasoning = Self::text_of(content);
                }
                // A complete thought may show or hide its message.
                change.structural = true;
            }
            wire::SessionUpdate::ToolCallUpdate(update) => {
                self.upsert_tool(update, &mut change)
            }
            wire::SessionUpdate::ToolCallContentChunk(chunk) => {
                let meta = Self::product(chunk.meta.as_ref());
                let row = self.tool_mut(
                    &chunk.tool_call_id,
                    ToolStatus::Running,
                    &meta,
                    &mut change,
                );
                if let Some(text) =
                    Self::tool_text(std::slice::from_ref(&chunk.content))
                {
                    row.output.push_str(&text);
                }
            }
            wire::SessionUpdate::StateUpdate(state) => {
                self.apply_state(state, &mut change)
            }
            wire::SessionUpdate::TerminalUpdate(terminal) => {
                self.apply_terminal(terminal, &mut change)
            }
            wire::SessionUpdate::CompactionUpdate(update) => {
                self.apply_compaction(update, &mut change)
            }
            wire::SessionUpdate::SessionInfoUpdate(info) => {
                if let Some(title) = info.title.as_opt_ref() {
                    self.title = title
                        .map(|title| title.trim())
                        .filter(|title| !title.is_empty())
                        .map(str::to_owned);
                    change.title = true;
                }
            }
            _ => {}
        }
        change
    }

    /// Replaces a tool's output with the terminal's full snapshot.
    fn apply_terminal(
        &mut self,
        terminal: &wire::TerminalUpdate,
        change: &mut ProjectionChange,
    ) {
        // Terminals share their tool call's id; the `Arc` is shared, not copied.
        let key = RowKey::Tool(wire::ToolCallId::from(Arc::clone(
            &terminal.terminal_id.0,
        )));
        let Some(output) = terminal.output.value() else {
            return;
        };
        // Unknown terminals and undecodable snapshots leave the transcript alone.
        let Some(&position) = self.index.get(&key) else {
            return;
        };
        let Ok(bytes) =
            base64::engine::general_purpose::STANDARD.decode(&output.data)
        else {
            return;
        };
        if let Some(TranscriptRow::Tool(row)) =
            self.entries.get_mut(position).map(|entry| &mut entry.row)
        {
            row.output = String::from_utf8_lossy(&bytes).into_owned();
            change.mark_updated(key);
        }
    }

    /// Applies a kernel event: queue snapshots and extension handler errors.
    fn apply_event(
        &mut self,
        event: &ProductEvent,
        change: &mut ProjectionChange,
    ) {
        match &event.payload {
            AgentEventPayload::QueueChanged { pending } => {
                change.queue |= self.set_pending(pending.clone());
            }
            AgentEventPayload::ExtensionHandlerFailed {
                extension_id,
                message,
                ..
            } => {
                let id = format!(
                    "{}:{}",
                    event.meta.turn_id.as_deref().unwrap_or_default(),
                    event.meta.sequence.unwrap_or_default()
                );
                self.insert_error(
                    ErrorRow {
                        id,
                        source: extension_id.as_ref().to_owned(),
                        message: message.clone(),
                    },
                    &event.meta,
                    change,
                );
            }
            // Persisted handler errors are extension messages marked `handler_error`.
            AgentEventPayload::MessageEnd { message } => {
                let protocol::MessageContent::Extension { extension } =
                    &message.content
                else {
                    return;
                };
                // Mail between agents shows as its own row (spec §7.2).
                if extension.custom_type == protocol::AGENT_MESSAGE_TYPE {
                    if extension.display {
                        self.insert_mail(
                            MailRow::from_extension(
                                message.identity.message_id.to_string(),
                                extension,
                            ),
                            &event.meta,
                            change,
                        );
                    }
                    return;
                }
                let kind = extension
                    .details
                    .as_ref()
                    .and_then(|details| details.get("kind"))
                    .and_then(serde_json::Value::as_str);
                if kind != Some("handler_error") {
                    return;
                }
                // The error text is the message's text blocks; the details
                // carry only the kind and the hook point.
                let text = extension
                    .blocks
                    .iter()
                    .filter_map(|block| match block {
                        protocol::ContentBlock::Text { text } => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.insert_error(
                    ErrorRow {
                        id: message.identity.message_id.to_string(),
                        source: extension.extension_id.as_ref().to_owned(),
                        message: text,
                    },
                    &event.meta,
                    change,
                );
            }
            _ => {}
        }
    }

    /// Adds a mail row once; a replayed duplicate is ignored.
    fn insert_mail(
        &mut self,
        row: MailRow,
        meta: &ProductMeta,
        change: &mut ProjectionChange,
    ) {
        if !self.index.contains_key(&RowKey::Mail(row.id.clone())) {
            self.insert(TranscriptRow::Mail(row), meta, change);
        }
    }

    /// Adds an error row once; a replayed duplicate is ignored.
    fn insert_error(
        &mut self,
        row: ErrorRow,
        meta: &ProductMeta,
        change: &mut ProjectionChange,
    ) {
        if !self.index.contains_key(&RowKey::Error(row.id.clone())) {
            self.insert(TranscriptRow::Error(row), meta, change);
        }
    }

    /// Adds the row of a completed compaction; other statuses live in the controls.
    fn apply_compaction(
        &mut self,
        update: &wire::CompactionUpdate,
        change: &mut ProjectionChange,
    ) {
        if update.status.value() != Some(&wire::CompactionStatus::Completed) {
            return;
        }
        let meta = Self::product(update.meta.value());
        let details = update
            .meta
            .value()
            .and_then(|meta| meta.get(ProductIdentity::ACP_NAMESPACE))
            .and_then(|product| product.get("compaction"));
        let text = |pointer: &str| {
            details
                .and_then(|details| details.pointer(pointer))
                .and_then(serde_json::Value::as_str)
        };
        let number = |pointer: &str| {
            text(pointer).and_then(|value| value.parse::<u64>().ok())
        };
        let Some(entry_id) = text("/result/entryId") else {
            return;
        };
        let reason = match text("/reason") {
            Some("threshold") => CompactionReason::Threshold,
            Some("overflow") => CompactionReason::Overflow,
            _ => CompactionReason::Manual,
        };
        let key = RowKey::Compaction(entry_id.to_owned());
        if self.index.contains_key(&key) {
            return;
        }
        let summary = update
            .summary
            .value()
            .map(|blocks| Self::text_of(blocks))
            .unwrap_or_default();
        let elapsed_ms = number("/result/endedAtMs")
            .zip(number("/result/startedAtMs"))
            .map(|(ended, started)| ended.saturating_sub(started));
        self.insert(
            TranscriptRow::Compaction(
                CompactionRow::builder()
                    .entry_id(entry_id.to_owned())
                    .reason(reason)
                    .summary(summary)
                    .tokens_before(number("/result/tokensBefore"))
                    .elapsed_ms(elapsed_ms)
                    .build(),
            ),
            &meta,
            change,
        );
    }

    /// Reads product metadata from an optional `_meta`.
    fn product(meta: Option<&wire::Meta>) -> ProductMeta {
        meta.map(ProductMeta::from).unwrap_or_default()
    }

    /// Joins the text blocks of message content.
    fn text_of(content: &[wire::ContentBlock]) -> String {
        content
            .iter()
            .filter_map(|block| match block {
                wire::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Collects the image blocks of message content.
    fn images_of(content: &[wire::ContentBlock]) -> Vec<ImageBlock> {
        content
            .iter()
            .filter_map(|block| match block {
                wire::ContentBlock::Image(image) => Some(ImageBlock {
                    data: image.data.clone(),
                    mime_type: image.mime_type.0.to_string(),
                }),
                _ => None,
            })
            .collect()
    }

    /// Joins the text of tool content; `None` when there is no text at all.
    fn tool_text(content: &[wire::ToolCallContent]) -> Option<String> {
        let texts: Vec<&str> = content
            .iter()
            .filter_map(|item| match item {
                wire::ToolCallContent::Content(content) => match &content
                    .content
                {
                    wire::ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        (!texts.is_empty()).then(|| texts.concat())
    }

    /// Inserts a row at its sorted position and records it as inserted.
    fn insert(
        &mut self,
        row: TranscriptRow,
        meta: &ProductMeta,
        change: &mut ProjectionChange,
    ) -> usize {
        let sequence = meta.sequence.unwrap_or(self.max_sequence);
        self.max_sequence = self.max_sequence.max(sequence);
        let order = (sequence, self.arrivals);
        let position =
            self.entries.partition_point(|entry| entry.order <= order);
        let key = row.key();
        self.entries.insert(position, Entry { order, row });
        // Every row from the insertion point on moved one slot down.
        for (index, entry) in self.entries.iter().enumerate().skip(position) {
            self.index.insert(entry.row.key(), index);
        }
        change.mark_inserted(key);
        position
    }

    /// Finds or creates the message row with this id.
    fn message_mut(
        &mut self,
        id: &wire::MessageId,
        role: Role,
        meta: &ProductMeta,
        change: &mut ProjectionChange,
    ) -> &mut MessageRow {
        let key = RowKey::Message(id.clone());
        let position = match self.index.get(&key) {
            Some(&position) => {
                change.mark_updated(key);
                position
            }
            None => self.insert(
                TranscriptRow::Message(
                    MessageRow::builder()
                        .message_id(id.clone())
                        .role(role)
                        .turn_id(meta.turn_id.clone())
                        .timestamp_ms(meta.timestamp_ms)
                        .origin(meta.origin.unwrap_or(MessageOrigin::Prompt))
                        .build(),
                ),
                meta,
                change,
            ),
        };
        match self.entries.get_mut(position).map(|entry| &mut entry.row) {
            Some(TranscriptRow::Message(row)) => row,
            // The index maps message keys to message rows only.
            _ => unreachable!("message key {id} indexes a non-message row"),
        }
    }

    /// Finds or creates the tool row with this id; a new row starts at `initial`.
    fn tool_mut(
        &mut self,
        id: &wire::ToolCallId,
        initial: ToolStatus,
        meta: &ProductMeta,
        change: &mut ProjectionChange,
    ) -> &mut ToolRow {
        let key = RowKey::Tool(id.clone());
        let position = match self.index.get(&key) {
            Some(&position) => {
                change.mark_updated(key);
                position
            }
            None => self.insert(
                TranscriptRow::Tool(
                    ToolRow::builder()
                        .tool_call_id(id.clone())
                        .turn_id(meta.turn_id.clone())
                        .title(id.to_string())
                        .status(initial)
                        .timestamp_ms(meta.timestamp_ms)
                        .build(),
                ),
                meta,
                change,
            ),
        };
        match self.entries.get_mut(position).map(|entry| &mut entry.row) {
            Some(TranscriptRow::Tool(row)) => row,
            // The index maps tool keys to tool rows only.
            _ => unreachable!("tool key {id} indexes a non-tool row"),
        }
    }

    /// Appends one streamed chunk to a message's text or thinking.
    fn append_chunk(
        &mut self,
        role: Role,
        chunk: &wire::ContentChunk,
        target: ChunkTarget,
        change: &mut ProjectionChange,
    ) {
        let meta = Self::product(chunk.meta.as_ref());
        let row = self.message_mut(&chunk.message_id, role, &meta, change);
        // A hidden (empty, settled) message becomes visible while it streams.
        if !row.is_visible() {
            change.structural = true;
        }
        row.streaming = true;
        match (&chunk.content, target) {
            (wire::ContentBlock::Text(text), ChunkTarget::Text) => {
                row.text.push_str(&text.text)
            }
            (wire::ContentBlock::Text(text), ChunkTarget::Reasoning) => {
                row.reasoning.push_str(&text.text)
            }
            (wire::ContentBlock::Image(_), ChunkTarget::Text) => {
                row.images.extend(Self::images_of(std::slice::from_ref(
                    &chunk.content,
                )));
            }
            _ => {}
        }
    }

    /// Applies a complete message upsert; defined content replaces what streamed.
    fn upsert_message(
        &mut self,
        role: Role,
        id: &wire::MessageId,
        content: Option<&Vec<wire::ContentBlock>>,
        meta: Option<&wire::Meta>,
        change: &mut ProjectionChange,
    ) {
        let meta = Self::product(meta);
        // A complete message may change visibility, time, or origin.
        change.structural = true;
        let row = self.message_mut(id, role, &meta, change);
        if let Some(content) = content {
            row.text = Self::text_of(content);
            row.images = Self::images_of(content);
        }
        row.streaming = false;
        if let Some((started, ended)) = meta.message_timing {
            row.duration_ms = Some(ended.saturating_sub(started));
        }
        if meta.timestamp_ms.is_some() {
            row.timestamp_ms = meta.timestamp_ms;
        }
        if meta.turn_id.is_some() {
            row.turn_id = meta.turn_id.clone();
        }
        if let Some(origin) = meta.origin {
            row.origin = origin;
        }
        if meta.command.is_some() {
            row.command = meta.command.clone();
        }
        if meta.assistant.is_some() {
            row.assistant = meta.assistant.clone();
        }
    }

    /// Merges the defined fields of a tool call update.
    fn upsert_tool(
        &mut self,
        update: &wire::ToolCallUpdate,
        change: &mut ProjectionChange,
    ) {
        let meta = Self::product(update.meta.value());
        // Status, kind, and title feed the layout's summaries and groups.
        change.structural = true;
        // A row created by an update starts from the update's status, pending by default.
        let row = self.tool_mut(
            &update.tool_call_id,
            ToolStatus::Pending,
            &meta,
            change,
        );
        if let Some(name) = update.name.value() {
            row.name = Some(name.clone());
        }
        if let Some(title) = update.title.value() {
            row.title = title.clone();
        }
        if let Some(kind) = update.kind.value() {
            row.kind = Some(kind.clone());
        }
        if let Some(raw_input) = update.raw_input.value() {
            row.raw_input = Some(raw_input.clone());
        }
        if let Some(status) = update.status.value() {
            let blocked = meta.blocked
                || update
                    .raw_output
                    .value()
                    .and_then(|output| output.pointer("/details/type"))
                    .and_then(serde_json::Value::as_str)
                    == Some("blocked");
            row.status = match status {
                wire::ToolCallStatus::InProgress => ToolStatus::Running,
                wire::ToolCallStatus::Completed => ToolStatus::Completed,
                wire::ToolCallStatus::Failed if blocked => ToolStatus::Blocked,
                wire::ToolCallStatus::Failed => ToolStatus::Failed,
                wire::ToolCallStatus::Cancelled => ToolStatus::Cancelled,
                // Pending, paused, and statuses newer than this client all wait.
                _ => ToolStatus::Pending,
            };
            // The child session exists once its spawn call has finished.
            change.agents |= row.status == ToolStatus::Completed
                && row.name.as_deref() == Some(protocol::SPAWN_AGENT_TOOL);
        }
        if let Some(content) = update.content.value() {
            let terminal = content
                .iter()
                .any(|item| matches!(item, wire::ToolCallContent::Terminal(_)));
            // A terminal's output arrives through terminal snapshots; text next
            // to it (such as "Full output: <path>") is a footer, not the output.
            if terminal {
                row.footer = Self::tool_text(content);
            } else if let Some(text) = Self::tool_text(content) {
                row.output = text;
            }
            let patch = content.iter().find_map(|item| match item {
                wire::ToolCallContent::Diff(diff) => {
                    diff.patch.as_ref().map(|patch| patch.text.clone())
                }
                _ => None,
            });
            if patch.is_some() {
                row.patch = patch;
            }
        }
    }

    /// Tracks run state; going idle ends every stream still marked as open.
    fn apply_state(
        &mut self,
        state: &wire::StateUpdate,
        change: &mut ProjectionChange,
    ) {
        let next = match state {
            wire::StateUpdate::Running(running) => {
                // Only a run starting from idle restarts the clock; resuming
                // after an approval or a question continues the same run.
                if self.run_state == RunState::Idle {
                    // A run begins at the update's own time, or now when it has none.
                    self.run_started_ms = Self::product(running.meta.as_ref())
                        .timestamp_ms
                        .or_else(|| Some(crate::now_ms()));
                }
                RunState::Running
            }
            wire::StateUpdate::Idle(_) => RunState::Idle,
            wire::StateUpdate::RequiresAction(_) => RunState::RequiresAction,
            _ => return,
        };
        if next == RunState::Idle {
            for entry in &mut self.entries {
                if let TranscriptRow::Message(row) = &mut entry.row
                    && row.streaming
                {
                    // An empty message disappears once it stops streaming.
                    row.streaming = false;
                    change.structural = true;
                    change
                        .mark_updated(RowKey::Message(row.message_id.clone()));
                }
            }
            self.run_started_ms = None;
        }
        if next != self.run_state {
            self.run_state = next;
            change.run_state = true;
            // Finished runs fold their tools, live ones do not.
            change.structural = true;
        }
    }
}
