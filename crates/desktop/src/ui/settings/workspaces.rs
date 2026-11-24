//! The Workspaces page: the registered folders, their order, and adding one.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::setting::{
    SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex};
use gpui_kit::{
    AnyElement, Context, IntoElement as _, ParentElement as _, WeakEntity,
};
use typed_builder::TypedBuilder;

use crate::ui::AppView;

/// One registered workspace as the page lists it.
#[derive(TypedBuilder)]
struct WorkspaceItem {
    view: WeakEntity<AppView>,
    path: PathBuf,
    /// Checked-out branch, when it is a Git work tree.
    #[builder(default)]
    branch: Option<String>,
    /// Whether the folder is gone.
    missing: bool,
    /// Whether nothing is above or below it to swap with.
    first: bool,
    last: bool,
}

impl AppView {
    /// The Workspaces page; it changes the same list as the sidebar.
    pub(super) fn workspaces_page(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> SettingPage {
        let view = cx.entity().downgrade();
        let entries = self.state.workspaces.entries();
        let count = entries.len();
        let rows = entries.iter().enumerate().map(|(index, entry)| {
            WorkspaceItem::builder()
                .view(view.clone())
                .path(entry.path.clone())
                .branch(self.branch(&entry.path))
                .missing(self.missing.contains(&entry.path))
                .first(index == 0)
                .last(index + 1 == count)
                .build()
                .into()
        });
        let adder = view.clone();
        let add = SettingItem::new(
            rust_i18n::t!("Settings.new_workspace").into_owned(),
            SettingField::render(move |_, _, _| {
                let view = adder.clone();
                Button::new("settings-add-workspace")
                    .outline()
                    .small()
                    .icon(LucideIcon::Plus)
                    .label(rust_i18n::t!("Sidebar.add_workspace").into_owned())
                    .on_click(move |_, window, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.add_workspace(window, cx)
                        });
                    })
            }),
        )
        .description(rust_i18n::t!("Settings.new_workspace_hint").into_owned());
        SettingPage::new(rust_i18n::t!("Settings.workspaces").into_owned())
            .group(SettingGroup::new().items(rows).item(add))
    }
}

/// Something a row's button does to its workspace.
type RowAction = fn(&mut AppView, &Path, &mut Context<'_, AppView>);

impl From<WorkspaceItem> for SettingItem {
    /// The name over the branch and path, with the actions on the right.
    fn from(item: WorkspaceItem) -> Self {
        let name = crate::workspace::display_name(&item.path);
        let state = if item.missing {
            Some(rust_i18n::t!("Sidebar.missing_folder").into_owned())
        } else {
            item.branch.clone()
        };
        let path = item.path.display().to_string();
        let description = match state {
            Some(state) => format!("{state} · {path}"),
            None => path,
        };
        SettingItem::new(
            name,
            SettingField::render(move |_, _, _| item.actions()),
        )
        .description(description)
    }
}

impl WorkspaceItem {
    /// The row's buttons.
    fn actions(&self) -> AnyElement {
        let button = |id: &'static str,
                      icon,
                      label: &str,
                      disabled,
                      action: RowAction| {
            let view = self.view.clone();
            let path = self.path.clone();
            let label = rust_i18n::t!(label).into_owned();
            Button::new(id)
                .ghost()
                .small()
                .icon(icon)
                .tooltip(label.clone())
                .accessibility_label(label)
                .disabled(disabled)
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| action(this, &path, cx));
                })
        };
        h_flex()
            .child(button(
                "up",
                LucideIcon::ArrowUp,
                "Workspace.move_up",
                self.first,
                |this, path, cx| this.move_workspace(path, -1, cx),
            ))
            .child(button(
                "down",
                LucideIcon::ArrowDown,
                "Workspace.move_down",
                self.last,
                |this, path, cx| this.move_workspace(path, 1, cx),
            ))
            .child(button(
                "reveal",
                LucideIcon::FolderOpen,
                "Workspace.reveal",
                self.missing,
                |this, path, cx| this.reveal_workspace(path, cx),
            ))
            .child(button(
                "remove",
                LucideIcon::X,
                "Workspace.remove",
                false,
                |this, path, cx| this.remove_workspace(path, cx),
            ))
            .into_any_element()
    }
}
