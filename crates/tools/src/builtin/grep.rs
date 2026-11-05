use std::path::{Path, PathBuf};

use async_trait::async_trait;
use grep::regex::{RegexMatcher, RegexMatcherBuilder};
use grep::searcher::{
    BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch,
};
use ignore::overrides::{Override, OverrideBuilder};
use protocol::{
    ContentBlock, ToolCall, ToolDefinition, ToolPromptContribution, ToolResult,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::path::ResolvedPath;
use crate::{
    AgentTool, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, ToolError,
    ToolExecutionContext, format_size, truncate_head,
};

/// Pi's default number of matching lines returned per call.
const DEFAULT_LIMIT: usize = 100;

/// Pi's maximum characters kept from one rendered grep line.
const MAX_LINE_CHARS: usize = 500;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrepArguments {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    #[serde(default)]
    ignore_case: bool,
    #[serde(default)]
    literal: bool,
    context: Option<f64>,
    limit: Option<f64>,
}

impl TryFrom<&ToolCall> for GrepArguments {
    type Error = ToolError;

    /// Decodes one grep call without consuming its execution correlation.
    fn try_from(call: &ToolCall) -> Result<Self, Self::Error> {
        serde_json::from_value(call.arguments.clone()).map_err(|error| {
            ToolError::InvalidArguments {
                tool: call.name.clone(),
                message: error.to_string(),
            }
        })
    }
}

impl GrepArguments {
    /// Compiles the pattern with ripgrep's line-oriented matcher settings.
    fn matcher(&self) -> Result<RegexMatcher, ToolError> {
        RegexMatcherBuilder::new()
            .case_insensitive(self.ignore_case)
            .fixed_strings(self.literal)
            // Like ripgrep, a match never spans a line terminator.
            .line_terminator(Some(b'\n'))
            .build(&self.pattern)
            .map_err(|error| {
                tracing::warn!("grep rejected pattern: {}", error);
                ToolError::InvalidArguments {
                    tool: "grep".to_string(),
                    message: error.to_string(),
                }
            })
    }

    /// Compiles the optional glob as a ripgrep `--glob` override rooted at `cwd`.
    fn overrides(&self, cwd: &Path) -> Result<Override, ToolError> {
        let mut builder = OverrideBuilder::new(cwd);
        self.glob
            .iter()
            .try_for_each(|glob| builder.add(glob).map(drop))
            .and_then(|()| builder.build())
            .map_err(|error| ToolError::InvalidArguments {
                tool: "grep".to_string(),
                message: error.to_string(),
            })
    }

    /// Context lines on each side of a match, clamped to 2000 lines.
    fn context_lines(&self) -> usize {
        // Float-to-integer casts saturate, so NaN and negatives become zero.
        // The searcher buffers every before-context line until a match, so an
        // unbounded value would hold a whole file; output is capped at 50KB anyway.
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        self.context
            .map_or(0, |context| (context as usize).min(DEFAULT_MAX_LINES))
    }

    /// Match cap, never below one like pi's `Math.max(1, limit)`.
    fn match_limit(&self) -> usize {
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        self.limit
            .map_or(DEFAULT_LIMIT, |limit| (limit as usize).max(1))
    }
}

/// One grep invocation's resolved inputs, shared by the walker and per-file sinks.
#[derive(typed_builder::TypedBuilder)]
struct GrepSearch {
    matcher: RegexMatcher,
    root: PathBuf,
    root_is_dir: bool,
    overrides: Override,
    context: usize,
    limit: usize,
}

impl GrepSearch {
    /// Walks the root like `rg --hidden` and collects rendered lines until the limit.
    fn run(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<GrepOutput, ToolError> {
        let mut searcher = SearcherBuilder::new()
            .line_number(true)
            .before_context(self.context)
            .after_context(self.context)
            .binary_detection(BinaryDetection::quit(b'\0'))
            .build();
        let walker = ignore::WalkBuilder::new(&self.root)
            .hidden(false)
            .overrides(self.overrides.clone())
            // `--hidden` would otherwise descend into repository metadata.
            .filter_entry(|entry| entry.file_name() != ".git")
            .sort_by_file_path(Path::cmp)
            .build();
        let mut output = GrepOutput::builder().limit(self.limit).build();
        for entry in walker {
            if cancellation.is_cancelled() {
                tracing::info!(
                    "grep cancelled while searching {}",
                    self.root.display()
                );
                return Err(ToolError::Cancelled);
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::warn!("grep skipped unreadable entry: {}", error);
                    continue;
                }
            };
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let sink = FileSink {
                output: &mut output,
                display_path: self.display_path(entry.path()),
                trailing: 0,
            };
            if let Err(error) =
                searcher.search_path(&self.matcher, entry.path(), sink)
            {
                tracing::warn!(
                    "grep skipped {}: {}",
                    entry.path().display(),
                    error
                );
            }
            if output.limit_reached || output.is_full() {
                break;
            }
        }
        tracing::debug!(
            "grep found {} matches under {} (limit reached: {})",
            output.matches,
            self.root.display(),
            output.limit_reached
        );
        Ok(output)
    }

    /// Renders a path relative to a directory root, or the basename for a file root.
    fn display_path(&self, path: &Path) -> String {
        let relative = self
            .root_is_dir
            .then(|| path.strip_prefix(&self.root).ok())
            .flatten()
            .filter(|relative| !relative.as_os_str().is_empty());
        match relative {
            Some(relative) => relative.to_string_lossy().replace('\\', "/"),
            None => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
        }
    }
}

/// Rendered lines across all searched files plus the limits they hit.
#[derive(typed_builder::TypedBuilder)]
struct GrepOutput {
    limit: usize,
    #[builder(default)]
    lines: Vec<String>,
    #[builder(default)]
    matches: usize,
    #[builder(default)]
    limit_reached: bool,
    #[builder(default)]
    lines_truncated: bool,
    /// Length of `lines` joined by newlines, so collection stops past the byte cap.
    #[builder(default)]
    bytes: usize,
}

impl GrepOutput {
    /// Whether any further line would only be cut by the 50KB cap.
    fn is_full(&self) -> bool {
        self.bytes > DEFAULT_MAX_BYTES
    }

    /// Appends one line as `path:N: text` for matches or `path-N- text` for context.
    fn push(
        &mut self,
        display_path: &str,
        separator: char,
        line_number: Option<u64>,
        bytes: &[u8],
    ) {
        let text = String::from_utf8_lossy(bytes);
        let mut text = text.trim_end_matches('\n').replace('\r', "");
        if let Some((cut, _)) = text.char_indices().nth(MAX_LINE_CHARS) {
            self.lines_truncated = true;
            text.truncate(cut);
            text.push_str("... [truncated]");
        }
        let line_number = line_number.unwrap_or_default();
        let line =
            format!("{display_path}{separator}{line_number}{separator} {text}");
        self.bytes += line.len() + usize::from(!self.lines.is_empty());
        self.lines.push(line);
    }

    /// Joins collected lines, applies pi's 50KB cap, and appends continuation notices.
    fn into_text(self) -> String {
        if self.matches == 0 {
            return "No matches found".to_string();
        }
        // The match limit already bounds rows, so only bytes are capped here.
        let truncation = truncate_head(
            &self.lines.join("\n"),
            usize::MAX,
            DEFAULT_MAX_BYTES,
        );
        let mut notices = Vec::new();
        if self.limit_reached {
            notices.push(format!(
                "{} matches limit reached. Use limit={} for more, or refine pattern",
                self.limit,
                self.limit.saturating_mul(2)
            ));
        }
        if truncation.truncated {
            notices.push(format!(
                "{} limit reached",
                format_size(DEFAULT_MAX_BYTES)
            ));
        }
        if self.lines_truncated {
            notices.push(format!(
                "Some lines truncated to {MAX_LINE_CHARS} chars. Use read tool to see full lines"
            ));
        }
        let mut text = truncation.content;
        if !notices.is_empty() {
            text.push_str(&format!("\n\n[{}]", notices.join(". ")));
        }
        text
    }
}

/// Searcher sink that renders one file's matches and context into the shared output.
struct FileSink<'a> {
    output: &'a mut GrepOutput,
    display_path: String,
    /// Context lines still owed after the final allowed match.
    trailing: usize,
}

