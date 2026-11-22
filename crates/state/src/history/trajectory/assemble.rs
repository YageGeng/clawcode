//! Turns the entries of one page window into typed trajectory records.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;

use protocol::{
    AgentMessage, AssistantMetadata, AssistantRecord, CommandKind,
    CommandRecord, ContentBlock, ContentHash, EntryId, ExtensionRecord,
    MessageContent, MessageTiming, RequestRecord, RequestSnapshot, RunRef,
    SettingChange, StopReason, ThinkingLevel, TimestampMs, ToolCallId,
    ToolRecord, TrajectoryRecord, TrajectoryRecordKind, TurnId, UserOrigin,
    UserRecord,
};
use serde::Deserialize as _;
use store::{EntryKind, SessionEntry};

use super::branch::BranchIndex;
use crate::StateError;

/// Text and attachment counts of a content block list.
trait BlockText {
    /// Text blocks joined by newlines.
    fn joined_text(&self) -> String;
    /// Image blocks.
    fn image_count(&self) -> u32;
    /// Embedded resources and resource links.
    fn file_count(&self) -> u32;
}

impl BlockText for [ContentBlock] {
    /// Joins the text blocks, leaving images and other data out.
    fn joined_text(&self) -> String {
        self.iter()
            .filter_map(ContentBlock::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Counts image blocks without reading their data.
    fn image_count(&self) -> u32 {
        count(self, |block| matches!(block, ContentBlock::Image { .. }))
    }

    /// Counts attached files.
    fn file_count(&self) -> u32 {
        count(self, |block| {
            matches!(
                block,
                ContentBlock::EmbeddedResource { .. }
                    | ContentBlock::ResourceLink { .. }
            )
        })
    }
}

/// Counts the blocks matching `predicate`, saturating at `u32::MAX`.
fn count(
    blocks: &[ContentBlock],
    predicate: impl Fn(&ContentBlock) -> bool,
) -> u32 {
    u32::try_from(blocks.iter().filter(|block| predicate(block)).count())
        .unwrap_or(u32::MAX)
}

/// Where a record sits: its entry, the entry's sequence, Run, and Turn.
#[derive(typed_builder::TypedBuilder)]
struct Place {
    entry_id: EntryId,
    sequence: u64,
    #[builder(default)]
    run: Option<RunRef>,
    /// Set from the message once it is read.
    #[builder(default)]
    turn_id: Option<TurnId>,
}

impl Place {
    /// Builds a record at this place.
    fn record(
        &self,
        id: String,
        (started, ended): (Option<TimestampMs>, Option<TimestampMs>),
        kind: TrajectoryRecordKind,
    ) -> TrajectoryRecord {
        TrajectoryRecord::builder()
            .id(id)
            .sequence(self.sequence)
            .run(self.run.clone())
            .turn_id(self.turn_id.clone())
            .started_at_ms(started)
            .ended_at_ms(ended)
            .kind(kind)
            .build()
    }
}

/// What a window entry holds, parsed once.
pub(super) enum EntryBody {
    /// A message entry.
    Message(Box<AgentMessage>),
    /// Any other entry kind with its raw payload.
    Other(EntryKind, serde_json::Value),
}

/// One window entry, parsed once and read by both the page query and the
/// assembler.
pub(super) struct PageEntry {
    id: EntryId,
    timestamp_ms: TimestampMs,
    body: EntryBody,
}

impl TryFrom<SessionEntry> for PageEntry {
    type Error = StateError;

    /// Parses a message entry; the provider-native replay payload is never
    /// read, so it is dropped before parsing.
    fn try_from(entry: SessionEntry) -> Result<Self, Self::Error> {
        let mut payload = serde_json::Value::Object(entry.payload);
        let body = match entry.kind {
            EntryKind::Message => {
                if let Some(content) = payload
                    .get_mut("content")
                    .and_then(serde_json::Value::as_object_mut)
                {
                    content.remove("model_message");
                }
                EntryBody::Message(Box::new(AgentMessage::deserialize(
                    payload,
                )?))
            }
            kind => EntryBody::Other(kind, payload),
        };
        Ok(Self {
            id: entry.id,
            timestamp_ms: entry.timestamp_ms,
            body,
        })
    }
}

impl PageEntry {
    /// The request snapshot of an assistant entry.
    pub(super) fn request(&self) -> Option<&RequestSnapshot> {
        match &self.body {
            EntryBody::Message(message) => match &message.content {
                MessageContent::Assistant { metadata, .. } => {
                    metadata.request.as_deref()
                }
                _ => None,
            },
            EntryBody::Other(..) => None,
        }
    }
}

/// Assembles the records of one page window, in branch order.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct TrajectoryAssembler<'a> {
    /// The branch the window is cut from.
    branch: &'a BranchIndex,
    /// Positions of the page on the branch.
    window: Range<usize>,
    /// How queued user messages entered the Session.
    origins: &'a HashMap<EntryId, UserOrigin>,
    /// Tool names by catalog hash, for the catalogs that change.
    tool_names: &'a HashMap<ContentHash, Vec<String>>,
    /// The snapshot of the request before the next one assembled.
    previous: Option<RequestSnapshot>,
    /// Records assembled so far.
    #[builder(default)]
    records: Vec<TrajectoryRecord>,
    /// Positions in `records` of tool calls still waiting for their result.
    #[builder(default)]
    pending: HashMap<ToolCallId, usize>,
}

impl TrajectoryAssembler<'_> {
    /// Assembles `entries`, the window's entries in branch order.
    pub(super) fn assemble(
        mut self,
        entries: Vec<PageEntry>,
    ) -> Vec<TrajectoryRecord> {
        let branch = self.branch;
        let in_window = branch
            .entries()
            .get(self.window.clone())
            .unwrap_or_default();
        let positions: HashMap<&EntryId, usize> = in_window
            .iter()
            .enumerate()
            .map(|(offset, entry)| (&entry.id, self.window.start + offset))
            .collect();
        for entry in entries {
            let Some(&position) = positions.get(&entry.id) else {
                continue;
            };
            let Some(branch_entry) = branch.entries().get(position) else {
                continue;
            };
            let run = branch_entry.run_id.as_ref().and_then(|id| {
                Some(RunRef {
                    id: id.clone(),
                    index: branch.run_index(id)?,
                })
            });
            let place = Place::builder()
                .entry_id(entry.id.clone())
                .sequence(branch_entry.sequence)
                .run(run)
                .build();
            if let Some(record) = self.entry(entry, place, position) {
                self.records.push(record);
            }
        }
        self.records
    }

