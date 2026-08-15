use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::PromptError;

/// One immutable Pi-compatible Markdown prompt template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    /// Slash-command name derived from the Markdown filename.
    pub name: String,
    /// Optional description from frontmatter or the first body line.
    pub description: String,
    /// Template body containing Pi argument placeholders.
    pub content: String,
}

impl PromptTemplate {
    /// Loads one Markdown file and separates its optional frontmatter from the body.
    fn load(path: &Path) -> Result<Self, PromptError> {
        let raw = fs::read_to_string(path).map_err(|source| {
            PromptError::PromptTemplate {
                path: path.to_path_buf(),
                source,
            }
        })?;
        let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
        let (frontmatter, content) = if let Some(rest) =
            normalized.strip_prefix("---\n")
        {
            match rest.split_once("\n---") {
                Some((frontmatter, body)) => (
                    frontmatter,
                    body.strip_prefix('\n').unwrap_or(body).trim().to_string(),
                ),
                None => ("", normalized),
            }
        } else {
            ("", normalized)
        };
        let description = frontmatter
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(key, value)| {
                    (key.trim() == "description").then(|| {
                        value.trim().trim_matches(['\'', '"']).to_string()
                    })
                })
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                let first =
                    content.lines().find(|line| !line.trim().is_empty());
                first.map_or_else(String::new, |line| {
                    let mut description =
                        line.chars().take(60).collect::<String>();
                    if line.chars().count() > 60 {
                        description.push_str("...");
                    }
                    description
                })
            });
        let name = path
            .file_stem()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default()
            .to_string();
        Ok(Self {
            name,
            description,
            content,
        })
    }

    /// Substitutes all Pi positional and aggregate argument placeholders.
    fn expand(&self, arguments: &[String]) -> String {
        let mut expanded = String::with_capacity(self.content.len());
        let mut remaining = self.content.as_str();
        while !remaining.is_empty() {
            if !remaining.starts_with('$') {
                let Some(character) = remaining.chars().next() else {
                    break;
                };
                expanded.push(character);
                remaining =
                    remaining.strip_prefix(character).unwrap_or_default();
                continue;
            }
            if let Some(next) = remaining.strip_prefix("$ARGUMENTS") {
                expanded.push_str(&arguments.join(" "));
                remaining = next;
                continue;
            }
            if let Some(next) = remaining.strip_prefix("$@") {
                expanded.push_str(&arguments.join(" "));
                remaining = next;
                continue;
            }
            if let Some(range_and_tail) = remaining.strip_prefix("${@:")
                && let Some((range, tail)) = range_and_tail.split_once('}')
            {
                let mut parts = range.split(':');
                if let Some(start) =
                    parts.next().and_then(|value| value.parse::<usize>().ok())
                {
                    let start = start.saturating_sub(1);
                    let selected = match parts
                        .next()
                        .and_then(|value| value.parse::<usize>().ok())
                    {
                        Some(length) => {
                            arguments.iter().skip(start).take(length)
                        }
                        None => arguments.iter().skip(start).take(usize::MAX),
                    };
                    expanded.push_str(
                        &selected.cloned().collect::<Vec<_>>().join(" "),
                    );
                    remaining = tail;
                    continue;
                }
            }
            let after_dollar = remaining.strip_prefix('$').unwrap_or_default();
            let digits = after_dollar
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            if !digits.is_empty() {
                let position = digits.parse::<usize>().unwrap_or_default();
                if position > 0 {
                    expanded.push_str(
                        arguments
                            .get(position - 1)
                            .map(String::as_str)
                            .unwrap_or_default(),
                    );
                    remaining =
                        after_dollar.strip_prefix(&digits).unwrap_or_default();
                    continue;
                }
            }
            expanded.push('$');
            remaining = after_dollar;
        }
        expanded
    }
}

/// Deterministic session-local catalog of Pi-compatible prompt templates.
#[derive(Debug, Clone, Default)]
pub struct PromptTemplateCatalog {
    templates: BTreeMap<String, PromptTemplate>,
}

impl PromptTemplateCatalog {
    /// Discovers direct Markdown files from ordered files and directories.
    pub fn discover(paths: &[PathBuf]) -> Result<Self, PromptError> {
        let mut templates = BTreeMap::new();
        for path in paths {
            if !path.exists() {
                continue;
            }
            if path.is_file() {
                if path.extension().is_some_and(|extension| extension == "md") {
                    let template = PromptTemplate::load(path)?;
                    templates.insert(template.name.clone(), template);
                }
                continue;
            }
            let mut entries = fs::read_dir(path)
                .map_err(|source| PromptError::PromptTemplate {
                    path: path.clone(),
                    source,
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| PromptError::PromptTemplate {
                    path: path.clone(),
                    source,
                })?;
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries {
                let file = entry.path();
                if file.is_file()
                    && file
                        .extension()
                        .is_some_and(|extension| extension == "md")
                {
                    let template = PromptTemplate::load(&file)?;
                    templates.insert(template.name.clone(), template);
                }
            }
        }
        Ok(Self { templates })
    }

    /// Expands a matching slash invocation or leaves non-template input untouched.
    #[must_use]
    pub fn expand(&self, input: &str) -> Option<String> {
        let command = input.strip_prefix('/')?;
        let mut invocation = command.splitn(2, char::is_whitespace);
        let template = self.templates.get(invocation.next()?)?;
        let mut arguments = Vec::new();
        let mut current = String::new();
        let mut quote = None;
        for character in invocation.next().unwrap_or_default().chars() {
            match (quote, character) {
                (Some(expected), current_character)
                    if expected == current_character =>
                {
                    quote = None;
                }
                (Some(_), current_character) => current.push(current_character),
                (None, '\'' | '"') => quote = Some(character),
                (None, ' ' | '\t') if !current.is_empty() => {
                    arguments.push(std::mem::take(&mut current));
                }
                (None, current_character) => current.push(current_character),
            }
        }
        if !current.is_empty() {
            arguments.push(current);
        }
        Some(template.expand(&arguments))
    }
}
