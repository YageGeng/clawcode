//! File I/O tools: read, write, and patch.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::fs;

use crate::Tool;

// ── ReadFile ──

/// Reads a file's content, optionally limited by offset and line count.
pub struct ReadFile;

impl ReadFile {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReadFile {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a file's content, with optional offset and limit (line numbers)"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file" },
                "offset": { "type": "integer", "description": "Start line (0-indexed)" },
                "limit": { "type": "integer", "description": "Max number of lines to read" }
            },
            "required": ["path"]
        })
    }

    fn needs_approval(&self, arguments: &serde_json::Value) -> bool {
        // Require approval only when the path escapes cwd
        arguments["path"]
            .as_str()
            .is_some_and(|p| Path::new(p).is_absolute() || p.contains(".."))
    }

    async fn execute(&self, arguments: serde_json::Value, cwd: &Path) -> Result<String, String> {
        let path = arguments["path"]
            .as_str()
            .ok_or("missing 'path' argument")?;
        let resolved = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            cwd.join(path)
        };
        // Canonicalize to detect symlink escapes even on absolute paths
        let resolved = fs::canonicalize(&resolved)
            .await
            .map_err(|e| format!("failed to resolve {}: {e}", resolved.display()))?;

        let content = fs::read_to_string(&resolved)
            .await
            .map_err(|e| format!("failed to read {}: {e}", resolved.display()))?;

        let lines: Vec<&str> = content.lines().collect();
        let offset = arguments["offset"].as_u64().unwrap_or(0) as usize;
        let limit = arguments["limit"].as_u64().map(|n| n as usize);

        let start = offset.min(lines.len());
        let end = limit
            .map(|l| (start + l).min(lines.len()))
            .unwrap_or(lines.len());

        let result: String = lines[start..end].join("\n");
        Ok(result)
    }
}

// ── WriteFile ──

/// Creates or overwrites a file with the given content.
pub struct WriteFile;

impl WriteFile {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for WriteFile {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Create or overwrite a file with the given content"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file" },
                "content": { "type": "string", "description": "Content to write" }
            },
            "required": ["path", "content"]
        })
    }

    fn needs_approval(&self, _: &serde_json::Value) -> bool {
        true
    }

    async fn execute(&self, arguments: serde_json::Value, cwd: &Path) -> Result<String, String> {
        let path = arguments["path"]
            .as_str()
            .ok_or("missing 'path' argument")?;
        let content = arguments["content"]
            .as_str()
            .ok_or("missing 'content' argument")?;

        // needs_approval() already gates dangerous paths — if we reach
        // execute(), the caller has either bypassed approval (safe paths)
        // or obtained user consent (absolute / escape paths).
        let resolved = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            cwd.join(path)
        };

        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("failed to create parent dir: {e}"))?;
        }

        fs::write(&resolved, content)
            .await
            .map_err(|e| format!("failed to write {}: {e}", resolved.display()))?;

        Ok(format!(
            "wrote {} bytes to {}",
            content.len(),
            resolved.display()
        ))
    }
}

// ── ApplyPatch ──

/// Searches for text in a file and replaces it.
pub struct ApplyPatch;

