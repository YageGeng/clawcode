use serde::{Deserialize, Serialize};
use snafu::Snafu;

use crate::completion::request::ToolDefinition;

/// Trait that represents a simple LLM tool
pub trait Tool: Sized {
    /// The name of the tool. This name should be unique.
    const NAME: &'static str;

    /// The error type of the tool.
    type Error: std::error::Error + 'static;
    /// The arguments type of the tool.
    type Args: for<'a> Deserialize<'a>;
    /// The output type of the tool.
    type Output: Serialize;

    /// A method returning the name of the tool.
    fn name(&self) -> &'static str {
        Self::NAME
    }

    /// A method returning the tool definition. The user prompt can be used to
    /// tailor the definition to the specific use case.
    fn definition(&self, _prompt: String) -> impl Future<Output = ToolDefinition>;

    /// The tool execution method.
    /// Both the arguments and return value are a String since these values are meant to
    /// be the output and input of LLM models (respectively)
    fn call(&self, args: Self::Args) -> impl Future<Output = Result<Self::Output, Self::Error>>;
}

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum ToolError {
    #[snafu(whatever, display("Tool call error: {source:?}, message: {message}"))]
    ToolCall {
        message: String,
        // Having a `source` is optional, but if it is present, it must
        // have this specific attribute and type:
        #[snafu(source(from(Box<dyn std::error::Error + 'static+Send+Sync>, Some)))]
        source: Option<Box<dyn std::error::Error + 'static + Send + Sync>>,
    },
    /// Error caused by a de/serialization fail
    Json {
        source: serde_json::Error,
        stage: String,
    },
}
