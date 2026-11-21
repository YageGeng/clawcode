//! The Files tab (spec §8, M3b spec §6.2): a workspace's file tree, listed
//! directory by directory as folders open; clicking a file asks the shell to
//! preview it.

use std::path::PathBuf;

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::list::ListItem;
use gpui_kit::component::tree::{TreeEvent, TreeItem, TreeState, tree};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex};
use gpui_kit::{
    AppContext as _, ClickEvent, Context, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    div, prelude::FluentBuilder as _, rems,
};
use protocol::WorkspaceDirListing;
use typed_builder::TypedBuilder;

use crate::backend::{AcpClient, BackendError};
use crate::file::tree::{FileNode, FileNodeKind, FileTree, PlaceholderKind};
use crate::ui::file_icon::FileIconPainter;

impl From<&FileNode> for TreeItem {
    /// A tree row: folders carry their children, placeholders are disabled notes.
    fn from(node: &FileNode) -> Self {
        let label = match &node.kind {
            FileNodeKind::Placeholder(PlaceholderKind::Loading) => {
                rust_i18n::t!("Files.loading").into_owned()
            }
            FileNodeKind::Placeholder(PlaceholderKind::Empty) => {
                rust_i18n::t!("Files.empty_dir").into_owned()
            }
            FileNodeKind::Placeholder(PlaceholderKind::Truncated) => {
                rust_i18n::t!("Files.truncated").into_owned()
            }
            FileNodeKind::Directory | FileNodeKind::File => node.name.clone(),
        };
        TreeItem::new(node.path.clone(), label)
            .expanded(node.expanded)
            .disabled(matches!(node.kind, FileNodeKind::Placeholder(_)))
            .children(node.children.iter().map(TreeItem::from))
    }
}

/// What the Files tab asks the shell to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesEvent {
    /// Preview this root-relative file.
    Open(String),
}

/// One workspace's file tree tab.
#[derive(TypedBuilder)]
pub struct FilesView {
    /// The agent connection; `None` until the backend is ready.
    client: Option<AcpClient>,
    /// Listings and expansion of the workspace.
    tree: FileTree,
    /// The rendered tree's rows, selection, and scroll position.
    state: Entity<TreeState>,
    /// Whether names starting with `.` are listed.
    #[builder(default)]
    show_hidden: bool,
    /// Why the root could not be listed; the tree is replaced by this.
    #[builder(default)]
    error: Option<String>,
    /// Draws file and folder icons; the shell refreshes it when the theme changes.
    painter: FileIconPainter,
    /// Expansion events of the tree.
    _events: Subscription,
    /// The full refresh in flight; a newer one replaces it.
    #[builder(default)]
    _load: Option<Task<()>>,
}

impl EventEmitter<FilesEvent> for FilesView {}

impl FilesView {
    /// A tree of `workspace` that starts listing its root right away.
    pub fn new(
        workspace: PathBuf,
        client: Option<AcpClient>,
        painter: FileIconPainter,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let state = cx.new(|cx| TreeState::new(cx));
        let events = cx.subscribe(&state, Self::on_tree_event);
        let mut view = Self::builder()
            .client(client)
            .tree(FileTree::new(workspace))
            .state(state)
            .painter(painter)
            ._events(events)
            .build();
        view.reload(cx);
        view
    }

    /// Draws icons with `painter` from the next frame on.
    pub fn set_painter(&mut self, painter: FileIconPainter) {
        self.painter = painter;
    }

    /// Lists the root and every expanded directory again, all at once.
    pub fn reload(&mut self, cx: &mut Context<'_, Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let root = self.tree.root().to_path_buf();
        let dirs = self.tree.expanded_dirs();
        let hidden = self.show_hidden;
        self._load = Some(cx.spawn(async move |this, cx| {
            // Every directory is listed at once, so a refresh costs one round trip.
            let listings =
                futures::future::join_all(dirs.into_iter().map(|dir| {
                    let (client, root) = (&client, &root);
                    async move {
                        let listing = client.list_dir(root, &dir, hidden).await;
                        (dir, listing)
                    }
                }))
                .await;
            let _ =
                this.update(cx, |this, cx| this.apply_listings(listings, cx));
        }));
    }

    /// Stores listings; a root that fails replaces the tree with an error.
    fn apply_listings(
        &mut self,
        listings: Vec<(String, Result<WorkspaceDirListing, BackendError>)>,
        cx: &mut Context<'_, Self>,
    ) {
        for (dir, listing) in listings {
            match listing {
                Ok(listing) => {
                    if dir.is_empty() {
                        self.error = None;
                    }
                    self.tree.set_listing(&dir, listing);
                }
                Err(error) => {
                    tracing::warn!(
                        "failed to list {:?} in {}: {}",
                        dir,
                        self.tree.root().display(),
                        error
                    );
                    if dir.is_empty() {
                        self.error = Some(error.to_string());
                    } else {
                        // A folder that vanished shows as empty instead of loading forever.
                        self.tree
                            .set_listing(&dir, WorkspaceDirListing::default());
                    }
                }
            }
        }
        let items: Vec<TreeItem> =
            self.tree.nodes().iter().map(TreeItem::from).collect();
        self.state
            .update(cx, |state, cx| state.set_items(items, cx));
        cx.notify();
    }