    /// The record of one entry; message entries may add or complete others.
    fn entry(
        &mut self,
        entry: PageEntry,
        place: Place,
        position: usize,
    ) -> Option<TrajectoryRecord> {
        let payload = match entry.body {
            EntryBody::Message(message) => {
                return self.message(*message, place, position);
            }
            EntryBody::Other(kind, payload) => (kind, payload),
        };
        let kind = match payload {
            // Only the summary and the count show; the retained messages,
            // whole images included, stay unread.
            (EntryKind::Compaction, payload) => {
                TrajectoryRecordKind::Compaction {
                    summary: text_at(&payload, "/summary")?,
                    tokens_before: payload.pointer("/tokensBefore").and_then(
                        |count| match count {
                            // Counts travel as decimal strings.
                            serde_json::Value::String(text) => {
                                text.parse().ok()
                            }
                            other => other.as_u64(),
                        },
                    ),
                }
            }
            (EntryKind::BranchSummary, payload) => {
                TrajectoryRecordKind::BranchSummary {
                    summary: text_at(&payload, "/summary").unwrap_or_default(),
                }
            }
            (EntryKind::ModelChange, payload) => {
                TrajectoryRecordKind::Setting(SettingChange::Model {
                    provider: text_at(&payload, "/provider")
                        .unwrap_or_default(),
                    model: text_at(&payload, "/modelId").unwrap_or_default(),
                })
            }
            (EntryKind::ThinkingLevelChange, payload) => {
                let level = payload.get("thinkingLevel")?;
                TrajectoryRecordKind::Setting(SettingChange::ThinkingLevel {
                    level: ThinkingLevel::deserialize(level).ok()?,
                })
            }
            (EntryKind::ActiveToolsChange, payload) => {
                let tools = payload
                    .get("activeTools")
                    .and_then(|tools| Vec::<String>::deserialize(tools).ok())
                    .unwrap_or_default();
                TrajectoryRecordKind::Setting(SettingChange::ActiveTools {
                    tools,
                })
            }
            // Custom entries are extension state; only plain text shows.
            (EntryKind::Custom, payload) => {
                let text = text_at(&payload, "/data")
                    .or_else(|| text_at(&payload, "/data/text"))?;
                TrajectoryRecordKind::Extension(ExtensionRecord {
                    extension_id: text_at(&payload, "/extensionId")
                        .unwrap_or_default(),
                    custom_type: text_at(&payload, "/customType")
                        .unwrap_or_default(),
                    text,
                })
            }
            (EntryKind::Message, _) => return None,
        };
        Some(place.record(
            entry.id.to_string(),
            (Some(entry.timestamp_ms), None),
            kind,
        ))
    }

