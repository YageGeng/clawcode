//! Bundled assets: every Lucide icon the shell draws is served.

use desktop::assets::AppAssets;
use gpui_kit::AssetSource as _;
use gpui_kit::assets::IconName;

#[test]
fn panel_and_details_icons_are_bundled() {
    for icon in [
        IconName::FolderTree,
        IconName::GitCompare,
        IconName::PanelRight,
        IconName::PanelRightClose,
        IconName::Maximize2,
        IconName::Minimize2,
        IconName::Info,
        IconName::Gauge,
        IconName::ChartColumn,
        IconName::SquareArrowOutUpRight,
        IconName::CornerLeftUp,
        IconName::FileDiff,
        IconName::Bot,
        IconName::ScrollText,
        IconName::User,
        IconName::Send,
        IconName::Puzzle,
    ] {
        let path = icon.path();
        assert!(
            AppAssets.load(&path).ok().flatten().is_some(),
            "{path} is not bundled"
        );
    }
}
