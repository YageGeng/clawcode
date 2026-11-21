//! Session-wide control state: configuration options, slash commands, context
//! usage, plan and todos, retries, compaction status, open questions, and
//! pending agent requests.
//!
//! Mirrors the WebUI decoder (`web/src/workspace/updateDecoder.ts`) for these
//! updates; see `docs/superpowers/plans/2026-11-20-desktop-m2-protocol-reference.md`.

use std::collections::HashSet;

use agent_client_protocol::v2::schema as wire;
use protocol::{AgentEventPayload, ProductIdentity, QuestionReply};
use serde::Deserialize as _;
use serde_json::Value;
use typed_builder::TypedBuilder;

use super::events::ProductEvent;
use crate::backend::{AgentRequest, QuestionKey, RequestId};

/// One choice of a select option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigChoice {
    /// Wire value sent back when chosen.
    pub value: String,
    /// Display name.
    pub name: String,
    /// Optional explanation, such as the model's context size.
    pub description: Option<String>,
}

/// One select-type configuration option, such as the model.
#[derive(Debug, Clone, PartialEq, Eq, TypedBuilder)]
pub struct ConfigOption {
    /// Option id: `model`, `thinking_level`, or `mode`.
    pub id: String,
    /// Display name of the option.
    pub name: String,
    /// The value in effect.
    pub current: String,
    /// Every offered choice, groups flattened.
    pub choices: Vec<ConfigChoice>,
}

impl ConfigOption {
    /// The choice in effect, if it is among the offered ones.
    pub fn current_choice(&self) -> Option<&ConfigChoice> {
        self.choices
            .iter()
            .find(|choice| choice.value == self.current)
    }
}

impl TryFrom<&wire::SessionConfigOption> for ConfigOption {
    type Error = ();

    /// Accepts select options only; grouped choices are flattened in order.
    fn try_from(option: &wire::SessionConfigOption) -> Result<Self, ()> {
        let wire::SessionConfigKind::Select(select) = &option.kind else {
            return Err(());
        };
        let choice = |choice: &wire::SessionConfigSelectOption| ConfigChoice {
            value: choice.value.to_string(),
            name: choice.name.clone(),
            description: choice.description.clone(),
        };
        let choices = match &select.options {
            wire::SessionConfigSelectOptions::Ungrouped(options) => {
                options.iter().map(choice).collect()
            }
            wire::SessionConfigSelectOptions::Grouped(groups) => groups
                .iter()
                .flat_map(|group| group.options.iter().map(choice))
                .collect(),
            _ => Vec::new(),
        };
        Ok(ConfigOption::builder()
            .id(option.config_id.to_string())
            .name(option.name.clone())
            .current(select.current_value.to_string())
            .choices(choices)
            .build())
    }
}

/// Where a slash command comes from, as the kernel's `SlashCommandSource` serializes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSource {
    /// Built into the kernel.
    Builtin,
    /// Registered by an extension.
    Extension,
    /// A skill.
    Skill,
    /// A prompt template.
    #[serde(rename = "prompt_template")]
    Template,
}

/// One slash command the session offers.
#[derive(Debug, Clone, PartialEq, Eq, TypedBuilder)]
pub struct SlashCommand {
    /// Name without the leading slash.
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Argument hint, when the command takes input.
    #[builder(default)]
    pub hint: Option<String>,
    /// Origin, when the agent reported a known one.
    #[builder(default)]
    pub source: Option<CommandSource>,
    /// Full name of a short alias.
    #[builder(default)]
    pub qualified_name: Option<String>,
}

impl SlashCommand {
    /// Reads one advertised command; entries that are not commands are `None`.
    fn from_wire(command: &wire::AvailableCommand) -> Option<Self> {
        let wire::AvailableCommand::Command(command) = command else {
            return None;
        };
        // `_meta.clawcode.slashCommand` holds the source and the qualified name.
        let product = command
            .meta
            .as_ref()
            .and_then(|meta| meta.get(ProductIdentity::ACP_NAMESPACE))
            .and_then(|product| product.get("slashCommand"));
        let field = |key: &str| product.and_then(|product| product.get(key));
        Some(
            SlashCommand::builder()
                .name(command.name.clone())
                .description(command.description.clone())
                .hint(match &command.input {
                    Some(wire::AvailableCommandInput::Text(input)) => {
                        Some(input.hint.clone())
                    }
                    _ => None,
                })
                .source(
                    field("source").and_then(|source| {
                        CommandSource::deserialize(source).ok()
                    }),
                )
                .qualified_name(
                    field("qualifiedName")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                )
                .build(),
        )
    }
}