    /// The record of one message; an assistant message adds its request and
    /// calls, a tool result completes its call.
    fn message(
        &mut self,
        message: AgentMessage,
        mut place: Place,
        position: usize,
    ) -> Option<TrajectoryRecord> {
        place.turn_id = Some(message.identity.turn_id.clone());
        let entry_id = &place.entry_id;
        let timing = message.timing;
        let span = (Some(timing.timestamp_ms), Some(timing.ended_at_ms));
        let user = |text, blocks: &[ContentBlock], origin| {
            UserRecord::builder()
                .text(text)
                .image_count(blocks.image_count())
                .file_count(blocks.file_count())
                .origin(origin)
                .build()
        };
        let origin = self.origins.get(entry_id).copied();
        let kind = match message.content {
            MessageContent::User { blocks } => TrajectoryRecordKind::User(
                user(blocks.joined_text(), &blocks, origin.unwrap_or_default()),
            ),
            MessageContent::ExpandedUser { expansion } => {
                TrajectoryRecordKind::User(user(
                    expansion.invocation.original.clone(),
                    &expansion.model_blocks,
                    origin.unwrap_or(UserOrigin::Skill),
                ))
            }
            MessageContent::SlashCommand { message } => {
                TrajectoryRecordKind::Command(CommandRecord {
                    kind: CommandKind::Slash,
                    text: message.text_content().unwrap_or_default(),
                })
            }
            MessageContent::BashExecution { bash } => {
                TrajectoryRecordKind::Command(CommandRecord {
                    kind: CommandKind::Bash,
                    text: bash.command,
                })
            }
            MessageContent::Assistant {
                blocks, metadata, ..
            } => {
                self.assistant(&blocks, metadata, timing, &place, position);
                return None;
            }
            MessageContent::ToolResult {
                tool_call_id,
                blocks,
                is_error,
                cancelled,
                ..
            } => {
                self.tool_result(
                    &tool_call_id,
                    &blocks,
                    is_error,
                    cancelled,
                    timing,
                );
                return None;
            }
            MessageContent::Extension { extension } => {
                TrajectoryRecordKind::Extension(ExtensionRecord {
                    extension_id: extension.extension_id.to_string(),
                    custom_type: extension.custom_type,
                    text: extension.blocks.joined_text(),
                })
            }
            MessageContent::CompactionSummary { compaction } => {
                TrajectoryRecordKind::Compaction {
                    summary: compaction.summary,
                    tokens_before: Some(compaction.tokens_before),
                }
            }
            MessageContent::BranchSummary { branch } => {
                TrajectoryRecordKind::BranchSummary {
                    summary: branch.summary,
                }
            }
            MessageContent::System { .. } => return None,
        };
        Some(place.record(entry_id.to_string(), span, kind))
    }

