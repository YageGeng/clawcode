use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use serde::Deserialize;
use snafu::ResultExt;

#[cfg(unix)]
use libc;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::session::SessionId;
use crate::{
    Result,
    error::{JsonSnafu, ToolExecutionSnafu, ToolIoSnafu},
    tools::{ApprovalRequirement, RiskLevel, Tool, ToolContext, ToolMetadata, ToolOutput},
};

const DEFAULT_FILE_READ_MAX_BYTES: usize = 1024 * 1024;
const DEFAULT_FILE_WRITE_MAX_BYTES: usize = 1024 * 1024;

/// Normalizes `.` path segments lexically without touching the filesystem.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            // Preserve `..` so the caller can reject traversal explicitly.
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }

    normalized
}

/// Resolves a non-namespaced tool request to a normalized path under the canonical root.
fn resolve_relative_tool_request(
    root_dir: &Path,
    requested: &str,
    tool: &'static str,
    stage: &'static str,
) -> Result<(PathBuf, PathBuf)> {
    if requested.trim().is_empty() {
        return ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: stage.to_string(),
            message: "path must not be empty".to_string(),
        }
        .fail();
    }

    require_relative_path(requested, tool, stage)?;

    let request_path = Path::new(requested);
    if request_path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: stage.to_string(),
            message: "path traversal via `..` is not allowed".to_string(),
        }
        .fail();
    }

    let canonical_root = root_dir.canonicalize().context(ToolIoSnafu {
        tool: tool.to_string(),
        stage: format!("{}-root-canonicalize", stage.trim_end_matches("-resolve")),
    })?;
    let normalized_candidate = normalize_lexical(&canonical_root.join(request_path));

    Ok((canonical_root, normalized_candidate))
}

