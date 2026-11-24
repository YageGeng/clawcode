//! The About page, and the path rows it shares with the Provider page.

use std::path::PathBuf;

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::label::Label;
use gpui_kit::component::setting::{
    SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex};
use gpui_kit::{ParentElement as _, SharedString, Styled as _};

use crate::ui::AppView;

/// A labelled file or folder that can be copied and shown in the file manager.
pub(super) struct PathRow {
    /// The setting's name.
    pub(super) label: SharedString,
    /// The path; `None` while the configuration is not loaded.
    pub(super) path: Option<PathBuf>,
}

impl From<PathRow> for SettingItem {
    /// The path under its label, with the actions on the right.
    fn from(row: PathRow) -> Self {
        let Some(path) = row.path else {
            let missing =
                rust_i18n::t!("Settings.config_not_loaded").into_owned();
            return SettingItem::new(
                row.label,
                SettingField::render(move |_, _, cx| {
                    Label::new(missing.clone())
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                }),
            );
        };
        let shown = SharedString::from(path.display().to_string());
        let copied = shown.clone();
        SettingItem::new(
            row.label,
            SettingField::render(move |_, _, _| {
                let path = path.clone();
                let reveal = rust_i18n::t!("Workspace.reveal").into_owned();
                h_flex()
                    .child(Clipboard::new("copy").value(copied.clone()))
                    .child(
                        Button::new("reveal")
                            .ghost()
                            .small()
                            .icon(LucideIcon::FolderOpen)
                            .tooltip(reveal.clone())
                            .accessibility_label(reveal)
                            .on_click(move |_, _, cx| cx.reveal_path(&path)),
                    )
            }),
        )
        .description(shown)
    }
}

impl AppView {
    /// Where the backend's configuration file is.
    pub(super) fn config_row(&self) -> PathRow {
        PathRow {
            label: rust_i18n::t!("Settings.config_file").into_owned().into(),
            path: self.host.config_path.clone(),
        }
    }

    /// The About page: the version and where things are kept.
    pub(super) fn about_page(&self) -> SettingPage {
        let version = SettingItem::new(
            rust_i18n::t!("Settings.version").into_owned(),
            SettingField::render(|_, _, cx| {
                Label::new(env!("CARGO_PKG_VERSION"))
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
            }),
        );
        let data = PathRow {
            label: rust_i18n::t!("Settings.data_dir").into_owned().into(),
            path: self.host.data_root.clone(),
        };
        SettingPage::new(rust_i18n::t!("Settings.about").into_owned()).group(
            SettingGroup::new()
                .item(version)
                .item(self.config_row().into())
                .item(data.into()),
        )
    }
}
