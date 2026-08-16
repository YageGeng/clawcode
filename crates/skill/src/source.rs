use std::fs;
use std::path::{Component, Path, PathBuf};

use protocol::{
    SkillDiscoveryMode, SkillSource, SkillSourceKind, SkillSourceScope,
};

use crate::SkillError;

/// Resolves user-relative and Session-relative Skill paths consistently.
pub(crate) struct SkillPathResolver<'a> {
    cwd: &'a Path,
    user_home: &'a Path,
}

impl<'a> SkillPathResolver<'a> {
    /// Creates a resolver anchored to one canonical Session cwd and user home.
    pub(crate) fn new(cwd: &'a Path, user_home: &'a Path) -> Self {
        Self { cwd, user_home }
    }

    /// Expands a leading `~` and resolves relative paths from the Session cwd.
    pub(crate) fn resolve(&self, path: &Path) -> PathBuf {
        let expanded = if path.components().next()
            == Some(Component::Normal("~".as_ref()))
        {
            path.strip_prefix("~").map_or_else(
                |_| path.to_path_buf(),
                |tail| self.user_home.join(tail),
            )
        } else {
            path.to_path_buf()
        };
        if expanded.is_absolute() {
            expanded
        } else {
            self.cwd.join(expanded)
        }
    }
}

/// One source plus whether a missing path must produce a diagnostic.
pub(crate) struct SkillRoot {
    pub(crate) info: SkillSource,
    pub(crate) required: bool,
}

/// Immutable inputs used to build an ordered Session Skill source list.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct SkillSourcePlan {
    cwd: PathBuf,
    global_root: PathBuf,
    user_home: PathBuf,
    configured_paths: Vec<PathBuf>,
    project_resources_allowed: bool,
    extension_paths: Vec<PathBuf>,
}

impl SkillSourcePlan {
    /// Resolves the Session cwd and returns sources in winner-first order.
    pub(crate) fn into_sources(self) -> Result<Vec<SkillRoot>, SkillError> {
        let cwd = fs::canonicalize(&self.cwd)?;
        let resolver = SkillPathResolver::new(&cwd, &self.user_home);
        let mut roots = self
            .configured_paths
            .iter()
            .map(|path| SkillRoot {
                info: SkillSource::builder()
                    .kind(SkillSourceKind::Configured)
                    .scope(SkillSourceScope::Configured)
                    .root(resolver.resolve(path))
                    .origin_base_dir(cwd.clone())
                    .discovery_mode(SkillDiscoveryMode::Pi)
                    .build(),
                required: true,
            })
            .collect::<Vec<_>>();

        if self.project_resources_allowed {
            roots.push(SkillRoot {
                info: SkillSource::builder()
                    .kind(SkillSourceKind::Pi)
                    .scope(SkillSourceScope::Project)
                    .root(cwd.join(".pi/skills"))
                    .origin_base_dir(cwd.join(".pi"))
                    .discovery_mode(SkillDiscoveryMode::Pi)
                    .build(),
                required: false,
            });
            for agents_root in Self::ancestor_agents_roots(&cwd) {
                if Self::same_path(
                    &agents_root,
                    &self.user_home.join(".agents/skills"),
                ) {
                    continue;
                }
                let origin_base_dir = agents_root
                    .parent()
                    .unwrap_or(agents_root.as_path())
                    .to_path_buf();
                roots.push(SkillRoot {
                    info: SkillSource::builder()
                        .kind(SkillSourceKind::Agents)
                        .scope(SkillSourceScope::Project)
                        .root(agents_root)
                        .origin_base_dir(origin_base_dir)
                        .discovery_mode(SkillDiscoveryMode::Agents)
                        .build(),
                    required: false,
                });
            }
        }

        roots.push(SkillRoot {
            info: SkillSource::builder()
                .kind(SkillSourceKind::Pi)
                .scope(SkillSourceScope::User)
                .root(self.global_root.join("skills"))
                .origin_base_dir(self.global_root.clone())
                .discovery_mode(SkillDiscoveryMode::Pi)
                .build(),
            required: false,
        });
        roots.push(SkillRoot {
            info: SkillSource::builder()
                .kind(SkillSourceKind::Agents)
                .scope(SkillSourceScope::User)
                .root(self.user_home.join(".agents/skills"))
                .origin_base_dir(self.user_home.join(".agents"))
                .discovery_mode(SkillDiscoveryMode::Agents)
                .build(),
            required: false,
        });
        roots.extend(self.extension_paths.iter().map(|path| {
            SkillRoot {
                info: SkillSource::builder()
                    .kind(SkillSourceKind::Extension)
                    .scope(SkillSourceScope::Extension)
                    .root(resolver.resolve(path))
                    .origin_base_dir(cwd.clone())
                    .discovery_mode(SkillDiscoveryMode::Pi)
                    .build(),
                required: true,
            }
        }));

        Ok(roots)
    }

    /// Returns cwd-to-root `.agents/skills` paths bounded by the Git root.
    fn ancestor_agents_roots(cwd: &Path) -> Vec<PathBuf> {
        let git_root = cwd
            .ancestors()
            .find(|directory| directory.join(".git").exists());
        let mut roots = Vec::new();
        for directory in cwd.ancestors() {
            roots.push(directory.join(".agents/skills"));
            if git_root.is_some_and(|root| root == directory) {
                break;
            }
        }
        roots
    }

    /// Compares existing paths canonically and absent paths lexically.
    fn same_path(left: &Path, right: &Path) -> bool {
        match (fs::canonicalize(left), fs::canonicalize(right)) {
            (Ok(left), Ok(right)) => left == right,
            _ => left == right,
        }
    }
}