#[derive(Debug, Deserialize)]
struct ReadFileArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
struct ReadTextFileArgs {
    path: String,
    line: Option<usize>,
    limit: Option<usize>,
    #[serde(alias = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "_meta")]
    meta: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct WriteFileArgs {
    path: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct WriteTextFileArgs {
    path: String,
    content: String,
    #[serde(alias = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "_meta")]
    meta: Option<serde_json::Value>,
}

/// Reads UTF-8 file contents from a safe path under a configured root directory.
pub struct ReadFileTool {
    root_dir: PathBuf,
    max_bytes: usize,
}

impl ReadFileTool {
    /// Creates a read tool with default max size.
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self::with_max_bytes(root_dir, DEFAULT_FILE_READ_MAX_BYTES)
    }

    /// Creates a read tool with an explicit max size.
    pub fn with_max_bytes(root_dir: impl Into<PathBuf>, max_bytes: usize) -> Self {
        Self {
            root_dir: root_dir.into(),
            max_bytes,
        }
    }

    /// Builds a tool-execution error with a shared format.
    fn tool_error(&self, stage: &'static str, message: impl Into<String>) -> Result<PathBuf> {
        ToolExecutionSnafu {
            tool: self.name().to_string(),
            stage: stage.to_string(),
            message: message.into(),
        }
        .fail()
    }

    /// Reads bytes from the target file with best-effort TOCTOU hardening.
    fn read_safe_text(&self, path: &Path) -> Result<String> {
        #[cfg(unix)]
        {
            let mut file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
                .context(ToolIoSnafu {
                    tool: self.name().to_string(),
                    stage: "read-file-open".to_string(),
                })?;

            let mut text = String::new();
            file.read_to_string(&mut text).context(ToolIoSnafu {
                tool: self.name().to_string(),
                stage: "read-file-open".to_string(),
            })?;

            Ok(text)
        }

        #[cfg(not(unix))]
        {
            fs::read_to_string(path).context(ToolIoSnafu {
                tool: self.name().to_string(),
                stage: "read-file-open".to_string(),
            })
        }
    }

    /// Applies optional line-based slicing for ACP-style text reads.
    fn select_text_lines(
        &self,
        text: &str,
        line: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String> {
        let lines = text.split('\n').collect::<Vec<_>>();

        let start = match line {
            Some(0) => {
                self.tool_error(
                    "read-file-parse-args",
                    "line must be 1-based and greater than 0",
                )?;
                0
            }
            Some(start) => start - 1,
            None => 0,
        };

        if start >= lines.len() && !lines.is_empty() {
            self.tool_error(
                "read-file-parse-args",
                "requested line is outside available file range",
            )?;
        }

        let end = match limit {
            Some(limit) => std::cmp::min(start + limit, lines.len()),
            None => lines.len(),
        };

        let selected = &lines[start..end];
        Ok(selected.join("\n"))
    }

    /// Ensures a requested path is within the configured root.
    fn resolve_path(&self, requested: &str) -> Result<PathBuf> {
        let (root_dir, resolved) = resolve_relative_tool_request(
            &self.root_dir,
            requested,
            self.name(),
            "read-file-resolve",
        )?;

        if !resolved.starts_with(&root_dir) {
            self.tool_error("read-file-resolve", "target must stay inside the tool root")?;
        }

        let resolved = resolved.canonicalize().context(ToolIoSnafu {
            tool: self.name().to_string(),
            stage: "read-file-canonicalize".to_string(),
        })?;

        if !resolved.starts_with(&root_dir) {
            self.tool_error(
                "read-file-resolve",
                "canonicalized target must stay inside the tool root",
            )?;
        }

        Ok(resolved)
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "file_read"
    }

    fn description(&self) -> &'static str {
        "Read a UTF-8 file from a SAFE RELATIVE path under the configured root. Use this for full-file reads when you want the entire file content. If you need only specific lines or a partial read, use `read_text_file` instead because this tool does NOT support `line`, `offset`, or `limit`."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path to the file to read inside the tool root. Returns the full UTF-8 file content."
                }
            },
            "required": ["path"]
        })
    }

    /// Resolves, validates, and reads file content in UTF-8.
    async fn execute(&self, args: serde_json::Value, _context: ToolContext) -> Result<ToolOutput> {
        let args: ReadFileArgs = serde_json::from_value(args).context(JsonSnafu {
            stage: "read-file-parse-args".to_string(),
        })?;

        let resolved = self.resolve_path(&args.path)?;
        let metadata = fs::metadata(&resolved).context(ToolIoSnafu {
            tool: self.name().to_string(),
            stage: "read-file-meta".to_string(),
        })?;

        if !metadata.is_file() {
            self.tool_error("read-file-meta", "target path is not a regular file")?;
        }
        if metadata.len() as usize > self.max_bytes {
            self.tool_error(
                "read-file-meta",
                format!("file exceeds size limit: {} bytes", self.max_bytes),
            )?;
        }

        let text = self.read_safe_text(&resolved)?;

        Ok(ToolOutput {
            text: text.clone(),
            structured: serde_json::json!({
                "path": resolved.to_string_lossy(),
                "bytes": text.len(),
                "status": "ok",
            }),
        })
    }
}

/// Writes UTF-8 file content to a safe path under a configured root directory.
pub struct WriteFileTool {
    root_dir: PathBuf,
    max_bytes: usize,
}