impl FileSink<'_> {
    /// Renders one trailing line after the limit and reports whether more may follow.
    fn push_trailing(
        &mut self,
        line_number: Option<u64>,
        bytes: &[u8],
    ) -> bool {
        if self.trailing == 0 {
            return false;
        }
        self.trailing -= 1;
        self.output
            .push(&self.display_path, '-', line_number, bytes);
        self.trailing > 0 && !self.output.is_full()
    }
}

impl Sink for FileSink<'_> {
    type Error = std::io::Error;

    /// Records a match until the limit; later matches only fill the trailing context.
    fn matched(
        &mut self,
        searcher: &Searcher,
        found: &SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        if self.output.limit_reached {
            return Ok(self.push_trailing(found.line_number(), found.bytes()));
        }
        self.output.matches += 1;
        self.output.limit_reached = self.output.matches >= self.output.limit;
        self.output.push(
            &self.display_path,
            ':',
            found.line_number(),
            found.bytes(),
        );
        if self.output.limit_reached {
            self.trailing = searcher.after_context();
        }
        Ok(!self.output.is_full()
            && (!self.output.limit_reached || self.trailing > 0))
    }

    /// Records context; after the limit, only the final match's trailing window.
    fn context(
        &mut self,
        _searcher: &Searcher,
        context: &SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        // The searcher's own after-window never ends before ours, so no
        // leading context can arrive while trailing lines are still owed.
        if self.output.limit_reached {
            return Ok(
                self.push_trailing(context.line_number(), context.bytes())
            );
        }
        self.output.push(
            &self.display_path,
            '-',
            context.line_number(),
            context.bytes(),
        );
        Ok(!self.output.is_full())
    }
}