/// Token counts of one model call or a sum of them; unreported counters are `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, TypedBuilder)]
pub struct ModelUsage {
    /// Prompt tokens.
    #[builder(default)]
    pub input: Option<u64>,
    /// Completion tokens.
    #[builder(default)]
    pub output: Option<u64>,
    /// Prompt tokens served from cache.
    #[builder(default)]
    pub cache_read: Option<u64>,
    /// Prompt tokens written to cache.
    #[builder(default)]
    pub cache_write: Option<u64>,
    /// Reasoning tokens.
    #[builder(default)]
    pub reasoning: Option<u64>,
    /// Total tokens.
    #[builder(default)]
    pub total: Option<u64>,
}

impl TryFrom<&Value> for ModelUsage {
    type Error = ();

    /// Reads the snake_case decimal-string counters; one malformed counter rejects the whole usage.
    fn try_from(value: &Value) -> Result<Self, ()> {
        let object = value.as_object().ok_or(())?;
        let count = |key: &str| -> Result<Option<u64>, ()> {
            match object.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(text)) => {
                    text.parse::<u64>().map(Some).map_err(|_malformed| ())
                }
                Some(_) => Err(()),
            }
        };
        Ok(ModelUsage::builder()
            .input(count("input_tokens")?)
            .output(count("output_tokens")?)
            .cache_read(count("cache_read_tokens")?)
            .cache_write(count("cache_write_tokens")?)
            .reasoning(count("reasoning_tokens")?)
            .total(count("total_tokens")?)
            .build())
    }
}

impl ModelUsage {
    /// Tokens the context holds: the total, else input plus output.
    pub fn context_tokens(&self) -> Option<u64> {
        self.total.or_else(|| {
            self.input
                .map(|input| input.saturating_add(self.output.unwrap_or(0)))
        })
    }

    /// Cache hits per mille of the input, capped at 1000; unknown without both counters.
    pub fn cache_hit_permille(&self) -> Option<u64> {
        match (self.input, self.cache_read) {
            (Some(input), Some(read)) if input > 0 => {
                Some((read.saturating_mul(1000) / input).min(1000))
            }
            _ => None,
        }
    }

    /// Counter-wise sum; a counter stays unknown only when both sides lack it.
    pub fn sum(&self, other: &ModelUsage) -> ModelUsage {
        let add = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0))),
        };
        ModelUsage::builder()
            .input(add(self.input, other.input))
            .output(add(self.output, other.output))
            .cache_read(add(self.cache_read, other.cache_read))
            .cache_write(add(self.cache_write, other.cache_write))
            .reasoning(add(self.reasoning, other.reasoning))
            .total(add(self.total, other.total))
            .build()
    }
}

/// How full the model context is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextUsage {
    /// Tokens in the context, when known.
    pub used: Option<u64>,
    /// Context window size in tokens.
    pub size: u64,
    /// The exact counters the agent reported, when they were well-formed.
    pub exact: Option<ModelUsage>,
}

impl ContextUsage {
    /// Fill level in tenths of a percent, capped at 1000; unknown without a size or usage.
    pub fn percent_tenths(&self) -> Option<u64> {
        let used = self.used?;
        (self.size > 0)
            .then(|| (used.saturating_mul(1000) / self.size).min(1000))
    }
}

/// Status of one todo item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    /// Not started.
    Pending,
    /// Being worked on.
    InProgress,
    /// Done.
    Completed,
}

/// One item of the agent's todo list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Todo {
    /// What the item says; the active form while in progress.
    pub content: String,
    /// Where it stands.
    pub status: TodoStatus,
}

/// Retry state of the current run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryState {
    /// Waiting before the next attempt.
    Waiting {
        /// Attempt about to start.
        attempt: u32,
        /// Attempts allowed.
        max: u32,
        /// When the attempt starts, in Unix milliseconds.
        due_ms: u64,
        /// Why the previous attempt failed.
        error: String,
    },
    /// An attempt is running.
    Running {
        /// The running attempt.
        attempt: u32,
        /// Attempts allowed.
        max: u32,
    },
    /// Retrying gave up.
    Failed {
        /// The last attempt.
        attempt: u32,
        /// The final error, when reported.
        error: Option<String>,
    },
}

/// Progress of a context compaction outside its finished transcript row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionStatus {
    /// Compacting now.
    Running,
    /// Compaction failed with this message.
    Failed(String),
    /// Compaction was cancelled.
    Cancelled,
}

