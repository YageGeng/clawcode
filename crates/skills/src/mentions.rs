use std::collections::HashSet;
use std::path::PathBuf;

use crate::model::SkillMetadata;

const SKILL_PATH_PREFIX: &str = "skill://";

/// Selects skills explicitly referenced by `$name` or linked `skill://` paths in user text.
pub fn collect_explicit_skill_mentions(text: &str, skills: &[SkillMetadata]) -> Vec<SkillMetadata> {
    let mentions = parse_mentions(text);
    let mut selected = Vec::new();
    let mut seen_paths = HashSet::new();

    for skill in skills {
        if !mentions.names.contains(skill.name.as_str()) && !mentions.paths.contains(&skill.path) {
            continue;
        }
        if seen_paths.insert(skill.path.clone()) {
            selected.push(skill.clone());
        }
    }

    selected
}

#[derive(Debug, Default)]
struct ParsedMentions<'a> {
    names: HashSet<&'a str>,
    paths: HashSet<PathBuf>,
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
            mentions.names.insert(name);
            mentions.paths.insert(path);
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

        mentions.names.insert(&text[name_start..name_end]);
        index = name_end;
    }

    mentions
}

/// Parses markdown-style linked mentions such as `[$name](skill:///abs/path/SKILL.md)`.
fn parse_linked_skill_mention<'a>(
    text: &'a str,
    bytes: &[u8],
    start: usize,
) -> Option<(&'a str, PathBuf, usize)> {
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
    if bytes.get(name_end) != Some(&b']') || bytes.get(name_end + 1) != Some(&b'(') {
        return None;
    }

    let path_start = name_end + 2;
    let mut path_end = path_start;
    while let Some(byte) = bytes.get(path_end)
        && *byte != b')'
    {
        path_end += 1;
    }
    if bytes.get(path_end) != Some(&b')') {
        return None;
    }

    let raw_path = text[path_start..path_end].trim();
    let path = raw_path.strip_prefix(SKILL_PATH_PREFIX)?;
    Some((
        &text[name_start..name_end],
        PathBuf::from(path),
        path_end + 1,
    ))
}

/// Returns whether one byte can be part of a skill mention name.
fn is_name_char(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b':')
}
