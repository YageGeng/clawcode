use protocol::ContentBlock;

use crate::ModelError;

/// Borrows the authoritative assistant turn for both final settlement and edit detection.
pub(crate) struct AssistantProjection<'a> {
    message: &'a rig_core::message::AssistantMessage,
}

impl<'a> From<&'a rig_core::message::AssistantMessage>
    for AssistantProjection<'a>
{
    /// Creates a scoped view without cloning native provider payloads.
    fn from(message: &'a rig_core::message::AssistantMessage) -> Self {
        Self { message }
    }
}

impl AssistantProjection<'_> {
    /// Maps native termination once for snapshots and replay comparison.
    pub(crate) fn stop_reason(&self) -> protocol::StopReason {
        use rig_core::message::StopReason;
        match self.message.stop.as_ref() {
            None | Some(StopReason::Stop) => protocol::StopReason::EndTurn,
            Some(StopReason::Length) => protocol::StopReason::MaxTokens,
            Some(StopReason::ToolUse) => protocol::StopReason::ToolUse,
            Some(StopReason::Aborted(_)) => protocol::StopReason::Cancelled,
            _ => protocol::StopReason::Error,
        }
    }

    /// Borrows only provider parts that have a project display representation.
    fn visible(
        &self,
    ) -> impl Iterator<Item = &rig_core::message::AssistantContent> {
        use rig_core::message::{AssistantContent, DocumentSourceKind};
        self.message.content.iter().filter(|content| match content {
            AssistantContent::Text(_) | AssistantContent::ToolCall(_) => true,
            AssistantContent::Reasoning(reasoning) => {
                !reasoning.redacted && !reasoning.text.is_empty()
            }
            AssistantContent::Image(image) => {
                matches!(image.data, DocumentSourceKind::Base64(_))
                    && image.media_type.is_some()
            }
            _ => false,
        })
    }

    /// Creates display blocks without exposing opaque native items or encrypted reasoning.
    pub(crate) fn blocks(&self) -> Result<Vec<ContentBlock>, ModelError> {
        use rig_core::message::{
            AssistantContent, DocumentSourceKind, MimeType,
        };
        let mut blocks = Vec::new();
        for content in self.visible() {
            match content {
                AssistantContent::Text(text) => match blocks.last_mut() {
                    Some(ContentBlock::Text { text: previous }) => {
                        previous.push_str(&text.text)
                    }
                    _ => blocks.push(ContentBlock::Text {
                        text: text.text.clone(),
                    }),
                },
                AssistantContent::Reasoning(reasoning) => {
                    match blocks.last_mut() {
                        Some(ContentBlock::Reasoning { text: previous }) => {
                            previous.push_str(&reasoning.text)
                        }
                        _ => blocks.push(ContentBlock::Reasoning {
                            text: reasoning.text.clone(),
                        }),
                    }
                }
                AssistantContent::ToolCall(call) => {
                    let call = protocol::ToolCall::try_from(call).map_err(
                        |error| ModelError::Protocol(error.to_string()),
                    )?;
                    blocks.push(ContentBlock::ToolCall {
                        tool_call_id: call.tool_call_id,
                        name: call.name,
                        arguments: call.arguments,
                        invalid_arguments: call.invalid_arguments,
                    });
                }
                AssistantContent::Image(image) => {
                    if let (DocumentSourceKind::Base64(data), Some(mime)) =
                        (&image.data, &image.media_type)
                    {
                        blocks.push(ContentBlock::Image {
                            data: data.clone(),
                            mime_type: mime.to_mime_type().to_string(),
                        });
                    }
                }
                _ => {}
            }
        }
        Ok(blocks)
    }

    /// Compares text runs by contiguous byte slices, avoiding both concatenation and per-byte iteration.
    fn same_text<'a>(
        mut left: impl Iterator<Item = &'a str>,
        mut right: impl Iterator<Item = &'a str>,
    ) -> bool {
        let mut lhs: &[u8] = &[];
        let mut rhs: &[u8] = &[];
        loop {
            if lhs.is_empty() {
                lhs = left
                    .find(|chunk| !chunk.is_empty())
                    .map(str::as_bytes)
                    .unwrap_or_default();
            }
            if rhs.is_empty() {
                rhs = right
                    .find(|chunk| !chunk.is_empty())
                    .map(str::as_bytes)
                    .unwrap_or_default();
            }
            if lhs.is_empty() || rhs.is_empty() {
                return lhs.is_empty() && rhs.is_empty();
            }
            let length = lhs.len().min(rhs.len());
            let (left_prefix, left_rest) = lhs.split_at(length);
            let (right_prefix, right_rest) = rhs.split_at(length);
            if left_prefix != right_prefix {
                return false;
            }
            lhs = left_rest;
            rhs = right_rest;
        }
    }

    /// Detects host edits only after the final response has replaced temporary delta content.
    pub(crate) fn matches(
        &self,
        blocks: &[ContentBlock],
        stop_reason: protocol::StopReason,
    ) -> Result<bool, ModelError> {
        if self.stop_reason() != stop_reason {
            return Ok(false);
        }
        use rig_core::message::{
            AssistantContent, DocumentSourceKind, Image, MimeType,
        };
        let mut expected = self.visible().peekable();
        let mut actual = blocks.iter().peekable();
        loop {
            match (expected.peek(), actual.peek()) {
                (Some(native), Some(display))
                    if matches!(
                        (native, display),
                        (AssistantContent::Text(_), ContentBlock::Text { .. })
                            | (
                                AssistantContent::Reasoning(_),
                                ContentBlock::Reasoning { .. }
                            )
                    ) =>
                {
                    let reasoning =
                        matches!(native, AssistantContent::Reasoning(_));
                    let native = std::iter::from_fn(|| {
                        expected.next_if(|part| {
                            matches!(
                                (reasoning, part),
                                (false, AssistantContent::Text(_))
                                    | (true, AssistantContent::Reasoning(_))
                            )
                        })
                    })
                    .map(|part| match part {
                        AssistantContent::Text(text) => text.text.as_str(),
                        AssistantContent::Reasoning(reasoning) => {
                            reasoning.text.as_str()
                        }
                        _ => "",
                    });
                    let display = std::iter::from_fn(|| {
                        actual.next_if(|part| {
                            matches!(
                                (reasoning, part),
                                (false, ContentBlock::Text { .. })
                                    | (true, ContentBlock::Reasoning { .. })
                            )
                        })
                    })
                    .map(|part| match part {
                        ContentBlock::Text { text }
                        | ContentBlock::Reasoning { text } => text.as_str(),
                        _ => "",
                    });
                    if !Self::same_text(native, display) {
                        return Ok(false);
                    }
                }
                (
                    Some(AssistantContent::ToolCall(call)),
                    Some(ContentBlock::ToolCall {
                        tool_call_id,
                        name,
                        arguments,
                        invalid_arguments,
                    }),
                ) => {
                    if call.id.wire().as_ref() != tool_call_id.as_str()
                        || call.function.name.as_str() != name
                        || arguments.as_object()
                            != Some(&call.function.arguments)
                        || &call.function.invalid_arguments != invalid_arguments
                    {
                        return Ok(false);
                    }
                    expected.next();
                    actual.next();
                }
                (
                    Some(AssistantContent::Image(Image {
                        data: DocumentSourceKind::Base64(expected_data),
                        media_type: Some(expected_mime),
                        ..
                    })),
                    Some(ContentBlock::Image { data, mime_type }),
                ) => {
                    if expected_data != data
                        || expected_mime.to_mime_type() != mime_type
                    {
                        return Ok(false);
                    }
                    expected.next();
                    actual.next();
                }
                (None, None) => return Ok(true),
                _ => return Ok(false),
            }
        }
    }
}