/// One question the agent asked without waiting.
#[derive(Debug, Clone, PartialEq, Eq, TypedBuilder)]
pub struct AskedQuestion {
    /// Identity used to answer or skip it.
    pub key: QuestionKey,
    /// The tool call that asked it.
    pub tool_call_id: wire::ToolCallId,
    /// The question, as Markdown.
    pub title: String,
    /// Offered answers; empty for free text only.
    pub options: Vec<String>,
    /// Whether several options may be picked.
    pub multi_select: bool,
}

/// Session-wide control state, projected from updates and agent requests.
#[derive(Debug, Default, TypedBuilder)]
pub struct SessionControls {
    #[builder(default)]
    config: Vec<ConfigOption>,
    #[builder(default)]
    commands: Vec<SlashCommand>,
    #[builder(default)]
    context: Option<ContextUsage>,
    #[builder(default)]
    todos: Vec<Todo>,
    #[builder(default)]
    approved_plan: Option<String>,
    /// Whether the plan panel should be open: a live run published plan content.
    #[builder(default)]
    plan_open: bool,
    #[builder(default)]
    retry: Option<RetryState>,
    #[builder(default)]
    compaction: Option<CompactionStatus>,
    /// Every question asked so far, in order.
    #[builder(default)]
    asked: Vec<AskedQuestion>,
    /// Questions a `question_reply` record settled.
    #[builder(default)]
    settled: HashSet<QuestionKey>,
    /// Questions this client is answering or skipping right now.
    #[builder(default)]
    settling: HashSet<QuestionKey>,
    /// Questions answered by messages still waiting in the queue.
    #[builder(default)]
    queued: HashSet<QuestionKey>,
    /// Agent requests waiting for the user, in arrival order.
    #[builder(default)]
    requests: Vec<AgentRequest>,
}

impl SessionControls {
    /// The option with this id.
    pub fn config(&self, id: &str) -> Option<&ConfigOption> {
        self.config.iter().find(|option| option.id == id)
    }

    /// Slash commands, in the agent's order.
    pub fn commands(&self) -> &[SlashCommand] {
        &self.commands
    }

    /// Context fill, when reported since the last compaction.
    pub fn context(&self) -> Option<&ContextUsage> {
        self.context.as_ref()
    }

    /// The agent's todo list.
    pub fn todos(&self) -> &[Todo] {
        &self.todos
    }

    /// The most recently approved plan.
    pub fn approved_plan(&self) -> Option<&str> {
        self.approved_plan.as_deref()
    }

    /// Whether the plan panel opens on its own.
    pub fn plan_open(&self) -> bool {
        self.plan_open
    }

    /// Retry state of the current run.
    pub fn retry(&self) -> Option<&RetryState> {
        self.retry.as_ref()
    }

    /// A running, failed, or cancelled compaction.
    pub fn compaction(&self) -> Option<&CompactionStatus> {
        self.compaction.as_ref()
    }

    /// Questions still waiting: not settled, not being answered, and not
    /// answered by a message waiting in the queue.
    pub fn open_questions(&self) -> Vec<&AskedQuestion> {
        self.asked
            .iter()
            .filter(|question| {
                !self.settled.contains(&question.key)
                    && !self.settling.contains(&question.key)
                    && !self.queued.contains(&question.key)
            })
            .collect()
    }

    /// Records which questions the queued messages answer, once per queue change.
    pub fn set_queued(&mut self, pending: &protocol::PendingMessages) {
        self.queued = pending
            .steering
            .iter()
            .chain(&pending.follow_up)
            .filter_map(|item| {
                QuestionReply::try_from(&item.message.content).ok()
            })
            .flat_map(|reply| reply.keys().collect::<Vec<_>>())
            .collect();
    }

    /// Agent requests waiting for the user.
    pub fn requests(&self) -> &[AgentRequest] {
        &self.requests
    }

    /// Replaces the options with the agent's complete list.
    pub fn set_config(&mut self, options: Vec<wire::SessionConfigOption>) {
        self.config = options
            .iter()
            .filter_map(|option| ConfigOption::try_from(option).ok())
            .collect();
    }

    /// Records a request waiting for the user.
    pub fn push_request(&mut self, request: AgentRequest) {
        self.requests.push(request);
    }

    /// Removes every pending request, for declining them when no view shows the session.
    pub fn take_requests(&mut self) -> Vec<AgentRequest> {
        std::mem::take(&mut self.requests)
    }

