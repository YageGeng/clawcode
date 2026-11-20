//! Application assets: the default component icons plus the Lucide icons the shell adds.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        MessagesSquare,
        GitBranch,
        ListFilter,
        SquarePen,
        CircleQuestionMark,
        X,
        Brain,
        Pencil,
        Trash,
        ArrowRightLeft,
        Wrench,
        Settings2
    ]
);

/// Serves the shell's extra icons first, then gpui-kit's default asset bundle.
pub struct AppAssets;

impl AssetSource for AppAssets {
    /// Loads one asset path from the extra icons or the default bundle.
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    /// Lists both sources under `path`, sorted and without duplicates.
    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
