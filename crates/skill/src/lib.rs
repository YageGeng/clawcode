//! Pi-compatible Session-scoped Skill discovery and explicit invocation.

mod catalog;
mod discovery;
mod error;
mod factory;
mod metadata;
mod source;

pub use catalog::{SkillCatalog, SkillCommandExpansion};
pub use error::SkillError;
pub use factory::{FilesystemSkillFactory, SkillFactory};
