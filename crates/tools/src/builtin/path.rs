use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization as _;

use crate::ToolError;

const NARROW_NO_BREAK_SPACE: char = '\u{202f}';

/// Fully resolved local path using pi's model-input normalization rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedPath(PathBuf);

impl ResolvedPath {
    /// Resolves relative, absolute, tilde, file URL, Unicode-space, and `@` paths.
    pub(super) fn new(input: &str, cwd: &Path) -> Result<Self, ToolError> {
        let mut normalized = input
            .chars()
            .map(|character| match character {
                '\u{00a0}'
                | '\u{2000}'..='\u{200a}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}' => ' ',
                other => other,
            })
            .collect::<String>();
        if let Some(stripped) = normalized.strip_prefix('@') {
            normalized = stripped.to_string();
        }
        if normalized == "~" || normalized.starts_with("~/") {
            let home =
                dirs::home_dir().ok_or_else(|| ToolError::Execution {
                    tool: "path".to_string(),
                    message: "home directory is unavailable".to_string(),
                })?;
            normalized = if normalized == "~" {
                home.to_string_lossy().into_owned()
            } else {
                home.join(normalized.trim_start_matches("~/"))
                    .to_string_lossy()
                    .into_owned()
            };
        } else if normalized.starts_with("file://") {
            normalized = url::Url::parse(&normalized)
                .ok()
                .and_then(|url| url.to_file_path().ok())
                .ok_or_else(|| ToolError::Execution {
                    tool: "path".to_string(),
                    message: format!("invalid file URL: {input}"),
                })?
                .to_string_lossy()
                .into_owned();
        }
        let path = PathBuf::from(normalized);
        Ok(Self(if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }))
    }

    /// Resolves a readable path and tries pi's macOS screenshot-name variants.
    pub(super) async fn for_read(
        input: &str,
        cwd: &Path,
    ) -> Result<Self, ToolError> {
        let resolved = Self::new(input, cwd)?;
        if tokio::fs::try_exists(&resolved.0).await.map_err(|error| {
            ToolError::Execution {
                tool: "read".to_string(),
                message: error.to_string(),
            }
        })? {
            return Ok(resolved);
        }

        let raw = resolved.0.to_string_lossy();
        let mut candidates = Vec::new();
        for marker in [" AM.", " PM.", " am.", " pm."] {
            if raw.contains(marker) {
                let suffix = marker.strip_prefix(' ').unwrap_or(marker);
                candidates.push(PathBuf::from(raw.replace(
                    marker,
                    &format!("{NARROW_NO_BREAK_SPACE}{suffix}"),
                )));
            }
        }
        candidates.push(PathBuf::from(raw.nfd().collect::<String>()));
        candidates.push(PathBuf::from(raw.replace('\'', "\u{2019}")));
        candidates.push(PathBuf::from(
            raw.nfd().collect::<String>().replace('\'', "\u{2019}"),
        ));
        for candidate in candidates {
            if candidate != resolved.0
                && tokio::fs::try_exists(&candidate).await.unwrap_or(false)
            {
                return Ok(Self(candidate));
            }
        }
        Ok(resolved)
    }

    /// Borrows the resolved path for filesystem APIs.
    pub(super) fn as_path(&self) -> &Path {
        &self.0
    }
}