    /// Drops an answered or withdrawn request; returns whether it was here.
    pub fn remove_request(&mut self, id: RequestId) -> bool {
        let before = self.requests.len();
        self.requests.retain(|request| request.id() != id);
        self.requests.len() != before
    }

    /// Hides questions while this client answers or skips them.
    pub fn mark_settling(
        &mut self,
        keys: impl IntoIterator<Item = QuestionKey>,
    ) {
        self.settling.extend(keys);
    }

    /// Shows questions again after an answer or skip failed.
    pub fn unmark_settling(&mut self, keys: &[QuestionKey]) {
        for key in keys {
            self.settling.remove(key);
        }
    }

    /// Folds one update into the control state; returns whether anything changed.
    ///
    /// `event` is the kernel event `update` carries, decoded once by the projection.
    pub fn apply(
        &mut self,
        update: &wire::SessionUpdate,
        event: Option<&ProductEvent>,
    ) -> bool {
        match update {
            wire::SessionUpdate::ConfigOptionUpdate(update) => {
                self.set_config(update.config_options.clone());
                true
            }
            wire::SessionUpdate::AvailableCommandsUpdate(update) => {
                self.commands = update
                    .available_commands
                    .iter()
                    .filter_map(SlashCommand::from_wire)
                    .collect();
                true
            }
            wire::SessionUpdate::UsageUpdate(usage) => {
                let product = Self::product(usage.meta.as_ref());
                self.set_context(
                    product.and_then(|product| product.get("usage")),
                    product
                        .and_then(|product| product.get("contextWindow"))
                        .and_then(Self::number)
                        .unwrap_or(usage.size),
                );
                true
            }
            wire::SessionUpdate::PlanUpdate(update) => self.apply_plan(update),
            wire::SessionUpdate::CompactionUpdate(update) => {
                self.apply_compaction(update)
            }
            wire::SessionUpdate::ToolCallUpdate(update) => {
                self.apply_questions(update)
            }
            wire::SessionUpdate::StateUpdate(wire::StateUpdate::Running(_)) => {
                // A new run starts fresh: the previous run's failures stop showing.
                let retry_failed =
                    matches!(self.retry, Some(RetryState::Failed { .. }));
                if retry_failed {
                    self.retry = None;
                }
                let compaction_ended = matches!(
                    self.compaction,
                    Some(
                        CompactionStatus::Failed(_)
                            | CompactionStatus::Cancelled
                    )
                );
                if compaction_ended {
                    self.compaction = None;
                }
                retry_failed || compaction_ended
            }
            wire::SessionUpdate::StateUpdate(wire::StateUpdate::Idle(_)) => {
                // A finished run ends retries, in-flight question settling, and the open plan.
                let retry_changed = matches!(
                    self.retry,
                    Some(
                        RetryState::Waiting { .. } | RetryState::Running { .. }
                    )
                );
                if retry_changed {
                    self.retry = None;
                }
                let changed = retry_changed
                    || !self.settling.is_empty()
                    || self.plan_open
                    || self.compaction == Some(CompactionStatus::Running);
                self.settling.clear();
                self.plan_open = false;
                if self.compaction == Some(CompactionStatus::Running) {
                    self.compaction = None;
                }
                changed
            }
            wire::SessionUpdate::Other(other) => match event {
                Some(event) => self.apply_event(event),
                None => self.apply_usage_event(other),
            },
            _ => false,
        }
    }

    /// Reads the `clawcode` product metadata.
    fn product(meta: Option<&wire::Meta>) -> Option<&Value> {
        meta.and_then(|meta| meta.get(ProductIdentity::ACP_NAMESPACE))
    }

    /// Reads a counter sent as a decimal string or a number.
    fn number(value: &Value) -> Option<u64> {
        match value {
            Value::String(text) => text.parse().ok(),
            other => other.as_u64(),
        }
    }

    /// Replaces the context fill from reported counters and the window size.
    fn set_context(&mut self, usage: Option<&Value>, size: u64) {
        let exact = usage.and_then(|usage| ModelUsage::try_from(usage).ok());
        self.context = Some(ContextUsage {
            used: exact.as_ref().and_then(ModelUsage::context_tokens),
            size,
            exact,
        });
    }