impl WriteFileTool {
    /// Creates a write tool with default max size.
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self::with_max_bytes(root_dir, DEFAULT_FILE_WRITE_MAX_BYTES)
    }

    /// Creates a write tool with an explicit max size.
    pub fn with_max_bytes(root_dir: impl Into<PathBuf>, max_bytes: usize) -> Self {
        Self {
            root_dir: root_dir.into(),
            max_bytes,
        }
    }

    /// Builds a tool-execution error with a shared format.
    fn tool_error(&self, stage: &'static str, message: impl Into<String>) -> Result<PathBuf> {
        ToolExecutionSnafu {
            tool: self.name().to_string(),
            stage: stage.to_string(),
            message: message.into(),
        }
        .fail()
    }

    /// Resolves non-existent targets against the nearest existing ancestor so symlinks
    /// above a new file cannot escape the configured root.
    fn resolve_against_existing_ancestor(&self, path: &Path) -> Result<PathBuf> {
        let mut ancestor = path;
        let mut tail_parts = Vec::new();

        loop {
            if ancestor.exists() {
                let canonical_ancestor = ancestor.canonicalize().context(ToolIoSnafu {
                    tool: self.name().to_string(),
                    stage: "write-file-parent-canonicalize".to_string(),
                })?;
                let mut resolved = canonical_ancestor;
                for part in tail_parts.iter().rev() {
                    resolved.push(part);
                }
                return Ok(resolved);
            }

            if let Some(name) = ancestor.file_name() {
                tail_parts.push(name.to_os_string());
            }

            match ancestor.parent() {
                Some(parent) if parent != ancestor => ancestor = parent,
                _ => return Ok(path.to_path_buf()),
            }
        }
    }

    /// Verifies the parent path is safe and free of symlink escapes.
    fn verify_parent_path(&self, parent: &Path, root_dir: &Path) -> Result<()> {
        let relative_parent = match parent.strip_prefix(root_dir) {
            Ok(relative_parent) => relative_parent,
            Err(_) => {
                return ToolExecutionSnafu {
                    tool: self.name().to_string(),
                    stage: "write-file-validate-parent".to_string(),
                    message: "target parent must stay inside the tool root".to_string(),
                }
                .fail();
            }
        };

        let mut cursor = root_dir.to_path_buf();
        for component in relative_parent.components() {
            cursor = cursor.join(component);
            if !cursor.exists() {
                break;
            }

            let metadata = fs::symlink_metadata(&cursor).context(ToolIoSnafu {
                tool: self.name().to_string(),
                stage: "write-file-parent-meta".to_string(),
            })?;

            if metadata.file_type().is_symlink() {
                self.tool_error(
                    "write-file-resolve",
                    "path traversal through symlink is not allowed",
                )?;
            }

            if !metadata.is_dir() {
                self.tool_error("write-file-resolve", "path component is not a directory")?;
            }
        }

        Ok(())
    }

    /// Writes text content with best-effort symlink avoidance.
    fn write_safe_text(&self, path: &Path, content: &[u8]) -> Result<()> {
        #[cfg(unix)]
        {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
                .context(ToolIoSnafu {
                    tool: self.name().to_string(),
                    stage: "write-file-open".to_string(),
                })?;

            file.write_all(content).context(ToolIoSnafu {
                tool: self.name().to_string(),
                stage: "write-file-open".to_string(),
            })?;

            Ok(())
        }

        #[cfg(not(unix))]
        {
            fs::write(path, content).context(ToolIoSnafu {
                tool: self.name().to_string(),
                stage: "write-file-open".to_string(),
            })
        }
    }

    /// Ensures a requested path is within the configured root.
    fn resolve_path(&self, requested: &str) -> Result<PathBuf> {
        let (root_dir, resolved) = resolve_relative_tool_request(
            &self.root_dir,
            requested,
            self.name(),
            "write-file-resolve",
        )?;
        let containment_path = if resolved.exists() {
            resolved.canonicalize().context(ToolIoSnafu {
                tool: self.name().to_string(),
                stage: "write-file-canonicalize".to_string(),
            })?
        } else {
            self.resolve_against_existing_ancestor(&resolved)?
        };

        if !containment_path.starts_with(&root_dir) {
            self.tool_error(
                "write-file-resolve",
                "target must stay inside the tool root",
            )?;
        }

        let parent = match resolved.parent() {
            Some(parent) => parent,
            None => {
                return ToolExecutionSnafu {
                    tool: self.name().to_string(),
                    stage: "write-file-resolve".to_string(),
                    message: "target must have a parent path".to_string(),
                }
                .fail();
            }
        };

        if parent.exists() {
            let canonical_parent = parent.canonicalize().context(ToolIoSnafu {
                tool: self.name().to_string(),
                stage: "write-file-parent-canonicalize".to_string(),
            })?;

            if !canonical_parent.starts_with(&root_dir) {
                self.tool_error(
                    "write-file-resolve",
                    "target parent must stay inside the tool root",
                )?;
            }
        }

        self.verify_parent_path(parent, &root_dir)?;

        if resolved.exists() {
            let target_metadata = fs::symlink_metadata(&resolved).context(ToolIoSnafu {
                tool: self.name().to_string(),
                stage: "write-file-target-meta".to_string(),
            })?;

            if target_metadata.file_type().is_symlink() {
                self.tool_error("write-file-resolve", "target path must not be a symlink")?;
            }
        }

        Ok(resolved)
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "file_write"
    }

    fn description(&self) -> &'static str {
        "Write UTF-8 content to a SAFE RELATIVE path under the configured root. Use this for direct file creation or replacement when the user explicitly asks to write a file. Nested relative paths such as `./doc/example.md` are allowed, and missing parent directories are created automatically. Paths containing `..` are not allowed."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path to write under the tool root. Nested relative paths like `./doc/example.md` are allowed, and missing parent directories are created automatically. Do not use `..`."
                },
                "content": {
                    "type": "string",
                    "description": "UTF-8 content to write to the target file."
                }
            },
            "required": ["path", "content"]
        })
    }

    /// Uses higher risk metadata because this tool mutates disk state.
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            risk_level: RiskLevel::High,
            approval: ApprovalRequirement::Always,
            timeout: std::time::Duration::from_secs(30),
        }
    }

    /// Resolves, validates, and writes file content.
    async fn execute(&self, args: serde_json::Value, _context: ToolContext) -> Result<ToolOutput> {
        let args: WriteFileArgs = serde_json::from_value(args).context(JsonSnafu {
            stage: "write-file-parse-args".to_string(),
        })?;

        if args.content.len() > self.max_bytes {
            self.tool_error(
                "write-file-execute",
                format!("content exceeds size limit: {} bytes", self.max_bytes),
            )?;
        }

        let path = self.resolve_path(&args.path)?;
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).context(ToolIoSnafu {
                    tool: self.name().to_string(),
                    stage: "write-file-create-parent".to_string(),
                })?;
            }
        }

        self.write_safe_text(&path, args.content.as_bytes())?;

        Ok(ToolOutput {
            text: format!(
                "wrote {} bytes to {}",
                args.content.len(),
                path.to_string_lossy()
            ),
            structured: serde_json::json!({
                "path": path.to_string_lossy(),
                "bytes_written": args.content.len(),
                "status": "ok",
            }),
        })
    }
}

