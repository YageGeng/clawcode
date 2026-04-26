use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use crate::model::{SkillInput, SkillMentionOptions, SkillMetadata};

const SKILL_PATH_PREFIX: &str = "skill://";
const SKILL_FILE_NAME: &str = "SKILL.md";

/// Selects explicitly referenced skills from structured inputs and text mentions.
pub fn collect_explicit_skill_mentions(
    inputs: &[SkillInput],
    skills: &[SkillMetadata],
    options: &SkillMentionOptions,
) -> Vec<SkillMetadata> {
    let disabled_paths = normalize_disabled_paths(&options.disabled_paths);
    let skill_name_counts = build_skill_name_counts(skills, &disabled_paths);
    let mut selection = SelectionState::default();

    select_structured_skill_inputs(inputs, skills, &disabled_paths, &mut selection);

    for input in inputs {
        let SkillInput::Text { text } = input else {
            continue;
        };
        let mentions = parse_mentions(text);
        select_skills_from_mentions(
            skills,
            options,
            &disabled_paths,
            &skill_name_counts,
            &mentions,
            &mut selection,
        );
    }

    selection.selected
}

#[derive(Debug, Default)]
struct SelectionState {
    selected: Vec<SkillMetadata>,
    seen_names: HashSet<String>,
    /// Normalized skill paths already selected during this mention pass.
    seen_paths: HashSet<PathBuf>,
    blocked_plain_names: HashSet<String>,
}

/// Selects structured skill inputs by exact path before text mentions are considered.
fn select_structured_skill_inputs(
    inputs: &[SkillInput],
    skills: &[SkillMetadata],
    disabled_paths: &HashSet<PathBuf>,
    selection: &mut SelectionState,
) {
    for input in inputs {
        let SkillInput::Skill { name, path } = input else {
            continue;
        };
        selection.blocked_plain_names.insert(name.clone());
        let normalized_path = normalize_path_for_match(path);
        if disabled_paths.contains(&normalized_path)
            || selection.seen_paths.contains(&normalized_path)
        {
            continue;
        }
        if let Some(skill) = skills
            .iter()
            .find(|skill| normalize_path_for_match(&skill.path) == normalized_path)
        {
            selection.seen_paths.insert(normalized_path);
            selection.seen_names.insert(skill.name.clone());
            selection.selected.push(skill.clone());
        }
    }
}

#[derive(Debug, Default)]
struct ParsedMentions<'a> {
    paths: HashSet<PathBuf>,
    plain_names: HashSet<&'a str>,
}

/// Parses mention tokens without allocating for plain names.
fn parse_mentions(text: &str) -> ParsedMentions<'_> {
    let bytes = text.as_bytes();
    let mut mentions = ParsedMentions::default();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'['
            && let Some((name, path, next_index)) = parse_linked_skill_mention(text, bytes, index)
        {
            if !is_common_env_var(name) && is_skill_path(&path) {
                mentions.paths.insert(normalize_skill_path(&path));
            }
            index = next_index;
            continue;
        }

        if bytes[index] != b'$' {
            index += 1;
            continue;
        }

        let name_start = index + 1;
        let Some(first) = bytes.get(name_start) else {
            break;
        };
        if !is_name_char(*first) {
            index += 1;
            continue;
        }

        let mut name_end = name_start + 1;
        while let Some(next) = bytes.get(name_end)
            && is_name_char(*next)
        {
            name_end += 1;
        }

        let name = &text[name_start..name_end];
        if !is_common_env_var(name) {
            mentions.plain_names.insert(name);
        }
        index = name_end;
    }

    mentions
}

