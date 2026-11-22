//! The Changes tab (M3b spec §6.2): the patch of every changed file of a
//! workspace, against the diff base or the index, beside a folder tree of
//! those files with their line counts; a file clicked in the tree scrolls the
//! patch to it.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{
    Diff, DiffFile, DiffFileStatus, DiffParseOptions, DiffState,
};
use gpui_kit::component::label::Label;
use gpui_kit::component::list::ListItem;
use gpui_kit::component::tree::{TreeEvent, TreeItem, TreeState, tree};
use gpui_kit::component::{
    ActiveTheme as _, ResizableState, Selectable as _, Sizable as _,
    StyledExt as _, Theme, h_flex, h_resizable, resizable_panel,
};
use gpui_kit::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render,
    SharedString, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    div, prelude::FluentBuilder as _, px, rems,
};
use protocol::{WorkspaceDiff, WorkspaceDiffScope, WorkspaceGitStatus};

use typed_builder::TypedBuilder;

use super::ViewerState;
use crate::backend::{AcpClient, BackendError};
use crate::file::ChangeNode;
use crate::ui::file_icon::FileIconPainter;

/// Width the file tree starts at, in rems; it is resizable.
const TREE_REMS: f32 = 12.0;

/// One changed file as the parsed patch names it.
#[derive(Debug, Clone, PartialEq, Eq, TypedBuilder)]
pub struct ChangedFile {
    /// Path after the change, as the patch names it.
    pub path: String,
    /// How the file changed.
    pub status: DiffFileStatus,
    /// Lines added.
    pub additions: usize,
    /// Lines removed.
    pub deletions: usize,
}

impl From<&DiffFile> for ChangedFile {
    /// The list row of one file of a parsed patch.
    fn from(file: &DiffFile) -> Self {
        Self::builder()
            .path(file.path().to_string())
            .status(file.status())
            .additions(file.additions())
            .deletions(file.deletions())
            .build()
    }
}

impl ChangedFile {
    /// The status letter and its theme color (M3 plan Global Constraints);
    /// the tree colors the file's name with it too.
    fn letter(&self, theme: &Theme) -> (&'static str, Hsla) {
        match self.status {
            DiffFileStatus::Added => ("A", theme.success),
            DiffFileStatus::Deleted => ("D", theme.danger),
            DiffFileStatus::Modified => ("M", theme.warning),
            DiffFileStatus::Renamed => ("R", theme.info),
            DiffFileStatus::Copied => ("C", theme.info),
            DiffFileStatus::Conflicted => ("U", theme.danger),
            // Unchanged reference files, and kinds a newer gpui-kit adds.
            _ => ("", theme.muted_foreground),
        }
    }
}

/// What one scope shows: the parsed patch and its files.
#[derive(TypedBuilder)]
pub struct ScopeContent {
    /// Hash of the patch text `diff` was parsed from, so an unchanged reload
    /// keeps the view without holding a second copy of the patch.
    #[builder(default)]
    patch: Option<u64>,
    /// Changed files in patch order.
    #[builder(default)]
    files: Vec<ChangedFile>,
    /// The parsed patch.
    #[builder(default)]
    diff: Option<Entity<DiffState>>,
    /// Whether the backend cut the patch at its size limit.
    #[builder(default)]
    truncated: bool,
    /// Where the content stands; `None` before the first read.
    #[builder(default)]
    state: Option<ViewerState>,
}

impl Default for ScopeContent {
    /// Nothing read yet.
    fn default() -> Self {
        Self::builder().build()
    }
}

/// The part of a workspace's Git status the Changes tab shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitSummary {
    /// A work tree diffed against `base`; empty when the base is `HEAD`.
    Repository { base: String },
    /// The workspace is not a Git work tree.
    NotRepository,
    /// No usable `git` executable was found.
    GitUnavailable,
}

impl From<&WorkspaceGitStatus> for GitSummary {
    /// Keeps only the base name or why there is no diff.
    fn from(status: &WorkspaceGitStatus) -> Self {
        match status {
            WorkspaceGitStatus::Repository { base, .. } => {
                Self::Repository { base: base.clone() }
            }
            WorkspaceGitStatus::NotRepository => Self::NotRepository,
            WorkspaceGitStatus::GitUnavailable => Self::GitUnavailable,
        }
    }
}

