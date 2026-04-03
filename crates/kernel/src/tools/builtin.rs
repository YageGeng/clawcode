use std::sync::Arc;

use async_trait::async_trait;
use chrono::Local;
use serde::Deserialize;
use snafu::ResultExt;

use crate::{
    Result,
    error::JsonSnafu,
    tools::{Tool, ToolContext, ToolOutput},
};

#[derive(Debug, Deserialize)]
struct EchoArgs {
    text: String,
}

#[derive(Debug, Deserialize)]
struct JsonArgs {
    value: String,
}

pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> &'static str {
        "Returns the provided text unchanged."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string" }
            },
            "required": ["text"]
        })
    }

    /// Parses the echo payload and returns it unchanged.
    async fn execute(&self, args: serde_json::Value, _context: ToolContext) -> Result<ToolOutput> {
        let args: EchoArgs = serde_json::from_value(args).context(JsonSnafu {
            stage: "echo-parse-args".to_string(),
        })?;
        Ok(ToolOutput::text(args.text))
    }
}

pub struct JsonTool;

#[async_trait]
impl Tool for JsonTool {
    fn name(&self) -> &'static str {
        "json"
    }

    fn description(&self) -> &'static str {
        "Pretty prints a JSON value encoded as a string and returns it as text."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "value": {
                    "type": "string",
                    "description": "A valid JSON string to pretty print, for example {\"hello\":\"world\"}."
                }
            },
            "required": ["value"]
        })
    }

    /// Parses a JSON string and pretty-prints the resulting value for model consumption.
    async fn execute(&self, args: serde_json::Value, _context: ToolContext) -> Result<ToolOutput> {
        let args: JsonArgs = serde_json::from_value(args).context(JsonSnafu {
            stage: "json-parse-args".to_string(),
        })?;
        let value: serde_json::Value = serde_json::from_str(&args.value).context(JsonSnafu {
            stage: "json-parse-input".to_string(),
        })?;
        let text = serde_json::to_string_pretty(&value).context(JsonSnafu {
            stage: "json-pretty-print".to_string(),
        })?;
        Ok(ToolOutput {
            text,
            structured: value,
        })
    }
}

pub struct TimeTool;

#[async_trait]
impl Tool for TimeTool {
    fn name(&self) -> &'static str {
        "time"
    }

    fn description(&self) -> &'static str {
        "Returns the current local time as a human-readable string."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    /// Formats the current local time once so text and structured output stay in sync.
    async fn execute(&self, _args: serde_json::Value, _context: ToolContext) -> Result<ToolOutput> {
        let now = Local::now();
        let local_time = now.format("%Y-%m-%d %H:%M:%S %Z").to_string();
        let unix_seconds: u64 = now
            .timestamp()
            .try_into()
            .expect("current local time should be after the Unix epoch");

        Ok(ToolOutput {
            text: local_time.clone(),
            structured: serde_json::json!({
                "local_time": local_time,
                "unix_seconds": unix_seconds,
            }),
        })
    }
}

/// Builds the first milestone's safe read-only tool set.
pub fn default_read_only_tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(EchoTool), Arc::new(JsonTool), Arc::new(TimeTool)]
}
