//! Pi-compatible Session-scoped Skill discovery and explicit invocation.

mod catalog;
mod discovery;
mod error;
mod factory;

pub use catalog::SkillCatalog;
pub use error::SkillError;
pub use factory::{FilesystemSkillFactory, SkillFactory};