/// One workspace's Changes tab.
#[derive(TypedBuilder)]
pub struct ChangesView {
    /// Workspace whose changes are shown.
    workspace: PathBuf,
    /// The agent connection; `None` until the backend is ready.
    client: Option<AcpClient>,
    /// The scope shown.
    #[builder(default)]
    scope: WorkspaceDiffScope,
    /// Content per scope, so switching back shows the last read at once.
    #[builder(default)]
    contents: HashMap<WorkspaceDiffScope, ScopeContent>,
    /// The workspace's Git status as the shell last read it.
    #[builder(default)]
    git: Option<GitSummary>,
    /// The read in flight; a newer one replaces it.
    #[builder(default)]
    _load: Option<Task<()>>,
    /// The folder tree of the shown scope's files.
    tree: Entity<TreeState>,
    /// Folders the user folded, by path; they stay folded as the tree is
    /// rebuilt for new reads.
    #[builder(default)]
    collapsed: HashSet<String>,
    /// Tracks folders folding and unfolding.
    _tree_events: Subscription,
    /// The shown files by path: their place in the patch and their counts.
    #[builder(default)]
    tree_files: Rc<HashMap<String, (usize, ChangedFile)>>,
    /// Whether the file tree shows beside the patch.
    #[builder(default = true)]
    tree_open: bool,
    /// The sizes of the patch and the tree as the user drags between them.
    split: Entity<ResizableState>,
    /// Picks the icons of files and folders in the tree.
    painter: FileIconPainter,
}

impl ChangesView {
    /// A Changes tab of `workspace` that starts reading right away.
    pub fn new(
        workspace: PathBuf,
        client: Option<AcpClient>,
        painter: FileIconPainter,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let tree = cx.new(|cx| TreeState::new(cx));
        let events = cx.subscribe(&tree, Self::on_tree_event);
        let mut view = Self::builder()
            .workspace(workspace)
            .client(client)
            .tree(tree)
            ._tree_events(events)
            .split(cx.new(|_| ResizableState::default()))
            .painter(painter)
            .build();
        view.reload(cx);
        view
    }

    /// Takes the icon theme the shell uses now.
    pub fn set_painter(&mut self, painter: FileIconPainter) {
        self.painter = painter;
    }

