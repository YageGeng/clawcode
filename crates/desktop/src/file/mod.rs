//! Workspace files on the desktop side: the file tree's model and the icon
//! theme that picks an icon for a file or folder name. Neither depends on gpui.

pub mod changes;
pub mod icons;
pub mod tree;
pub use changes::{ChangeNode, PATCH_BATCH, PatchLoader};
