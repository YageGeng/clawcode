//! Codex `request_user_input_async` questions and the replies that settle them.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    ContentBlock, ExtensionId, ExtensionMessage, MessageContent,
    ProductIdentity, ScalarError,
};

/// Tool that asks the user questions without waiting for an answer.
pub const REQUEST_USER_INPUT_ASYNC_TOOL: &str = "request_user_input_async";
/// Extension custom type of one persisted answer or skip record.
pub const QUESTION_REPLY_TYPE: &str = "question_reply";

/// Opening tag of Codex's reply envelope.
const ENVELOPE_START: &str = "<send_user_message_question_reply>";
/// Closing tag of Codex's reply envelope.
const ENVELOPE_END: &str = "</send_user_message_question_reply>";
/// Bound on the model-authored title repeated inside the envelope.
const ENVELOPE_TITLE_BYTES: usize = 512;

/// One model-authored question shown to the user without waiting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AsyncQuestion {
    /// Complete question text, rendered as Markdown.
    pub title: String,
    /// Suggested answers in display order; the first is preselected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// Whether the user may pick several options; the answer then arrives as
    /// a Markdown list with one picked option per item.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi_select: bool,
}

/// Identity of one question: the asking call plus its position; clients
/// address questions with it as `{ requestId, index }`.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionKey {
    /// Kernel-generated identity of the asking `request_user_input_async` call.
    pub request_id: String,
    /// Position of the question within that call.
    pub index: usize,
}

/// One settlement of one asynchronous question.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct QuestionReplyItem {
    /// Identity of the asking `request_user_input_async` call.
    pub request_id: String,
    /// Position of the question within that call.
    pub index: usize,
    /// Title of the settled question, copied by the kernel from the asking
    /// result so the record renders without looking the question up.
    #[serde(default)]
    #[builder(default)]
    pub question: String,
    /// The user's answer; `None` means the user skipped the question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub answer: Option<String>,
}

impl From<&QuestionReplyItem> for QuestionKey {
    /// Identifies the question one item settles.
    fn from(item: &QuestionReplyItem) -> Self {
        Self {
            request_id: item.request_id.clone(),
            index: item.index,
        }
    }
}

/// Settlements recorded together in one `question_reply` message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionReply {
    /// Settled questions in the order the user acted on them.
    pub items: Vec<QuestionReplyItem>,
}

impl QuestionReply {
    /// Returns every question this reply settles, in item order.
    pub fn keys(&self) -> impl Iterator<Item = QuestionKey> + '_ {
        self.items.iter().map(QuestionKey::from)
    }
}

/// Why a message does not carry a question reply.
#[derive(Debug, thiserror::Error)]
pub enum QuestionReplyError {
    /// The message is not a `question_reply` Extension.
    #[error("message is not a question reply")]
    NotAReply,
    /// The Extension's details do not decode as a reply.
    #[error("invalid question reply details: {0}")]
    Details(#[from] serde_json::Error),
}

impl TryFrom<&ExtensionMessage> for QuestionReply {
    type Error = QuestionReplyError;

    /// Reads the reply a `question_reply` Extension carries; only the
    /// product's own Extension counts, so other extensions cannot forge one.
    fn try_from(extension: &ExtensionMessage) -> Result<Self, Self::Error> {
        match (&extension.details, extension.custom_type.as_str()) {
            (Some(details), QUESTION_REPLY_TYPE)
                if extension.extension_id.as_ref() == ProductIdentity::SLUG =>
            {
                Ok(serde_json::from_value(details.clone())?)
            }
            _ => Err(QuestionReplyError::NotAReply),
        }
    }
}

impl TryFrom<&MessageContent> for QuestionReply {
    type Error = QuestionReplyError;

    /// Reads the reply a transcript or queued message carries.
    fn try_from(content: &MessageContent) -> Result<Self, Self::Error> {
        match content {
            MessageContent::Extension { extension } => {
                Self::try_from(extension)
            }
            _ => Err(QuestionReplyError::NotAReply),
        }
    }
}

/// One envelope entry; field order is fixed so the text never depends on
/// serde_json map features.
#[derive(Serialize, typed_builder::TypedBuilder)]
struct EnvelopeEntry<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    answer: Option<&'a str>,
    question: String,
    #[serde(rename = "questionItemId")]
    question_item_id: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    skipped: bool,
}

impl fmt::Display for QuestionReply {
    /// Writes the envelope exactly as Codex's `AnsweredQuestion` frames replies.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let entries = self
            .items
            .iter()
            .map(|item| {
                // Model-authored titles are bounded and kept on one line.
                let end =
                    item.question.floor_char_boundary(ENVELOPE_TITLE_BYTES);
                EnvelopeEntry::builder()
                    .answer(item.answer.as_deref())
                    .question(
                        item.question
                            .get(..end)
                            .unwrap_or_default()
                            .replace(['\n', '\r'], " "),
                    )
                    .question_item_id(
                        serde_json::json!([
                            REQUEST_USER_INPUT_ASYNC_TOOL,
                            item.request_id,
                            item.index
                        ])
                        .to_string(),
                    )
                    .skipped(item.answer.is_none())
                    .build()
            })
            .collect::<Vec<_>>();
        // fmt::Error carries no source, so the serde error cannot travel with it.
        let json =
            serde_json::to_string(&entries).map_err(|_serde| fmt::Error)?;
        write!(formatter, "{ENVELOPE_START}\n{json}\n{ENVELOPE_END}")
    }
}

impl TryFrom<&QuestionReply> for ExtensionMessage {
    type Error = ScalarError;

    /// Builds the visible, context-included Extension message for one reply.
    fn try_from(reply: &QuestionReply) -> Result<Self, Self::Error> {
        let details = serde_json::to_value(reply)
            .expect("question replies serialize as plain JSON");
        Ok(Self::builder()
            .extension_id(ExtensionId::try_from(ProductIdentity::SLUG)?)
            .custom_type(QUESTION_REPLY_TYPE.to_string())
            .blocks(vec![ContentBlock::Text {
                text: reply.to_string(),
            }])
            .display(true)
            .include_in_context(true)
            .details(details)
            .build())
    }
}