    /// Lays the shown scope's files out as a folder tree. An unchanged file
    /// list keeps the tree as it is, its selection included; a new one keeps
    /// the folders the user folded.
    fn sync_tree(&mut self, cx: &mut Context<'_, Self>) {
        let files = self
            .contents
            .get(&self.scope)
            .map(|content| content.files.as_slice())
            .unwrap_or_default();
        let shown: HashMap<String, (usize, ChangedFile)> = files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.path.clone(), (index, file.clone())))
            .collect();
        if *self.tree_files == shown && !shown.is_empty() {
            return;
        }
        let items: Vec<TreeItem> =
            ChangeNode::tree(files.iter().map(|file| file.path.as_str()))
                .iter()
                .map(|node| self.tree_item(node))
                .collect();
        self.tree_files = Rc::new(shown);
        self.tree.update(cx, |tree, cx| tree.set_items(items, cx));
    }

    /// One row of the tree, open unless the user folded it.
    fn tree_item(&self, node: &ChangeNode) -> TreeItem {
        TreeItem::new(node.path.clone(), node.name.clone())
            .expanded(!self.collapsed.contains(&node.path))
            .children(node.children.iter().map(|child| self.tree_item(child)))
    }

    /// Remembers a folder folded or unfolded in the tree.
    fn on_tree_event(
        &mut self,
        _: Entity<TreeState>,
        event: &TreeEvent,
        _: &mut Context<'_, Self>,
    ) {
        match event {
            TreeEvent::Collapsed(path) => {
                self.collapsed.insert(path.to_string());
            }
            TreeEvent::Expanded(path) => {
                self.collapsed.remove(path.as_ref());
            }
        }
    }

    /// The workspace this tab shows.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Takes the shell's latest Git status of the workspace; a repository
    /// that just appeared is read.
    pub fn set_git(
        &mut self,
        status: Option<GitSummary>,
        cx: &mut Context<'_, Self>,
    ) {
        let was_repository = self.is_repository();
        self.git = status;
        if !was_repository && self.is_repository() {
            self.reload(cx);
        }
    }

    /// Whether the last status said this is a Git work tree; unknown counts as one.
    fn is_repository(&self) -> bool {
        !matches!(
            self.git,
            Some(GitSummary::NotRepository | GitSummary::GitUnavailable)
        )
    }

    /// Shows `scope`: its last content right away, then a fresh read.
    pub fn set_scope(
        &mut self,
        scope: WorkspaceDiffScope,
        cx: &mut Context<'_, Self>,
    ) {
        self.scope = scope;
        self.sync_tree(cx);
        self.reload(cx);
        cx.notify();
    }

    /// Reads the shown scope's patch again.
    pub fn reload(&mut self, cx: &mut Context<'_, Self>) {
        if !self.is_repository() {
            return;
        }
        let scope = self.scope;
        let content = self.contents.entry(scope).or_default();
        let Some(client) = self.client.clone() else {
            content.state = Some(ViewerState::Failed(
                rust_i18n::t!("Viewer.no_backend").into_owned(),
            ));
            return;
        };
        if content.state.is_none() {
            content.state = Some(ViewerState::Loading);
        }
        let workspace = self.workspace.clone();
        self._load = Some(cx.spawn(async move |this, cx| {
            let result = client.git_diff(&workspace, None, scope).await;
            let _ = this.update(cx, |this, cx| {
                this.show(scope, result, cx);
                if this.scope == scope {
                    this.sync_tree(cx);
                }
                cx.notify();
            });
        }));
    }

    /// Shows one scope's read: the parsed patch, or why there is none.
    fn show(
        &mut self,
        scope: WorkspaceDiffScope,
        result: Result<WorkspaceDiff, BackendError>,
        cx: &mut Context<'_, Self>,
    ) {
        let workspace = self.workspace.display().to_string();
        let content = self.contents.entry(scope).or_default();
        let state = match result {
            Ok(WorkspaceDiff::Patch { patch, truncated }) => {
                // An unchanged patch keeps the view and its scroll position.
                let mut hasher = DefaultHasher::new();
                patch.hash(&mut hasher);
                let hash = hasher.finish();
                if content.diff.is_some() && content.patch == Some(hash) {
                    content.truncated = truncated;
                    ViewerState::Ready
                } else {
                    // A patch cut at the size limit ends mid-hunk; let the parser accept that.
                    let options =
                        DiffParseOptions::new().truncated_hunks(truncated);
                    match DiffFile::parse_with(&patch, options) {
                        Ok(files) => {
                            content.files =
                                files.iter().map(ChangedFile::from).collect();
                            content.diff =
                                Some(cx.new(|cx| DiffState::new(files, cx)));
                            content.patch = Some(hash);
                            content.truncated = truncated;
                            ViewerState::Ready
                        }
                        Err(error) => {
                            tracing::warn!(
                                "unreadable diff of {}: {}",
                                workspace,
                                error
                            );
                            ViewerState::Failed(error.to_string())
                        }
                    }
                }
            }
            Ok(WorkspaceDiff::Unchanged | WorkspaceDiff::Binary) => {
                *content = ScopeContent::default();
                ViewerState::Notice(rust_i18n::t!("Changes.clean").into_owned())
            }
            Err(error) => {
                tracing::warn!("failed to diff {}: {}", workspace, error);
                ViewerState::Failed(error.to_string())
            }
        };
        content.state = Some(state);
    }

    /// Scrolls the patch to one file's header.
    fn reveal(&mut self, path: &str, cx: &mut Context<'_, Self>) {
        if let Some(diff) = self
            .contents
            .get(&self.scope)
            .and_then(|content| content.diff.clone())
        {
            diff.update(cx, |diff, cx| diff.scroll_to_file(path, cx));
        }
    }

    /// The scope switch, totals, and refresh button.
    fn render_header(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = cx.theme();
        let base = match &self.git {
            Some(GitSummary::Repository { base }) if !base.is_empty() => {
                base.clone()
            }
            _ => "HEAD".to_string(),
        };
        let files = self
            .contents
            .get(&self.scope)
            .map(|content| content.files.as_slice())
            .unwrap_or_default();
        let additions: usize = files.iter().map(|file| file.additions).sum();
        let deletions: usize = files.iter().map(|file| file.deletions).sum();
        let scope_button = |id: &'static str, label: String, scope| {
            Button::new(id)
                .ghost()
                .small()
                .label(label)
                .selected(self.scope == scope)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_scope(scope, cx)
                }))
        };
        h_flex()
            .flex_none()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .child(scope_button(
                "changes-scope-base",
                rust_i18n::t!("Changes.against", base = base).into_owned(),
                WorkspaceDiffScope::Base,
            ))
            .child(scope_button(
                "changes-scope-unstaged",
                rust_i18n::t!("Changes.unstaged").into_owned(),
                WorkspaceDiffScope::Unstaged,
            ))
            .child(div().flex_1())
            .child(
                h_flex()
                    .id("changes-totals")
                    .test_support()
                    .gap_1p5()
                    .font_family(theme.mono_font_family.clone())
                    .font_weight(crate::theme::FontWeights::get(cx).code)
                    .text_xs()
                    .child(
                        Label::new(format!("+{additions}"))
                            .text_xs()
                            .text_color(theme.success),
                    )
                    .child(
                        Label::new(format!("−{deletions}"))
                            .text_xs()
                            .text_color(theme.danger),
                    ),
            )
            .child(
                Button::new("changes-tree")
                    .ghost()
                    .small()
                    .icon(LucideIcon::FolderTree)
                    .selected(self.tree_open)
                    .tooltip(rust_i18n::t!("Changes.toggle_tree").into_owned())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.tree_open = !this.tree_open;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("changes-refresh")
                    .ghost()
                    .small()
                    .icon(LucideIcon::RefreshCw)
                    .tooltip(rust_i18n::t!("Changes.refresh").into_owned())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.reload(cx)
                    })),
            )
    }

    /// The shown files as a folder tree under a count: each file in its
    /// status color with its added and removed lines; a click scrolls the
    /// patch to it.
    fn render_tree(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let files = Rc::clone(&self.tree_files);
        let painter = self.painter.clone();
        let view = cx.entity().downgrade();
        let count = files.len();
        let rows = tree(&self.tree, move |_, entry, _, _, _| {
            let item = entry.item();
            let path = item.id.clone();
            // One indent step per level, as in the Files tab.
            let indent = rems(0.75 * entry.depth() as f32);
            let Some((index, file)) = files.get(path.as_ref()) else {
                return ListItem::new(SharedString::from(format!(
                    "change-folder-{path}"
                )))
                .py_0p5()
                .child(
                    h_flex()
                        .gap_1p5()
                        .pl(indent)
                        .text_sm()
                        .child(painter.folder(&item.label, entry.is_expanded()))
                        .child(Label::new(item.label.clone()).text_sm()),
                );
            };
            let (letter, color) = file.letter(&theme);
            let view = view.clone();
            ListItem::new(SharedString::from(format!("changed-file-{index}")))
                .py_0p5()
                .child(
                    h_flex()
                        .gap_1p5()
                        .pl(indent)
                        .text_sm()
                        .child(painter.file(&item.label))
                        .child(
                            div().flex_1().min_w_0().truncate().child(
                                Label::new(item.label.clone())
                                    .text_sm()
                                    .text_color(color),
                            ),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .gap_1()
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .child(
                                    Label::new(format!("+{}", file.additions))
                                        .text_xs()
                                        .text_color(theme.success),
                                )
                                .child(
                                    Label::new(format!("−{}", file.deletions))
                                        .text_xs()
                                        .text_color(theme.danger),
                                )
                                // The letter says the status in text, too.
                                .child(
                                    Label::new(letter)
                                        .text_xs()
                                        .font_semibold()
                                        .text_color(color),
                                ),
                        ),
                )
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.reveal(&path, cx));
                })
        });
        div()
            .id("changes-tree")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        Label::new(rust_i18n::t!("Changes.files").into_owned())
                            .text_xs()
                            .font_semibold(),
                    )
                    .child(div().flex_1())
                    .child(Label::new(count.to_string()).text_xs()),
            )
            .child(div().flex_1().min_h_0().px_1().child(rows))
            .into_any_element()
    }
}