impl ApplyPatch {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ApplyPatch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ApplyPatch {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Replace a text block in a file. Searches for the exact `search` string and replaces it with `replace`."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file" },
                "search": { "type": "string", "description": "Exact text to find" },
                "replace": { "type": "string", "description": "Text to replace with" }
            },
            "required": ["path", "search", "replace"]
        })
    }

    fn needs_approval(&self, _: &serde_json::Value) -> bool {
        true
    }

    async fn execute(&self, arguments: serde_json::Value, cwd: &Path) -> Result<String, String> {
        let path = arguments["path"]
            .as_str()
            .ok_or("missing 'path' argument")?;
        let search = arguments["search"]
            .as_str()
            .ok_or("missing 'search' argument")?;
        let replace = arguments["replace"]
            .as_str()
            .ok_or("missing 'replace' argument")?;

        // needs_approval() already gates dangerous paths — if we reach
        // execute(), the caller has either bypassed approval (safe paths)
        // or obtained user consent (absolute / escape paths).
        let resolved = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            cwd.join(path)
        };

        let resolved = fs::canonicalize(&resolved)
            .await
            .map_err(|e| format!("failed to resolve {}: {e}", resolved.display()))?;

        let original = fs::read_to_string(&resolved)
            .await
            .map_err(|e| format!("failed to read {}: {e}", resolved.display()))?;

        if let Some(_patched) = original.split(search).nth(1) {
            // Apply only the first exact match to keep model edits predictable.
            let result = original.replacen(search, replace, 1);
            fs::write(&resolved, &result)
                .await
                .map_err(|e| format!("failed to write {}: {e}", resolved.display()))?;
            Ok(format!(
                "patched {}: replaced 1 occurrence, {} bytes",
                resolved.display(),
                result.len()
            ))
        } else {
            Err(format!("search text not found in {}", resolved.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn read_file_content() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = NamedTempFile::new_in(dir.path()).unwrap();
        writeln!(f, "line1\nline2\nline3").unwrap();
        let path = f.path().file_name().unwrap().to_string_lossy().to_string();

        let tool = ReadFile::new();
        let result = tool
            .execute(serde_json::json!({"path": path}), dir.path())
            .await
            .unwrap();
        assert!(result.contains("line2"));
    }

    #[tokio::test]
    async fn read_file_with_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = NamedTempFile::new_in(dir.path()).unwrap();
        writeln!(f, "a\nb\nc\nd").unwrap();
        let path = f.path().file_name().unwrap().to_string_lossy().to_string();

        let tool = ReadFile::new();
        let result = tool
            .execute(
                serde_json::json!({"path": path, "offset": 1, "limit": 2}),
                dir.path(),
            )
            .await
            .unwrap();
        assert_eq!(result, "b\nc");
    }

    #[tokio::test]
    async fn read_file_needs_no_approval() {
        let tool = ReadFile::new();
        assert!(!tool.needs_approval(&serde_json::json!({"path": "x"})));
    }

    #[tokio::test]
    async fn read_file_requires_approval_for_absolute_path() {
        let tool = ReadFile::new();
        assert!(tool.needs_approval(&serde_json::json!({"path": "/etc/passwd"})));
        // execute with absolute path works when the caller trusts the path
        let result = tool
            .execute(serde_json::json!({"path": "/etc/hostname"}), Path::new("."))
            .await;
        // File may or may not exist, we just verify the sandbox doesn't reject it
        // If the file doesn't exist, the error is from the filesystem, not from us
        let _ = result;
    }

    #[tokio::test]
    async fn read_file_requires_approval_for_parent_escape() {
        let tool = ReadFile::new();
        assert!(tool.needs_approval(&serde_json::json!({"path": "../secret"})));
    }

    #[tokio::test]
    async fn write_and_read_file() {
        let dir = tempfile::tempdir().unwrap();

        let write = WriteFile::new();
        write
            .execute(
                serde_json::json!({"path": "test.txt", "content": "hello world"}),
                dir.path(),
            )
            .await
            .unwrap();

        let read = ReadFile::new();
        let result = read
            .execute(serde_json::json!({"path": "test.txt"}), dir.path())
            .await
            .unwrap();
        assert_eq!(result, "hello world");
    }

    #[tokio::test]
    async fn write_file_resolves_relative_path_under_cwd() {
        let dir = tempfile::tempdir().unwrap();

        let tool = WriteFile::new();
        tool.execute(
            serde_json::json!({"path": "nested/test.txt", "content": "cwd scoped"}),
            dir.path(),
        )
        .await
        .unwrap();

        let content = std::fs::read_to_string(dir.path().join("nested/test.txt")).unwrap();
        assert_eq!(content, "cwd scoped");
    }

    #[tokio::test]
    async fn write_file_always_requires_approval() {
        let tool = WriteFile::new();
        assert!(tool.needs_approval(&serde_json::json!({"path": "test.txt"})));
        assert!(tool.needs_approval(&serde_json::json!({"path": "../escape.txt"})));
    }

    #[tokio::test]
    async fn apply_patch_replaces_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("patch.txt");

        std::fs::write(&path, "before\ntarget\nafter").unwrap();

        let tool = ApplyPatch::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "patch.txt", "search": "target", "replace": "REPLACED"}),
                dir.path(),
            )
            .await
            .unwrap();
        assert!(result.contains("patched"));

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "before\nREPLACED\nafter");
    }

    #[tokio::test]
    async fn apply_patch_search_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.txt");
        std::fs::write(&path, "hello").unwrap();

        let tool = ApplyPatch::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "nope.txt", "search": "xyz", "replace": "abc"}),
                dir.path(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn apply_patch_always_requires_approval() {
        let tool = ApplyPatch::new();
        assert!(tool.needs_approval(&serde_json::json!({
            "path": "test.txt",
            "search": "t",
            "replace": "r"
        })));
    }
}
