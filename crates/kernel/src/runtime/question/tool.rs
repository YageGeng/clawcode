//! The `request_user_input_async` tool: validates questions and returns at once.

use std::sync::Arc;

use ::tools::{AgentTool, ToolError, ToolExecutionContext};
use async_trait::async_trait;
use protocol::{
    AsyncQuestion, ContentBlock, IdGenerator, IdKind,
    REQUEST_USER_INPUT_ASYNC_TOOL, ToolCall, ToolDefinition, ToolResult,
    ToolResultDetails,
};
use serde::Deserialize;

/// Codex's model-facing description, copied verbatim.
const DESCRIPTION: &str = "Ask the user one or more questions during ongoing work. Use this tool only to request missing information, preferences, constraints, clarification, or approval. The tool returns immediately without ending the turn or waiting for a reply; any reply arrives asynchronously as a new user message. Keep questions concise, self-contained, and easy to understand, using a level of detail appropriate to the user and task. The UI always allows a free-text answer, including when suggested options are provided. A preselected option is not submitted automatically.";
/// Result the model sees; the answer arrives later as a user message.
const ACCEPTED_TEXT: &str = r#"{"accepted":true}"#;
/// Most questions one call may ask.
const MAX_QUESTIONS: usize = 8;
/// Longest question title in bytes.
const MAX_TITLE_BYTES: usize = 2048;
/// Most suggested answers per question.
const MAX_OPTIONS: usize = 8;
/// Longest suggested answer in bytes.
const MAX_OPTION_BYTES: usize = 512;

/// Asks the user questions and returns without waiting for the answers.
pub(super) struct RequestUserInputAsyncTool {
    pub(super) id_generator: Arc<dyn IdGenerator>,
}

/// The complete, validated argument object; unknown fields are refused.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    questions: Vec<AsyncQuestion>,
}

impl TryFrom<&ToolCall> for Arguments {
    type Error = ToolError;

    /// Parses one call and checks every limit the WebUI relies on.
    fn try_from(call: &ToolCall) -> Result<Self, Self::Error> {
        let invalid = |message: String| ToolError::InvalidArguments {
            tool: REQUEST_USER_INPUT_ASYNC_TOOL.into(),
            message,
        };
        let arguments: Self = serde_json::from_value(call.arguments.clone())
            .map_err(|error| invalid(error.to_string()))?;
        if arguments.questions.is_empty()
            || arguments.questions.len() > MAX_QUESTIONS
        {
            return Err(invalid(format!(
                "questions must contain 1 to {MAX_QUESTIONS} items"
            )));
        }
        for (position, question) in arguments.questions.iter().enumerate() {
            let number = position + 1;
            if question.title.trim().is_empty()
                || question.title.len() > MAX_TITLE_BYTES
            {
                return Err(invalid(format!(
                    "question {number} needs a non-empty title of at most {MAX_TITLE_BYTES} bytes"
                )));
            }
            let Some(options) = &question.options else {
                continue;
            };
            if options.is_empty() || options.len() > MAX_OPTIONS {
                return Err(invalid(format!(
                    "question {number} options must contain 1 to {MAX_OPTIONS} items"
                )));
            }
            if options.iter().any(|option| {
                option.trim().is_empty() || option.len() > MAX_OPTION_BYTES
            }) {
                return Err(invalid(format!(
                    "question {number} options must be non-empty and at most {MAX_OPTION_BYTES} bytes"
                )));
            }
            // The UI always offers free text, so a model-authored Other is a duplicate.
            if options
                .iter()
                .any(|option| option.trim().eq_ignore_ascii_case("other"))
            {
                return Err(invalid(format!(
                    "question {number} must not include an Other option; the UI provides free-text input"
                )));
            }
        }
        Ok(arguments)
    }
}

#[async_trait]
impl AgentTool for RequestUserInputAsyncTool {
    /// Declares Codex's schema, with the item limits this kernel enforces.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: REQUEST_USER_INPUT_ASYNC_TOOL.into(),
            description: DESCRIPTION.into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "description": "One or more self-contained questions to present together, in display order.",
                        "minItems": 1,
                        "maxItems": MAX_QUESTIONS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {
                                    "type": "string",
                                    "description": "The complete question shown to the user, including any context needed to answer it."
                                },
                                "options": {
                                    "type": "array",
                                    "description": "Suggested answers, in display order. Put the recommended answer first; the first option is preselected by default. The user can select one option (or several when multi_select is true) or enter a free-text answer. Do not include an Other option or a free-text placeholder; the UI provides free-text input automatically. Omit options for a free-text-only question.",
                                    "minItems": 1,
                                    "maxItems": MAX_OPTIONS,
                                    "items": { "type": "string" }
                                },
                                "multi_select": {
                                    "type": "boolean",
                                    "description": "Set to true when the options are not mutually exclusive and the user may pick several. The answer then arrives as a Markdown list with one picked option per `- ` item. Defaults to false."
                                }
                            },
                            "required": ["title"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["questions"],
                "additionalProperties": false
            }),
        }
    }

    /// Refuses questions outside the limits before the call runs.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        Arguments::try_from(call).map(drop)
    }

    /// Records the questions in the result's details and returns at once.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        let Arguments { questions } = Arguments::try_from(&call)?;
        // Tool call ids can repeat across Turns, so answers address this id.
        let request_id = self.id_generator.next(IdKind::Request);
        tracing::info!(
            "asked {} questions {} in session {}",
            questions.len(),
            request_id,
            context.session_id
        );
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text {
                text: ACCEPTED_TEXT.into(),
            }])
            .is_error(false)
            .details(ToolResultDetails::UserQuestions {
                request_id,
                questions,
            })
            .build())
    }
}
