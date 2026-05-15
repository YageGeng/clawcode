//! Mention detection: scans user input for `$skill-name` tokens and
//! resolves them to enabled [`SkillMetadata`].

use crate::{SkillMetadata, SkillRegistry};

/// Common environment variable names excluded from `$name` mention matching
/// to avoid false positives.
const EXCLUDED_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "USER", "SHELL", "PWD", "LANG", "TERM", "EDITOR",
];

/// Extracts and resolves `$skill-name` mentions from user input.
pub(crate) struct MentionMatcher;

impl MentionMatcher {
    /// Scan `text` for `$skill-name` tokens, then resolve them to enabled
    /// [`SkillMetadata`] in the registry.
    ///
    /// A mention matches when exactly one enabled skill has the given name
    /// (case-insensitive).  Common env-var names and tokens longer than 64
    /// characters are excluded.  Duplicate and unmatched mentions are
    /// silently skipped.
    pub fn resolve<'a>(registry: &'a SkillRegistry, text: &str) -> Vec<&'a SkillMetadata> {
        let mentions = Self::extract(text);
        if mentions.is_empty() {
            return Vec::new();
        }

        let enabled = registry.enabled_skills();
        let mut result = Vec::new();

        for mention in &mentions {
            let mut matching = enabled
                .iter()
                .filter(|s| s.name.eq_ignore_ascii_case(mention))
                .take(2);
            if let (Some(first), None) = (matching.next(), matching.next()) {
                result.push(*first);
            }
        }

        result
    }

    /// Extract `$identifier` tokens from text.
    ///
    /// Identifiers may contain ASCII letters, digits, hyphens, and underscores.
    /// Known environment variable names are excluded.  Results are deduplicated
    /// in first-occurrence order.
    #[allow(clippy::string_slice)]
    pub fn extract(text: &str) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();

        let mut chars = text.char_indices().peekable();
        while let Some((start, ch)) = chars.next() {
            if ch != '$' {
                continue;
            }
            let mut end = start + 1;
            while let Some((i, c)) = chars.peek() {
                if c.is_ascii_alphanumeric() || *c == '-' || *c == '_' {
                    end = *i + c.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            // SAFETY: start is the byte index of '$' (ASCII, 1 byte), so start+1 points
            // to the next char. The following chars are all ASCII (is_ascii_alphanumeric
            // or '-'/'_'), and end is accumulated via len_utf8() — always a valid boundary.
            let captured = &text[start + 1..end];
            if captured.is_empty() {
                continue;
            }
            if EXCLUDED_ENV_VARS
                .iter()
                .any(|v| v.eq_ignore_ascii_case(captured))
            {
                continue;
            }
            if captured.len() > 64 {
                continue;
            }
            if seen.insert(captured.to_lowercase()) {
                result.push(captured.to_string());
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_simple_mention() {
        let mentions = MentionMatcher::extract("use $demo to do something");
        assert_eq!(mentions, vec!["demo"]);
    }

    #[test]
    fn extract_multiple_mentions() {
        let mentions = MentionMatcher::extract("try $foo and $bar together");
        assert_eq!(mentions, vec!["foo", "bar"]);
    }

    #[test]
    fn deduplicates_mentions() {
        let mentions = MentionMatcher::extract("$demo and $demo again");
        assert_eq!(mentions, vec!["demo"]);
    }

    #[test]
    fn excludes_env_vars() {
        let mentions = MentionMatcher::extract("echo $PATH and $HOME");
        assert!(mentions.is_empty());
    }

    #[test]
    fn allows_hyphens_and_underscores() {
        let mentions = MentionMatcher::extract("use $skill-creator and $my_skill");
        assert_eq!(mentions, vec!["skill-creator", "my_skill"]);
    }

    #[test]
    fn rejects_overly_long_mentions() {
        let long = "a".repeat(65);
        let text = format!("${long}");
        let mentions = MentionMatcher::extract(&text);
        assert!(mentions.is_empty());
    }

    #[test]
    fn empty_text_returns_empty() {
        assert!(MentionMatcher::extract("").is_empty());
    }
}