/// ACP/MCP-compatible alias for reading text files.
pub struct ReadTextFileTool {
    inner: ReadFileTool,
}

impl ReadTextFileTool {
    /// Creates the alias tool backed by the same validation and read behavior.
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            inner: ReadFileTool::new(root_dir),
        }
    }
}

#[async_trait]
impl Tool for ReadTextFileTool {
    fn name(&self) -> &'static str {
        "read_text_file"
    }

    fn description(&self) -> &'static str {
        "Read UTF-8 text from a SAFE RELATIVE path with optional line-based slicing. Use this when you need only part of a file, such as the first line or a bounded range of lines. Prefer this tool over `file_read` whenever the request mentions specific lines, a partial read, or a limit."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Relative path to the UTF-8 text file to read inside the tool root."
            },
            "line": {
                "type": "integer",
                "minimum": 1,
                "description": "Optional 1-based starting line. Use `line: 1, limit: 1` to read only the first line."
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "description": "Optional maximum number of lines to return starting from `line`. Use this for partial reads instead of `file_read`."
            },
            "session_id": {
                "type": "string",
                "description": "Optional opaque session identifier carried for tool-call correlation."
            },
            "sessionId": {
                "type": "string",
                "description": "Alias for `session_id` in camelCase protocol payloads."
            },
            "_meta": {
                "type": "object",
                "description": "Reserved metadata for protocol envelopes."
            }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        let args: ReadTextFileArgs = serde_json::from_value(args).context(JsonSnafu {
            stage: "read-text-file-parse-args".to_string(),
        })?;
        let file_output = self
            .inner
            .execute(
                serde_json::json!({
                    "path": args.path,
                }),
                context,
            )
            .await?;

        let selected = self
            .inner
            .select_text_lines(&file_output.text, args.line, args.limit)?;

        Ok(ToolOutput {
            text: selected.clone(),
            structured: {
                let mut structured = serde_json::json!({
                    "path": file_output.structured["path"].clone(),
                    "bytes": selected.len(),
                    "status": file_output.structured["status"].clone(),
                    "line": args.line,
                    "limit": args.limit,
                    "session_id": args.session_id,
                });
                append_meta_if_present(&mut structured, args.meta);
                structured
            },
        })
    }
}

