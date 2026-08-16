use serde::de::DeserializeOwned;

use crate::PromptError;

/// Parsed Markdown content with optional YAML frontmatter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownDocument {
    frontmatter: Option<String>,
    body: String,
}

impl MarkdownDocument {
    /// Parses pi-compatible frontmatter after normalizing platform newlines.
    #[must_use]
    pub fn parse(content: &str) -> Self {
        let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
        let Some(after_delimiter) = normalized.strip_prefix("---\n") else {
            return Self {
                frontmatter: None,
                body: normalized,
            };
        };
        let Some(closing_index) = after_delimiter.find("\n---") else {
            return Self {
                frontmatter: None,
                body: normalized,
            };
        };
        let Some(frontmatter) = after_delimiter.get(..closing_index) else {
            return Self {
                frontmatter: None,
                body: normalized,
            };
        };
        let Some(after_frontmatter) = after_delimiter.get(closing_index..)
        else {
            return Self {
                frontmatter: None,
                body: normalized,
            };
        };
        let body = after_frontmatter
            .strip_prefix("\n---")
            .unwrap_or(after_frontmatter)
            .trim()
            .to_string();

        Self {
            frontmatter: Some(frontmatter.to_string()),
            body,
        }
    }

    /// Deserializes YAML frontmatter into caller-selected metadata.
    pub fn metadata<T: DeserializeOwned>(&self) -> Result<T, PromptError> {
        serde_yaml_ng::from_str(self.frontmatter.as_deref().unwrap_or("{}"))
            .map_err(|error| PromptError::Frontmatter(error.to_string()))
    }

    /// Returns the normalized Markdown body without valid frontmatter.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}