impl Render for ChangesView {
    /// The header above the patch and its file tree, or the state that
    /// replaces them.
    fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let header = self.render_header(cx).into_any_element();
        let notice = match &self.git {
            Some(GitSummary::NotRepository) => {
                Some(rust_i18n::t!("Changes.not_repository").into_owned())
            }
            Some(GitSummary::GitUnavailable) => {
                Some(rust_i18n::t!("Changes.git_missing").into_owned())
            }
            _ => None,
        };
        let content = self.contents.get(&self.scope);
        let state = notice
            .map(ViewerState::Notice)
            .or_else(|| content.and_then(|content| content.state.clone()))
            .unwrap_or(ViewerState::Loading);
        let placeholder = state
            .render_placeholder(|this: &mut Self, _, cx| this.reload(cx), cx);
        let ready =
            content
                .filter(|_| placeholder.is_none())
                .and_then(|content| {
                    Some((content.diff.clone()?, content.truncated))
                });
        let warning = cx.theme().warning;
        let code_weight = crate::theme::FontWeights::get(cx).code;
        let tree =
            (self.tree_open && ready.is_some()).then(|| self.render_tree(cx));
        div()
            .id("changes-view")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .when_some(placeholder, |this, placeholder| this.child(placeholder))
            .when_some(ready, |this, (diff, truncated)| {
                let patch = div()
                    .id("changes-diff")
                    .test_support()
                    .size_full()
                    .font_weight(code_weight)
                    .child(
                        Diff::new(&diff)
                            .line_number(true)
                            .syntax_highlight(true),
                    );
                // The patch takes the room; the tree keeps its width, like
                // the review pane it follows.
                let mut split = h_resizable("changes-split")
                    .with_state(&self.split)
                    .child(resizable_panel().child(patch));
                if let Some(tree) = tree {
                    split = split.child(
                        resizable_panel()
                            .size(window.rem_size() * TREE_REMS)
                            .size_range(px(160.)..Pixels::MAX)
                            .child(tree),
                    );
                }
                this.when(truncated, |this| {
                    this.child(
                        div().flex_none().px_3().py_1().child(
                            Label::new(
                                rust_i18n::t!("Viewer.diff_truncated")
                                    .into_owned(),
                            )
                            .text_xs()
                            .text_color(warning),
                        ),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(split),
                )
            })
    }
}