/// ACP namespaced alias for reading text files.
pub struct FsReadTextFileTool {}

/// Validates that a namespaced text read path is absolute and path-safe.
fn require_absolute_path(path: &str, tool: &'static str, stage: &'static str) -> Result<()> {
    let requested = Path::new(path);
    if path.trim().is_empty() {
        return ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: stage.to_string(),
            message: "path must not be empty".to_string(),
        }
        .fail();
    }

    if !requested.is_absolute() {
        return ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: stage.to_string(),
            message: "path must be an absolute filesystem path".to_string(),
        }
        .fail();
    }

    if requested
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: stage.to_string(),
            message: "path traversal via `..` is not allowed".to_string(),
        }
        .fail();
    }

    Ok(())
}

/// Ensures a non-namespaced file tool only accepts relative paths under its configured root.
fn require_relative_path(path: &str, tool: &'static str, stage: &'static str) -> Result<()> {
    let requested = Path::new(path);
    if requested.is_absolute() {
        return ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: stage.to_string(),
            message: "absolute paths are not allowed; use the namespaced fs/* tool for absolute filesystem access"
                .to_string(),
        }
        .fail();
    }

    Ok(())
}

/// Converts a validated absolute filesystem path into a root-relative path for tools
/// that are sandboxed at `/` internally.
fn absolute_path_to_root_relative(
    path: &str,
    tool: &'static str,
    stage: &'static str,
) -> Result<String> {
    let requested = Path::new(path);
    let relative = match requested.strip_prefix(Path::new("/")) {
        Ok(relative) => relative,
        Err(_) => {
            return ToolExecutionSnafu {
                tool: tool.to_string(),
                message: "absolute path must be rooted at '/'".to_string(),
                stage: stage.to_string(),
            }
            .fail();
        }
    };

    Ok(relative.to_string_lossy().to_string())
}

/// Requires a session identifier for namespaced file operations.
fn require_session_id(
    session_id: Option<String>,
    tool: &'static str,
    stage: &'static str,
) -> Result<String> {
    let session_id = match session_id {
        Some(session_id) => session_id,
        None => {
            return ToolExecutionSnafu {
                tool: tool.to_string(),
                stage: stage.to_string(),
                message: "sessionId is required".to_string(),
            }
            .fail();
        }
    };

    if session_id.trim().is_empty() {
        return ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: stage.to_string(),
            message: "sessionId must not be empty".to_string(),
        }
        .fail();
    }

    Ok(session_id)
}

/// Ensures the provided session ID matches the runtime tool context session.
fn require_runtime_session_match(
    session_id: &str,
    context_session_id: &SessionId,
    tool: &'static str,
    stage: &'static str,
) -> Result<()> {
    if session_id != context_session_id.to_string() {
        return ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: stage.to_string(),
            message: "sessionId does not match runtime session context".to_string(),
        }
        .fail();
    }

    Ok(())
}

/// Injects protocol metadata into tool outputs when present.
fn append_meta_if_present(structured: &mut serde_json::Value, meta: Option<serde_json::Value>) {
    if let Some(meta) = meta {
        if let Some(map) = structured.as_object_mut() {
            map.insert("_meta".to_string(), meta);
        }
    }
}

