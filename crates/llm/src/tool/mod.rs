use serde::{Deserialize, Serialize};

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
    fn name(&self) -> String {
        Self::NAME.to_string()
    }

    /// A method returning the tool definition. The user prompt can be used to
    /// tailor the definition to the specific use case.
    fn definition(&self, _prompt: String) -> impl Future<Output = ToolDefinition>;

    /// The tool execution method.
    /// Both the arguments and return value are a String since these values are meant to
    /// be the output and input of LLM models (respectively)
    fn call(&self, args: Self::Args) -> impl Future<Output = Result<Self::Output, Self::Error>>;
}
