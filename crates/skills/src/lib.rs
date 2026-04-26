//! Skill discovery and injection support for the agent runtime.

mod error;
mod injection;
mod loader;
mod mentions;
mod model;
mod render;

pub use error::{Error, Result};
pub use injection::build_skill_injections;
pub use loader::SkillsManager;
pub use mentions::collect_explicit_skill_mentions;
pub use model::{
    SkillConfig, SkillInput, SkillLoadError, SkillLoadOutcome, SkillMentionOptions, SkillMetadata,
};
pub use render::render_skills_section;