/// Pi-compatible content search backed by ripgrep's library crates.
pub(super) struct GrepTool;

#[async_trait]
impl AgentTool for GrepTool {
    /// Marks the tool read-only so plan mode keeps it available.
    fn metadata(&self) -> &protocol::ToolMetadata {
        super::read_only_metadata()
    }

    /// Returns pi's grep schema and truncation contract.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep".to_string(),
            description: format!(
                "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to {DEFAULT_LIMIT} matches or {}KB (whichever is hit first). Long lines are truncated to {MAX_LINE_CHARS} chars.",
                DEFAULT_MAX_BYTES / 1_024
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Search pattern (regex or literal string)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search (default: current directory)"
                    },
                    "glob": {
                        "type": "string",
                        "description": "Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'"
                    },
                    "ignoreCase": {
                        "type": "boolean",
                        "description": "Case-insensitive search (default: false)"
                    },
                    "literal": {
                        "type": "boolean",
                        "description": "Treat pattern as literal string instead of regex (default: false)"
                    },
                    "context": {
                        "type": "number",
                        "description": "Number of lines to show before and after each match (default: 0)"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of matches to return (default: 100)"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    /// Contributes pi's grep capability snippet.
    fn prompt_contribution(&self) -> ToolPromptContribution {
        ToolPromptContribution {
            snippet: Some(
                "Search file contents for patterns (respects .gitignore)"
                    .to_string(),
            ),
            guidelines: Vec::new(),
        }
    }

    /// Validates grep JSON, the pattern, and the glob without filesystem access.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        let arguments = GrepArguments::try_from(call)?;
        arguments.matcher()?;
        // Glob syntax does not depend on the override root, so any root checks it.
        arguments.overrides(Path::new("."))?;
        Ok(())
    }

    /// Searches the resolved path on a blocking worker and renders pi's output.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        let arguments = GrepArguments::try_from(&call)?;
        let root = ResolvedPath::new(
            arguments
                .path
                .as_deref()
                .filter(|path| !path.is_empty())
                .unwrap_or("."),
            &context.cwd,
        )?;
        let Ok(metadata) = tokio::fs::metadata(root.as_path()).await else {
            tracing::warn!("grep path not found: {}", root.as_path().display());
            return Err(ToolError::Execution {
                tool: call.name,
                message: format!(
                    "Path not found: {}",
                    root.as_path().display()
                ),
            });
        };
        let search = GrepSearch::builder()
            .matcher(arguments.matcher()?)
            // Collecting components drops `.` segments, which would otherwise
            // leave `./` in walked paths and break cwd-anchored glob overrides.
            .root(root.as_path().components().collect())
            .root_is_dir(metadata.is_dir())
            .overrides(arguments.overrides(&context.cwd)?)
            .context(arguments.context_lines())
            .limit(arguments.match_limit())
            .build();
        let cancellation = context.cancellation.clone();
        let output =
            tokio::task::spawn_blocking(move || search.run(&cancellation))
                .await
                .map_err(|error| {
                    tracing::warn!("grep worker failed: {}", error);
                    ToolError::Execution {
                        tool: call.name.clone(),
                        message: format!("grep worker failed: {error}"),
                    }
                })??;
        context.ensure_active()?;
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text {
                text: output.into_text(),
            }])
            .is_error(false)
            .build())
    }
}
