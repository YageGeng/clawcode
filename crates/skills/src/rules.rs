//! Per-skill enable/disable rule resolution.
//!
//! Consumes [`SkillsConfig`] rules and resolves them against discovered
//! [`SkillMetadata`] into a set of disabled skill paths.

use std::collections::HashSet;
use std::path::PathBuf;

use config::skills::{SkillConfigRule, SkillsConfig};

use crate::SkillMetadata;

/// Engine that resolves per-skill enable/disable rules into a set of
/// disabled skill paths.
///
/// Rules are applied in list order; later rules override earlier ones when
/// their selectors match the same skill.  Invalid rules are warned and skipped.
pub(crate) struct ConfigRules {
    rules: Vec<SkillConfigRule>,
}

impl ConfigRules {
    /// Build the rule engine from a [`SkillsConfig`].
    pub fn new(config: &SkillsConfig) -> Self {
        Self {
            rules: config.rules.clone(),
        }
    }

    /// Resolve the set of disabled skill paths against the given skills.
    pub fn resolve(&self, skills: &[SkillMetadata]) -> HashSet<PathBuf> {
        let mut disabled: HashSet<PathBuf> = HashSet::new();

        for rule in &self.rules {
            match Self::apply_selector(rule, skills) {
                Ok(paths) => {
                    for path in paths {
                        if rule.enabled {
                            disabled.remove(&path);
                        } else {
                            disabled.insert(path);
                        }
                    }
                }
                Err(msg) => {
                    tracing::warn!("ignoring skills.config entry: {msg}");
                }
            }
        }

        disabled
    }

    /// Map a config rule to the set of skill paths it selects.
    fn apply_selector(
        rule: &SkillConfigRule,
        skills: &[SkillMetadata],
    ) -> Result<Vec<PathBuf>, String> {
        match (rule.path.as_ref(), rule.name.as_deref()) {
            (Some(path), None) => {
                let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
                Ok(vec![canonical])
            }
            (None, Some(name)) if !name.trim().is_empty() => Ok(skills
                .iter()
                .filter(|s| s.name.eq_ignore_ascii_case(name.trim()))
                .map(|s| s.path.clone())
                .collect()),
            (Some(_), Some(_)) => {
                Err("entry has both path and name selectors (must have exactly one)".to_string())
            }
            (None, None) => Err("entry missing path or name selector".to_string()),
            (None, Some(_)) => Err("entry has empty name selector".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SkillScope;

    fn make_skill(name: &str, path: &str) -> SkillMetadata {
        SkillMetadata {
            name: name.to_string(),
            description: String::new(),
            path: PathBuf::from(path),
            scope: SkillScope::Repo,
        }
    }

    #[test]
    fn empty_rules_all_enabled() {
        let skills = vec![make_skill("demo", "/tmp/demo/SKILL.md")];
        let config = SkillsConfig::default();
        let engine = ConfigRules::new(&config);
        assert!(engine.resolve(&skills).is_empty());
    }

    #[test]
    fn disable_by_name() {
        let skills = vec![
            make_skill("demo", "/tmp/demo/SKILL.md"),
            make_skill("other", "/tmp/other/SKILL.md"),
        ];
        let config = SkillsConfig {
            rules: vec![SkillConfigRule {
                name: Some("demo".to_string()),
                path: None,
                enabled: false,
            }],
            ..Default::default()
        };
        let engine = ConfigRules::new(&config);
        let disabled = engine.resolve(&skills);
        assert_eq!(disabled.len(), 1);
        assert!(disabled.contains(&PathBuf::from("/tmp/demo/SKILL.md")));
    }

    #[test]
    fn disable_by_path() {
        let skills = vec![make_skill("demo", "/tmp/demo/SKILL.md")];
        let config = SkillsConfig {
            rules: vec![SkillConfigRule {
                path: Some(PathBuf::from("/tmp/demo/SKILL.md")),
                name: None,
                enabled: false,
            }],
            ..Default::default()
        };
        let engine = ConfigRules::new(&config);
        let disabled = engine.resolve(&skills);
        assert!(disabled.contains(&PathBuf::from("/tmp/demo/SKILL.md")));
    }

    #[test]
    fn later_rule_overrides_earlier() {
        let skills = vec![make_skill("demo", "/tmp/demo/SKILL.md")];
        let config = SkillsConfig {
            rules: vec![
                SkillConfigRule {
                    name: Some("demo".to_string()),
                    path: None,
                    enabled: false,
                },
                SkillConfigRule {
                    name: Some("demo".to_string()),
                    path: None,
                    enabled: true,
                },
            ],
            ..Default::default()
        };
        let engine = ConfigRules::new(&config);
        let disabled = engine.resolve(&skills);
        assert!(disabled.is_empty());
    }
}