    /// Replaces the todo list or the approved plan.
    fn apply_plan(&mut self, update: &wire::PlanUpdate) -> bool {
        match &update.plan {
            wire::PlanUpdateContent::Items(items)
                if &*items.plan_id.0 == "todos" =>
            {
                self.todos = items
                    .entries
                    .iter()
                    .map(|entry| Todo {
                        content: entry.content.clone(),
                        status: match entry.status {
                            wire::PlanEntryStatus::InProgress => {
                                TodoStatus::InProgress
                            }
                            wire::PlanEntryStatus::Completed => {
                                TodoStatus::Completed
                            }
                            _ => TodoStatus::Pending,
                        },
                    })
                    .collect();
            }
            wire::PlanUpdateContent::Markdown(plan)
                if &*plan.plan_id.0 == "plan-mode" =>
            {
                self.approved_plan = Some(plan.content.clone());
            }
            _ => return false,
        }
        self.plan_open = true;
        true
    }

    /// Tracks the compaction status; a completed one empties the context fill.
    fn apply_compaction(&mut self, update: &wire::CompactionUpdate) -> bool {
        self.compaction = match update.status {
            wire::CompactionStatus::InProgress => {
                Some(CompactionStatus::Running)
            }
            wire::CompactionStatus::Completed => {
                self.context = None;
                None
            }
            wire::CompactionStatus::Failed => Some(CompactionStatus::Failed(
                update.error.value().cloned().unwrap_or_default(),
            )),
            wire::CompactionStatus::Cancelled => {
                Some(CompactionStatus::Cancelled)
            }
            _ => return false,
        };
        true
    }

    /// Opens the questions a persisted `request_user_input_async` result carries.
    fn apply_questions(&mut self, update: &wire::ToolCallUpdate) -> bool {
        // The persisted result message, not the live tool end, opens questions:
        // only then can the kernel accept an answer or a skip.
        let Some(details) = update
            .raw_output
            .value()
            .and_then(|output| output.pointer("/content/details"))
        else {
            return false;
        };
        let Ok(protocol::ToolResultDetails::UserQuestions {
            request_id,
            questions,
        }) = protocol::ToolResultDetails::deserialize(details)
        else {
            return false;
        };
        if self
            .asked
            .iter()
            .any(|question| question.key.request_id == request_id)
        {
            return false;
        }
        for (index, question) in questions.into_iter().enumerate() {
            self.asked.push(
                AskedQuestion::builder()
                    .key(QuestionKey {
                        request_id: request_id.clone(),
                        index,
                    })
                    .tool_call_id(update.tool_call_id.clone())
                    .title(question.title)
                    .options(question.options.unwrap_or_default())
                    .multi_select(question.multi_select)
                    .build(),
            );
        }
        true
    }

    /// Reads the context fill a `usage_updated` event reports.
    ///
    /// The server re-encodes these counters as decimal strings, so this event
    /// is read from its raw payload instead of the typed one.
    fn apply_usage_event(&mut self, update: &wire::OtherSessionUpdate) -> bool {
        let Some(payload) = update.fields.get("payload").filter(|payload| {
            payload.get("event").and_then(Value::as_str)
                == Some("usage_updated")
        }) else {
            return false;
        };
        let size = payload
            .get("context_window")
            .and_then(Self::number)
            .unwrap_or_default();
        self.set_context(payload.get("usage"), size);
        true
    }

    /// Reads kernel events: retries and question settlements.
    fn apply_event(&mut self, event: &ProductEvent) -> bool {
        match &event.payload {
            AgentEventPayload::RetryScheduled {
                attempt,
                max_attempts,
                delay_ms,
                error,
                ..
            } => {
                let Some(at) = event.meta.timestamp_ms else {
                    return false;
                };
                self.retry = Some(RetryState::Waiting {
                    attempt: *attempt,
                    max: *max_attempts,
                    due_ms: at.saturating_add(*delay_ms),
                    error: error.clone(),
                });
                true
            }
            AgentEventPayload::RetryStart {
                attempt,
                max_attempts,
                ..
            } => {
                self.retry = Some(RetryState::Running {
                    attempt: *attempt,
                    max: *max_attempts,
                });
                true
            }
            AgentEventPayload::RetryEnd {
                attempt,
                success,
                final_error,
                ..
            } => {
                self.retry = (!success).then(|| RetryState::Failed {
                    attempt: *attempt,
                    error: final_error.clone(),
                });
                true
            }
            AgentEventPayload::MessageEnd { message } => {
                let Ok(reply) = QuestionReply::try_from(&message.content)
                else {
                    return false;
                };
                for key in reply.keys() {
                    self.settling.remove(&key);
                    self.settled.insert(key);
                }
                true
            }
            _ => false,
        }
    }
}