/// Selects path mentions before plain names while preserving loaded skill order.
fn select_skills_from_mentions(
    skills: &[SkillMetadata],
    options: &SkillMentionOptions,
    disabled_paths: &HashSet<PathBuf>,
    skill_name_counts: &HashMap<String, usize>,
    mentions: &ParsedMentions<'_>,
    selection: &mut SelectionState,
) {
    if mentions.paths.is_empty() && mentions.plain_names.is_empty() {
        return;
    }

    for skill in skills {
        let normalized_path = normalize_path_for_match(&skill.path);
        if disabled_paths.contains(&normalized_path)
            || selection.seen_paths.contains(&normalized_path)
        {
            continue;
        }
        if mentions.paths.contains(&normalized_path) {
            selection.seen_paths.insert(normalized_path);
            selection.seen_names.insert(skill.name.clone());
            selection.selected.push(skill.clone());
        }
    }

    for skill in skills {
        let normalized_path = normalize_path_for_match(&skill.path);
        if disabled_paths.contains(&normalized_path)
            || selection.seen_paths.contains(&normalized_path)
        {
            continue;
        }
        if selection.blocked_plain_names.contains(skill.name.as_str())
            || !mentions.plain_names.contains(skill.name.as_str())
        {
            continue;
        }

        let skill_count = skill_name_counts.get(&skill.name).copied().unwrap_or(0);
        let connector_count = options
            .connector_slug_counts
            .get(&skill.name.to_ascii_lowercase())
            .copied()
            .unwrap_or(0);
        if skill_count != 1 || connector_count != 0 {
            continue;
        }

        if selection.seen_names.insert(skill.name.clone()) {
            selection.seen_paths.insert(normalized_path);
            selection.selected.push(skill.clone());
        }
    }
}

/// Normalizes disabled skill paths once so every selection branch compares the same path shape.
fn normalize_disabled_paths(disabled_paths: &HashSet<PathBuf>) -> HashSet<PathBuf> {
    disabled_paths
        .iter()
        .map(|path| normalize_path_for_match(path))
        .collect()
}

/// Counts enabled skills by name so plain names can be rejected when ambiguous.
fn build_skill_name_counts(
    skills: &[SkillMetadata],
    disabled_paths: &HashSet<PathBuf>,
) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for skill in skills {
        if disabled_paths.contains(&normalize_path_for_match(&skill.path)) {
            continue;
        }
        *counts.entry(skill.name.clone()).or_insert(0) += 1;
    }
    counts
}

/// Parses markdown-style linked mentions such as `[$name](skill:///abs/path/SKILL.md)`.
fn parse_linked_skill_mention<'a>(
    text: &'a str,
    bytes: &[u8],
    start: usize,
) -> Option<(&'a str, String, usize)> {
    let sigil = start + 1;
    if bytes.get(sigil) != Some(&b'$') {
        return None;
    }

    let name_start = sigil + 1;
    if !bytes.get(name_start).copied().is_some_and(is_name_char) {
        return None;
    }

    let mut name_end = name_start + 1;
    while bytes.get(name_end).copied().is_some_and(is_name_char) {
        name_end += 1;
    }
    if bytes.get(name_end) != Some(&b']') {
        return None;
    }

    let mut path_start = name_end + 1;
    while bytes
        .get(path_start)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        path_start += 1;
    }
    if bytes.get(path_start) != Some(&b'(') {
        return None;
    }

    let mut path_end = path_start + 1;
    while let Some(byte) = bytes.get(path_end)
        && *byte != b')'
    {
        path_end += 1;
    }
    if bytes.get(path_end) != Some(&b')') {
        return None;
    }

    let path = text[path_start + 1..path_end].trim();
    if path.is_empty() {
        return None;
    }

    Some((&text[name_start..name_end], path.to_string(), path_end + 1))
}

/// Returns true when a linked mention points at a skill resource.
fn is_skill_path(path: &str) -> bool {
    path.starts_with(SKILL_PATH_PREFIX)
        || Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(SKILL_FILE_NAME))
}

/// Normalizes supported linked skill paths into the stored filesystem path shape.
fn normalize_skill_path(path: &str) -> PathBuf {
    normalize_path_for_match(Path::new(
        path.strip_prefix(SKILL_PATH_PREFIX).unwrap_or(path),
    ))
}

/// Converts a path into the canonical-or-absolute lexical shape used only for equality checks.
fn normalize_path_for_match(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }

    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    normalize_lexical_path(&absolute_path)
}

/// Removes `.` and resolvable `..` path components without requiring the path to exist on disk.
fn normalize_lexical_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() && !path.is_absolute() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}

/// Returns whether a parsed mention name is a common shell environment variable.
fn is_common_env_var(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "PATH"
            | "HOME"
            | "USER"
            | "SHELL"
            | "PWD"
            | "TMPDIR"
            | "TEMP"
            | "TMP"
            | "LANG"
            | "TERM"
            | "XDG_CONFIG_HOME"
    )
}

/// Returns whether one byte can be part of a skill mention name.
fn is_name_char(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b':')
}
