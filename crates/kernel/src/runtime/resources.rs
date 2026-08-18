use std::sync::Arc;

/// Immutable Prompt and optional Skill resources published together at startup.
pub(super) struct SessionResources {
    prompt: Arc<::prompt::PromptSession>,
    skills: Option<Arc<skill::SkillCatalog>>,
}

impl SessionResources {
    /// Creates one complete resource snapshot after all discovery succeeds.
    pub(super) fn new(
        prompt: Arc<::prompt::PromptSession>,
        skills: Option<Arc<skill::SkillCatalog>>,
    ) -> Self {
        Self { prompt, skills }
    }

    /// Returns the immutable Prompt resources for this Session.
    pub(super) fn prompt(&self) -> &Arc<::prompt::PromptSession> {
        &self.prompt
    }

    /// Returns the optional immutable Skill catalog for this Session.
    pub(super) fn skills(&self) -> Option<&Arc<skill::SkillCatalog>> {
        self.skills.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::SessionResources;

    /// Verifies that Prompt and optional Skill resources become visible together.
    #[test]
    fn resources_publish_one_complete_snapshot() {
        let prompt = Arc::new(
            ::prompt::PromptSession::builder()
                .cwd(std::path::PathBuf::from("/workspace"))
                .templates(::prompt::PromptTemplateCatalog::default())
                .build(),
        );

        let resources = SessionResources::new(Arc::clone(&prompt), None);

        assert!(Arc::ptr_eq(resources.prompt(), &prompt));
        assert!(resources.skills().is_none());
    }
}