impl FsReadTextFileTool {
    /// Creates the namespaced read alias.
    pub fn new(_root_dir: impl Into<PathBuf>) -> Self {
        Self {}
    }
}

#[async_trait]
impl Tool for FsReadTextFileTool {
    fn name(&self) -> &'static str {
        "fs/read_text_file"
    }

    fn description(&self) -> &'static str {
        "Read UTF-8 text from an ABSOLUTE filesystem path with optional line-based slicing. Use this only for absolute-path ACP/MCP reads. If the path is relative, use `read_text_file` or `file_read` instead."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the UTF-8 text file to read from the local filesystem."
                },
                "line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional 1-based starting line. Use `line: 1, limit: 1` to read only the first line."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional maximum number of lines to return starting from `line` for partial absolute-path reads."
                },
                "session_id": {
                    "type": "string",
                    "description": "Canonical session identifier carried for tool-call correlation. Runtime also accepts `sessionId` as an input alias."
                }
        },
            "required": ["path", "session_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        let args: ReadTextFileArgs = serde_json::from_value(args).context(JsonSnafu {
            stage: "fs-read-text-file-parse-args".to_string(),
        })?;
        let session_id =
            require_session_id(args.session_id, self.name(), "fs-read-text-file-parse-args")?;
        require_runtime_session_match(
            &session_id,
            &context.session_id,
            self.name(),
            "fs-read-text-file-parse-args",
        )?;
        require_absolute_path(&args.path, self.name(), "fs-read-text-file-parse-args")?;
        let inner_relative_path = absolute_path_to_root_relative(
            &args.path,
            self.name(),
            "fs-read-text-file-parse-args",
        )?;

        let inner = ReadFileTool::new("/");
        let file_output = inner
            .execute(
                serde_json::json!({
                    "path": inner_relative_path,
                }),
                context,
            )
            .await?;

        let selected = inner.select_text_lines(&file_output.text, args.line, args.limit)?;

        Ok(ToolOutput {
            text: selected.clone(),
            structured: {
                let mut structured = serde_json::json!({
                    "path": file_output.structured["path"].clone(),
                    "bytes": selected.len(),
                    "status": file_output.structured["status"].clone(),
                    "line": args.line,
                    "limit": args.limit,
                    "session_id": session_id,
                });
                append_meta_if_present(&mut structured, args.meta);
                structured
            },
        })
    }
}

/// ACP/MCP-compatible alias for writing text files.
pub struct WriteTextFileTool {
    inner: WriteFileTool,
}

impl WriteTextFileTool {
    /// Creates the alias tool backed by the same validation and write behavior.
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            inner: WriteFileTool::new(root_dir),
        }
    }
}

#[async_trait]
impl Tool for WriteTextFileTool {
    fn name(&self) -> &'static str {
        "write_text_file"
    }

    fn description(&self) -> &'static str {
        "Write UTF-8 text to a SAFE RELATIVE path under the configured root. Use this when the user explicitly asks to write a text file and you need the text-file alias surface. Nested relative paths such as `./doc/example.md` are allowed, and missing parent directories are created automatically. Paths containing `..` are not allowed."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Relative path to write under the tool root. Nested relative paths like `./doc/example.md` are allowed, and missing parent directories are created automatically. Do not use `..`."
            },
            "content": {
                "type": "string",
                "description": "UTF-8 content to write to the target file."
            },
            "session_id": {
                "type": "string",
                "description": "Optional opaque session identifier carried for tool-call correlation."
            },
            "sessionId": {
                "type": "string",
                "description": "Alias for `session_id` in camelCase protocol payloads."
            },
            "_meta": {
                "type": "object",
                "description": "Reserved metadata for protocol envelopes."
            }
            },
            "required": ["path", "content"]
        })
    }

    fn metadata(&self) -> ToolMetadata {
        self.inner.metadata()
    }

    async fn execute(&self, args: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        let args: WriteTextFileArgs = serde_json::from_value(args).context(JsonSnafu {
            stage: "write-text-file-parse-args".to_string(),
        })?;
        let session_id = args.session_id.clone();
        let structured_args = serde_json::json!({
            "path": args.path,
            "content": args.content,
            "session_id": session_id,
        });
        let mut output = self.inner.execute(structured_args, context).await?;
        append_meta_if_present(&mut output.structured, args.meta);

        match output.structured.as_object_mut() {
            Some(map) => {
                map.insert("session_id".to_string(), serde_json::json!(session_id));
            }
            None => {
                output.structured = serde_json::json!({"session_id": session_id});
            }
        }

        Ok(output)
    }
}

