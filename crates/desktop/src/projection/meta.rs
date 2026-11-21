//! Product metadata the kernel attaches to projected events: ordering,
//! timing, and the slash-command and assistant records of a message.

use agent_client_protocol::v2::schema as wire;
use protocol::ProductIdentity;
use serde::Deserialize as _;
use typed_builder::TypedBuilder;

use super::{CommandSource, MessageOrigin, ModelUsage};

/// Product metadata the kernel attaches to each projected event.
#[derive(Debug, Clone, Default, PartialEq, Eq, TypedBuilder)]
pub struct ProductMeta {
    /// Model turn that produced the event.
    #[builder(default)]
    pub turn_id: Option<String>,
    /// Event time in Unix milliseconds.
    #[builder(default)]
    pub timestamp_ms: Option<u64>,
    /// Kernel sequence number; the authoritative row order.
    #[builder(default)]
    pub sequence: Option<u64>,
    /// Where a message came from, such as a user-run command.
    #[builder(default)]
    pub origin: Option<MessageOrigin>,
    /// Whether a policy blocked the tool call.
    #[builder(default)]
    pub blocked: bool,
    /// `(started_at_ms, ended_at_ms)` of a completed message, in Unix milliseconds.
    #[builder(default)]
    pub message_timing: Option<(u64, u64)>,
    /// Slash-command identity of a command message.
    #[builder(default)]
    pub command: Option<CommandMeta>,
    /// Model and usage of an assistant message.
    #[builder(default)]
    pub assistant: Option<AssistantMeta>,
}

impl From<&wire::Meta> for ProductMeta {
    /// Reads the `clawcode` namespace; absent or malformed fields stay empty.
    fn from(meta: &wire::Meta) -> Self {
        let Some(product) = meta.get(ProductIdentity::ACP_NAMESPACE) else {
            return ProductMeta::default();
        };
        // Borrow every string in place; only the turn id is kept as an owned copy.
        let text =
            |key: &str| product.get(key).and_then(serde_json::Value::as_str);
        // The server serializes `protocol::MessageTiming`; a value that does
        // not decode as one is ignored rather than guessed at.
        let message_timing = product
            .get("messageTiming")
            .and_then(|timing| {
                protocol::MessageTiming::deserialize(timing).ok()
            })
            .map(|timing| {
                (timing.started_at_ms.get(), timing.ended_at_ms.get())
            });
        ProductMeta::builder()
            .command(
                product
                    .get("slashCommand")
                    .and_then(|command| CommandMeta::deserialize(command).ok()),
            )
            .assistant(product.get("assistant").and_then(AssistantMeta::parse))
            .message_timing(message_timing)
            .turn_id(text("turnId").map(str::to_owned))
            // Timestamps travel as decimal strings to keep full precision in JavaScript.
            .timestamp_ms(
                text("timestampMs").and_then(|value| value.parse().ok()),
            )
            .sequence(
                product.get("sequence").and_then(serde_json::Value::as_u64),
            )
            .origin(
                text("origin").map(|origin| MessageOrigin::from(Some(origin))),
            )
            .blocked(
                product
                    .pointer("/disposition/type")
                    .and_then(serde_json::Value::as_str)
                    == Some("blocked"),
            )
            .build()
    }
}

/// What part of a slash command a message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    /// The user's `/name args` invocation.
    Invocation,
    /// The command's result.
    Output,
    /// A skill or template expanded into a prompt.
    Expansion,
}

/// How a slash command ended, as the kernel's `SlashCommandStatus` serializes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// It finished.
    Succeeded,
    /// It failed.
    Failed,
    /// It was cancelled.
    Cancelled,
}

/// Slash-command identity of a message, `_meta.clawcode.slashCommand` on the wire.
///
/// Any unknown enum value fails the whole record, so it is not shown at all.
#[derive(Debug, Clone, PartialEq, Eq, TypedBuilder, serde::Deserialize)]
pub struct CommandMeta {
    /// Command name without the slash.
    pub name: String,
    /// Where the command comes from.
    pub source: CommandSource,
    /// Which part of the command this message is.
    #[serde(rename = "messageKind")]
    pub kind: CommandKind,
    /// Outcome, on output messages.
    #[builder(default)]
    #[serde(default)]
    pub status: Option<CommandStatus>,
}

/// Model, usage, and error of an assistant message.
#[derive(Debug, Clone, PartialEq, Eq, TypedBuilder)]
pub struct AssistantMeta {
    /// Provider that served the reply.
    pub provider_id: String,
    /// Model that wrote the reply.
    pub model_id: String,
    /// Token counts, when reported and well-formed.
    #[builder(default)]
    pub usage: Option<ModelUsage>,
    /// Why the model call failed, if it did.
    #[builder(default)]
    pub error: Option<String>,
}

impl AssistantMeta {
    /// Reads `_meta.clawcode.assistant`; the model ids are required.
    fn parse(value: &serde_json::Value) -> Option<Self> {
        let text = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        Some(
            AssistantMeta::builder()
                .provider_id(text("provider_id")?)
                .model_id(text("model_id")?)
                .usage(
                    value
                        .get("usage")
                        .and_then(|usage| ModelUsage::try_from(usage).ok()),
                )
                .error(text("error"))
                .build(),
        )
    }
}