/// Converts transcript content with the same text view used by the output budget.
pub(super) trait ProviderContentProjection {
    /// Consumes one non-native block and projects its model-visible text.
    fn into_provider_text(self) -> Result<String, ModelError>;
}

impl ProviderContentProjection for ContentBlock {
    /// Reuses bounded-output formatting so JSON expansion cannot bypass accounting.
    fn into_provider_text(self) -> Result<String, ModelError> {
        if let Self::Text { text } | Self::Reasoning { text } = self {
            return Ok(text);
        }
        let view = tools::ToolContentText::try_from(&self)
            .map_err(|error| ModelError::Protocol(error.to_string()))?;
        let mut bytes = Vec::new();
        view.write_to(&mut bytes)
            .map_err(|error| ModelError::Protocol(error.to_string()))?;
        String::from_utf8(bytes)
            .map_err(|error| ModelError::Protocol(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::message::{AssistantContent, AssistantMessage, Reasoning};

    /// Harmless display resegmentation must retain signed native history.
    #[test]
    fn matching_projection_ignores_adjacent_text_and_reasoning_boundaries() {
        let native = AssistantMessage::new(vec![
            AssistantContent::text("Hel"),
            AssistantContent::text("lo"),
            AssistantContent::Reasoning(Reasoning::new("Plan B")),
        ]);
        let blocks = vec![
            ContentBlock::Text {
                text: "Hello".into(),
            },
            ContentBlock::Reasoning {
                text: "Plan ".into(),
            },
            ContentBlock::Reasoning { text: "B".into() },
        ];
        assert!(
            AssistantProjection::from(&native)
                .matches(&blocks, protocol::StopReason::EndTurn)
                .expect("display comparison")
        );
    }
    /// Borrowed comparison must keep the display contract for hidden data, chunk boundaries and edits.
    #[test]
    fn projection_matches_only_equivalent_visible_content() {
        let mut hidden = Reasoning::new("hidden");
        hidden.redacted = true;
        let native = AssistantMessage::new(vec![
            AssistantContent::text("你"),
            AssistantContent::Reasoning(hidden),
            AssistantContent::text("好"),
            AssistantContent::reasoning("Plan "),
            AssistantContent::reasoning("B"),
            AssistantContent::tool_call(
                "call",
                rig_core::message::ToolName::new("lookup").expect("name"),
                serde_json::json!("{"),
            ),
            AssistantContent::image_base64(
                "AA==",
                Some(rig_core::message::ImageMediaType::PNG),
                None,
            ),
            AssistantContent::image_base64("hidden-image", None, None),
        ]);
        let blocks = vec![
            ContentBlock::Text {
                text: "你好".into(),
            },
            ContentBlock::Reasoning {
                text: "Plan B".into(),
            },
            ContentBlock::ToolCall {
                tool_call_id: protocol::ToolCallId::try_from("call")
                    .expect("ID"),
                name: "lookup".into(),
                arguments: serde_json::json!({}),
                invalid_arguments: Some("{".into()),
            },
            ContentBlock::Image {
                data: "AA==".into(),
                mime_type: "image/png".into(),
            },
        ];
        let projection = AssistantProjection::from(&native);
        assert_eq!(projection.blocks().expect("projection"), blocks);
        assert!(
            projection
                .matches(&blocks, protocol::StopReason::EndTurn)
                .expect("comparison")
        );
        let mut split = blocks.clone();
        split.splice(
            0..1,
            [
                ContentBlock::Text { text: "你".into() },
                ContentBlock::Text {
                    text: String::new(),
                },
                ContentBlock::Text { text: "好".into() },
            ],
        );
        assert!(
            projection
                .matches(&split, protocol::StopReason::EndTurn)
                .expect("split comparison")
        );
        for replacement in [
            ContentBlock::Text {
                text: "different".into(),
            },
            ContentBlock::Reasoning {
                text: "different".into(),
            },
            ContentBlock::ToolCall {
                tool_call_id: protocol::ToolCallId::try_from("call")
                    .expect("ID"),
                name: "lookup".into(),
                arguments: serde_json::json!({"edited":true}),
                invalid_arguments: Some("{".into()),
            },
            ContentBlock::Image {
                data: "changed".into(),
                mime_type: "image/png".into(),
            },
        ] {
            let mut changed = blocks.clone();
            let index = match replacement {
                ContentBlock::Text { .. } => 0,
                ContentBlock::Reasoning { .. } => 1,
                ContentBlock::ToolCall { .. } => 2,
                _ => 3,
            };
            changed[index] = replacement;
            assert!(
                !projection
                    .matches(&changed, protocol::StopReason::EndTurn)
                    .expect("edit comparison")
            );
        }
        assert!(
            !projection
                .matches(&blocks, protocol::StopReason::Error)
                .expect("stop comparison")
        );
        let empty = AssistantMessage::new(vec![AssistantContent::text("")]);
        assert!(
            !AssistantProjection::from(&empty)
                .matches(&[], protocol::StopReason::EndTurn)
                .expect("empty presence")
        );
    }
}