    /// Adds one model request: its request record when it carries a
    /// snapshot, its output, and one pending record per tool call.
    fn assistant(
        &mut self,
        blocks: &[ContentBlock],
        metadata: AssistantMetadata,
        timing: MessageTiming,
        place: &Place,
        position: usize,
    ) {
        let entry_id = &place.entry_id;
        let span = (Some(timing.timestamp_ms), Some(timing.ended_at_ms));
        let index = self.branch.request_index(position);
        if let Some(snapshot) = metadata.request {
            let request = self.request(index, *snapshot);
            self.records.push(place.record(
                format!("request:{entry_id}"),
                span,
                TrajectoryRecordKind::Request(request),
            ));
        }
        let reasoning = blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Reasoning { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let calls: Vec<ToolRecord> = blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolCall {
                    tool_call_id,
                    name,
                    arguments,
                    ..
                } => Some(
                    ToolRecord::builder()
                        .call_id(tool_call_id.clone())
                        .name(name.clone())
                        .arguments(arguments.clone())
                        .build(),
                ),
                _ => None,
            })
            .collect();
        let assistant = AssistantRecord::builder()
            .request_index(index)
            .provider_id(metadata.provider_id)
            .model_id(metadata.model_id)
            .reasoning(reasoning)
            .text(blocks.joined_text())
            .tool_call_ids(
                calls.iter().map(|call| call.call_id.clone()).collect(),
            )
            .usage(metadata.usage)
            .requested_at_ms(timing.timestamp_ms)
            .first_token_at_ms(timing.started_at_ms)
            .last_token_at_ms(timing.ended_at_ms)
            .stop_reason(metadata.stop_reason)
            .error(metadata.error)
            .cancelled(metadata.stop_reason == StopReason::Cancelled)
            .build();
        self.records.push(place.record(
            entry_id.to_string(),
            span,
            TrajectoryRecordKind::Assistant(assistant),
        ));
        // A call is drawn from when it was issued until its result.
        for call in calls {
            self.pending
                .insert(call.call_id.clone(), self.records.len());
            self.records.push(place.record(
                format!("tool:{}", call.call_id),
                (Some(timing.ended_at_ms), None),
                TrajectoryRecordKind::Tool(call),
            ));
        }
    }

    /// Completes the pending call `call_id` with its result and timing.
    fn tool_result(
        &mut self,
        call_id: &ToolCallId,
        blocks: &[ContentBlock],
        is_error: bool,
        cancelled: bool,
        timing: MessageTiming,
    ) {
        let Some(record) = self
            .pending
            .remove(call_id)
            .and_then(|index| self.records.get_mut(index))
        else {
            return;
        };
        record.started_at_ms = Some(timing.started_at_ms);
        record.ended_at_ms = Some(timing.ended_at_ms);
        if let TrajectoryRecordKind::Tool(tool) = &mut record.kind {
            tool.result_text = Some(blocks.joined_text());
            tool.image_count = blocks.image_count();
            tool.is_error = is_error;
            tool.cancelled = cancelled;
        }
    }

    /// The request record of request `index`, compared with the request
    /// before it, which it then becomes.
    fn request(
        &mut self,
        index: u32,
        snapshot: RequestSnapshot,
    ) -> RequestRecord {
        let first = index == 1;
        let previous = self.previous.replace(snapshot.clone());
        let mut record = RequestRecord::builder()
            .index(index)
            .snapshot(snapshot)
            .first(first)
            .build();
        let Some(previous) = previous.filter(|_| !first) else {
            return record;
        };
        record.prompt_changed =
            previous.system_prompt != record.snapshot.system_prompt;
        if previous.tools != record.snapshot.tools {
            let names = |hash| {
                self.tool_names
                    .get(hash)
                    .map(|names| names.iter().collect::<BTreeSet<_>>())
                    .unwrap_or_default()
            };
            let before = names(&previous.tools);
            let after = names(&record.snapshot.tools);
            record.tools_added = after
                .difference(&before)
                .map(|&name| name.clone())
                .collect();
            record.tools_removed = before
                .difference(&after)
                .map(|&name| name.clone())
                .collect();
        }
        record
    }
}

/// The string at `pointer` in `value`.
fn text_at(value: &serde_json::Value, pointer: &str) -> Option<String> {
    value.pointer(pointer)?.as_str().map(ToOwned::to_owned)
}
