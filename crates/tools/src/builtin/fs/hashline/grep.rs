//! Hashline grep tool.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;

use crate::Tool;

use super::format::compute_line_hash;

const DEFAULT_LIMIT: usize = 100;

/// Searches files and returns hashline-prefixed ripgrep matches.
pub struct HashlineGrep;

impl HashlineGrep {
    /// Create a new hashline grep tool instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Return whether the ripgrep binary is available on PATH.
    #[must_use]
    pub fn is_available() -> bool {
        StdCommand::new("rg")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }
}

impl Default for HashlineGrep {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for HashlineGrep {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search files with hashline-prefixed results that can be used by edit_file"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to search for" },
                "path": { "type": "string", "description": "File or directory to search; defaults to cwd" },
                "glob": { "type": "string", "description": "Filter files by glob pattern, e.g. '*.rs'" },
                "type": { "type": "string", "description": "Filter by ripgrep file type, e.g. rust" },
                "i": { "type": "boolean", "description": "Case-insensitive search" },
                "pre": { "type": "integer", "description": "Lines of context before matches" },
                "post": { "type": "integer", "description": "Lines of context after matches" },
                "limit": { "type": "integer", "description": "Maximum matches per searched file" }
            },
            "required": ["pattern"]
        })
    }

    fn needs_approval(&self, arguments: &serde_json::Value, _ctx: &crate::ToolContext) -> bool {
        // Require approval only when the target path escapes cwd.
        arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|path| Path::new(path).is_absolute() || path.contains(".."))
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let args = GrepArgs::from_value(arguments)?;
        let output = args.run(ctx).await?;

        Ok(format_grep_output(&output))
    }
}

#[derive(Debug, Deserialize, typed_builder::TypedBuilder)]
struct GrepArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    glob: Option<String>,
    #[serde(default, rename = "type")]
    file_type: Option<String>,
    #[serde(default)]
    i: bool,
    #[serde(default)]
    pre: Option<usize>,
    #[serde(default)]
    post: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

impl GrepArgs {
    /// Parse model-facing grep arguments.
    fn from_value(arguments: serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(arguments).map_err(|error| format!("invalid arguments: {error}"))
    }

    /// Run ripgrep and return raw ripgrep output.
    async fn run(&self, ctx: &crate::ToolContext) -> Result<String, String> {
        let output = self
            .command(ctx)
            .output()
            .await
            .map_err(|error| format!("failed to run rg: {error}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout)
            .trim_end_matches(['\r', '\n'])
            .to_string();

        if output.status.code() == Some(1) {
            return Ok(stdout);
        }
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = if stderr.trim().is_empty() {
                "unknown error".to_string()
            } else {
                stderr.trim().to_string()
            };
            return Err(format!("grep error: {message}"));
        }

        Ok(stdout)
    }

    /// Build the ripgrep process with the same flags as the source server.
    fn command(&self, ctx: &crate::ToolContext) -> Command {
        let mut command = Command::new("rg");
        command
            .arg("--line-number")
            .arg("--no-heading")
            .arg("--with-filename");

        if self.i {
            command.arg("-i");
        }
        if let Some(pre) = self.pre.filter(|pre| *pre > 0) {
            command.arg("-B").arg(pre.to_string());
        }
        if let Some(post) = self.post.filter(|post| *post > 0) {
            command.arg("-A").arg(post.to_string());
        }
        if let Some(glob) = &self.glob {
            command.arg("--glob").arg(glob);
        }
        if let Some(file_type) = &self.file_type {
            command.arg("--type").arg(file_type);
        }

        command
            .arg("-m")
            .arg(self.limit.unwrap_or(DEFAULT_LIMIT).to_string())
            .arg("--")
            .arg(&self.pattern)
            .arg(self.target_path(ctx))
            .current_dir(&ctx.cwd);

        command
    }

    /// Resolve the optional search path relative to the tool context cwd.
    fn target_path(&self, ctx: &crate::ToolContext) -> PathBuf {
        self.path
            .as_ref()
            .map(PathBuf::from)
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    ctx.cwd.join(path)
                }
            })
            .unwrap_or_else(|| ctx.cwd.clone())
    }
}

enum RgLine<'a> {
    Match {
        file: &'a str,
        line: usize,
        content: &'a str,
    },
    Context {
        file: &'a str,
        line: usize,
        content: &'a str,
    },
}

impl<'a> RgLine<'a> {
    /// Parse one ripgrep output line into match or context metadata.
    fn parse(input: &'a str) -> Option<Self> {
        Self::parse_with_separator(input, ':')
            .map(|(file, line, content)| Self::Match {
                file,
                line,
                content,
            })
            .or_else(|| {
                Self::parse_with_separator(input, '-').map(|(file, line, content)| Self::Context {
                    file,
                    line,
                    content,
                })
            })
    }