/// ACP namespaced alias for writing text files.
pub struct FsWriteTextFileTool {}

impl FsWriteTextFileTool {
    /// Creates the namespaced write alias.
    pub fn new(_root_dir: impl Into<PathBuf>) -> Self {
        Self {}
    }
}

#[async_trait]
impl Tool for FsWriteTextFileTool {
    fn name(&self) -> &'static str {
        "fs/write_text_file"
    }

    fn description(&self) -> &'static str {
        "Writes UTF-8 content to an absolute filesystem path."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to write into the local filesystem."
                },
                "content": {
                    "type": "string",
                    "description": "UTF-8 content to write to the target file."
                },
                "session_id": {
                    "type": "string",
                    "description": "Canonical session identifier carried for tool-call correlation. Runtime also accepts `sessionId` as an input alias."
                }
            },
            "required": ["path", "content", "session_id"]
        })
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            risk_level: RiskLevel::High,
            approval: ApprovalRequirement::Always,
            timeout: std::time::Duration::from_secs(30),
        }
    }

    async fn execute(&self, args: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        let args: WriteTextFileArgs = serde_json::from_value(args).context(JsonSnafu {
            stage: "fs-write-text-file-parse-args".to_string(),
        })?;
        let session_id = require_session_id(
            args.session_id,
            self.name(),
            "fs-write-text-file-parse-args",
        )?;
        require_runtime_session_match(
            &session_id,
            &context.session_id,
            self.name(),
            "fs-write-text-file-parse-args",
        )?;
        require_absolute_path(&args.path, self.name(), "fs-write-text-file-parse-args")?;
        let inner_relative_path = absolute_path_to_root_relative(
            &args.path,
            self.name(),
            "fs-write-text-file-parse-args",
        )?;

        let inner = WriteFileTool::new("/");
        let mut output = inner
            .execute(
                serde_json::json!({
                        "path": inner_relative_path,
                    "content": args.content,
                    "session_id": session_id,
                }),
                context,
            )
            .await?;
        append_meta_if_present(&mut output.structured, args.meta);

        match output.structured.as_object_mut() {
            Some(map) => {
                map.insert("session_id".to_string(), serde_json::json!(session_id));
            }
            None => {
                output.structured = serde_json::json!({"session_id": session_id});
            }
        }

        Ok(output)
    }
}

/// Builds the default read-only tool set.
///
/// The temporary built-in read-only demo tools (`echo`, `json`, `time`) were removed,
/// so the default kernel surface currently contributes no standalone read-only tools.
pub fn default_read_only_tools() -> Vec<Arc<dyn Tool>> {
    vec![]
}

/// Builds the default file tool set rooted at `.`.
pub fn default_file_tools() -> Vec<Arc<dyn Tool>> {
    default_file_tools_with_root(PathBuf::from("."))
}

/// Builds the file-tool set rooted at the provided directory.
pub fn default_file_tools_with_root(root_dir: impl Into<PathBuf>) -> Vec<Arc<dyn Tool>> {
    let root_dir = root_dir.into();
    vec![
        Arc::new(ReadFileTool::new(root_dir.clone())),
        Arc::new(WriteFileTool::new(root_dir.clone())),
        Arc::new(ReadTextFileTool::new(root_dir.clone())),
        Arc::new(WriteTextFileTool::new(root_dir.clone())),
        Arc::new(FsReadTextFileTool::new(root_dir.clone())),
        Arc::new(FsWriteTextFileTool::new(root_dir)),
    ]
}