    /// Tracks folders opening and closing; an unlisted folder is listed when it opens.
    fn on_tree_event(
        &mut self,
        _: Entity<TreeState>,
        event: &TreeEvent,
        cx: &mut Context<'_, Self>,
    ) {
        let (dir, expanded) = match event {
            TreeEvent::Expanded(dir) => (dir.to_string(), true),
            TreeEvent::Collapsed(dir) => (dir.to_string(), false),
        };
        self.tree.set_expanded(&dir, expanded);
        if !expanded || !self.tree.needs_listing(&dir) {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let root = self.tree.root().to_path_buf();
        let hidden = self.show_hidden;
        cx.spawn(async move |this, cx| {
            let listing = client.list_dir(&root, &dir, hidden).await;
            let _ = this.update(cx, |this, cx| {
                this.apply_listings(vec![(dir, listing)], cx)
            });
        })
        .detach();
    }

    /// Shows or hides names starting with `.`, listing everything again.
    fn toggle_hidden(&mut self, cx: &mut Context<'_, Self>) {
        self.show_hidden = !self.show_hidden;
        self.tree.clear();
        self.reload(cx);
        cx.notify();
    }

    /// The tab's toolbar: hidden files and refresh.
    fn render_toolbar(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let show_hidden = self.show_hidden;
        h_flex()
            .flex_none()
            .justify_end()
            .gap_1()
            .px_2()
            .py_1()
            .child(
                Button::new("files-hidden")
                    .ghost()
                    .small()
                    .icon(if show_hidden {
                        LucideIcon::EyeOff
                    } else {
                        LucideIcon::Eye
                    })
                    .tooltip(
                        if show_hidden {
                            rust_i18n::t!("Files.hide_hidden")
                        } else {
                            rust_i18n::t!("Files.show_hidden")
                        }
                        .into_owned(),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.toggle_hidden(cx)
                    })),
            )
            .child(
                Button::new("files-refresh")
                    .ghost()
                    .small()
                    .icon(LucideIcon::RefreshCw)
                    .tooltip(rust_i18n::t!("Files.refresh").into_owned())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.reload(cx)
                    })),
            )
    }
}

impl Render for FilesView {
    /// The toolbar above the tree, or the error that replaces the tree.
    fn render(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let toolbar = self.render_toolbar(cx).into_any_element();
        let body = match &self.error {
            Some(error) => div()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .child(
                    Label::new(
                        rust_i18n::t!("Sidebar.missing_folder").into_owned(),
                    )
                    .text_sm(),
                )
                .child(Label::new(error.clone()).text_xs().text_color(muted))
                .child(
                    Button::new("files-retry")
                        .ghost()
                        .small()
                        .label(rust_i18n::t!("Files.retry").into_owned())
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| this.reload(cx),
                        )),
                )
                .into_any_element(),
            None => {
                let view = cx.entity().downgrade();
                let painter = self.painter.clone();
                div()
                    .flex_1()
                    .min_h_0()
                    .px_1()
                    .child(tree(&self.state, move |_, entry, _, _, _| {
                        let item = entry.item();
                        let path = item.id.clone();
                        // Placeholder notes get no icon; files and folders follow the icon theme.
                        let icon = if entry.is_disabled() {
                            div().size(rems(1.)).flex_none().into_any_element()
                        } else if entry.is_folder() {
                            painter.folder(&item.label, entry.is_expanded())
                        } else {
                            painter.file(&item.label)
                        };
                        // One indent step per level, matching the sidebar's 1 rem rhythm.
                        let indent = rems(0.75 * entry.depth() as f32);
                        let row = ListItem::new(SharedString::from(format!(
                            "file-node-{path}"
                        )))
                        .py_0p5()
                        .child(
                            h_flex()
                                .gap_1p5()
                                .pl(indent)
                                .text_sm()
                                .when(entry.is_disabled(), |this| {
                                    this.text_color(muted)
                                })
                                .child(icon)
                                .child(item.label.clone()),
                        );
                        if entry.is_folder() || entry.is_disabled() {
                            return row;
                        }
                        let view = view.clone();
                        row.on_click(move |_, _, cx| {
                            let _ = view.update(cx, |_, cx| {
                                cx.emit(FilesEvent::Open(path.to_string()))
                            });
                        })
                    }))
                    .into_any_element()
            }
        };
        div()
            .id("files-view")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(body)
    }
}