    /// Parse `file<sep>line<sep>content` while tolerating separators inside file names.
    fn parse_with_separator(input: &'a str, separator: char) -> Option<(&'a str, usize, &'a str)> {
        let separator_len = separator.len_utf8();
        for (separator_index, _) in input.match_indices(separator) {
            let after_separator = input.get(separator_index + separator_len..)?;
            let digit_len = after_separator
                .chars()
                .take_while(|ch| ch.is_ascii_digit())
                .map(char::len_utf8)
                .sum::<usize>();
            if digit_len == 0 {
                continue;
            }

            let after_digits = after_separator.get(digit_len..)?;
            if !after_digits.starts_with(separator) {
                continue;
            }

            let file = input.get(..separator_index)?;
            let line = after_separator.get(..digit_len)?.parse::<usize>().ok()?;
            let content = after_digits.get(separator_len..)?;
            return Some((file, line, content));
        }
        None
    }

    /// Format one parsed ripgrep line with match or context markers.
    fn format(self) -> String {
        match self {
            Self::Match {
                file,
                line,
                content,
            } => format!("{file}:>>{line}:{}|{content}", compute_line_hash(content)),
            Self::Context {
                file,
                line,
                content,
            } => format!("{file}:  {line}:{}|{content}", compute_line_hash(content)),
        }
    }
}

/// Format raw ripgrep output as hashline-prefixed grep results.
#[must_use]
fn format_grep_output(output: &str) -> String {
    if output.is_empty() {
        return "No matches found.".to_string();
    }

    let formatted = output
        .split('\n')
        .map(|line| {
            if line == "--" {
                "--".to_string()
            } else {
                RgLine::parse(line)
                    .map(RgLine::format)
                    .unwrap_or_else(|| line.to_string())
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("```\n{formatted}\n```")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Tool, ToolContext};

    /// Verifies rg match and context output is converted to hashline form.
    #[test]
    fn format_grep_output_marks_matches_and_context_with_hashes() {
        let output = format_grep_output(
            "src/main.rs:2:hello world\nsrc/main.rs-3-context\n--\nsrc/lib.rs:1:hello again",
        );

        assert_eq!(
            output,
            "```\nsrc/main.rs:>>2:02|hello world\nsrc/main.rs:  3:61|context\n--\nsrc/lib.rs:>>1:e4|hello again\n```"
        );
    }

    /// Verifies an empty raw grep output is formatted as a no-match response.
    #[test]
    fn format_grep_output_reports_no_matches_for_empty_output() {
        assert_eq!(format_grep_output(""), "No matches found.");
    }

    /// Verifies grep invokes local ripgrep without using a filesystem backend.
    #[tokio::test]
    async fn hashline_grep_runs_ripgrep_locally() {
        if !HashlineGrep::is_available() {
            return;
        }

        let dir = tempfile::tempdir().expect("tempdir should be created");
        let src = dir.path().join("src");
        tokio::fs::create_dir_all(&src)
            .await
            .expect("src dir should be created");
        tokio::fs::write(src.join("main.rs"), "hay\nneedle\nNEEDLE\n")
            .await
            .expect("rust fixture should be written");
        tokio::fs::write(src.join("main.py"), "needle\n")
            .await
            .expect("python fixture should be written");

        let tool = HashlineGrep::new();

        let result = tool
            .execute(
                serde_json::json!({
                    "pattern": "needle",
                    "path": "src",
                    "glob": "*.rs",
                    "i": true,
                    "limit": 10
                }),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .expect("grep should succeed");

        assert!(result.contains(&format!(":>>2:{}|needle", compute_line_hash("needle"))));
        assert!(result.contains(&format!(":>>3:{}|NEEDLE", compute_line_hash("NEEDLE"))));
        assert!(!result.contains(":>>1:"));
        assert!(!result.contains("main.py"));
    }

    /// Verifies local ripgrep no-match output is model-facing plain text.
    #[tokio::test]
    async fn hashline_grep_reports_no_matches() {
        if !HashlineGrep::is_available() {
            return;
        }

        let dir = tempfile::tempdir().expect("tempdir should be created");
        tokio::fs::write(dir.path().join("file.txt"), "aaa\nbbb\n")
            .await
            .expect("fixture should be written");
        let tool = HashlineGrep::new();

        let result = tool
            .execute(
                serde_json::json!({"pattern": "missing"}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .expect("no-match grep should succeed");

        assert_eq!(result, "No matches found.");
    }
}
