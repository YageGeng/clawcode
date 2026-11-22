//! The Changes tab (M3b spec §6.2, M4b spec §4): a folder tree of a
//! workspace's changed files beside their patches, against the diff base or
//! the index. The file list comes first; each file's patch loads as its
//! header comes into view, or at once when picked in the tree.

mod content;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{Diff, DiffFile, DiffState};
use gpui_kit::component::label::Label;
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::tree::{TreeEvent, TreeItem, TreeState, tree};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, ResizableState, Selectable as _, Side,
    Sizable as _, StyledExt as _, h_flex, h_resizable, resizable_panel, v_flex,
};
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render,
    SharedString, Styled as _, Subscription, Task, TestSupportExt as _,
    WeakEntity, Window, div, prelude::FluentBuilder as _, px, rems,
};
use protocol::{
    RunId, WorkspaceChange, WorkspaceChanges, WorkspaceDiffScope,
    WorkspaceGitStatus,
};

use typed_builder::TypedBuilder;

use super::ViewerState;
use crate::backend::SessionId;
use crate::backend::{AcpClient, BackendError};
use crate::file::ChangeNode;
use crate::projection::ProjectionChange;
use crate::session::SessionHandle;
use crate::ui::LineCounts;
use crate::ui::file_icon::FileIconPainter;
use content::{FileNote, ScopeContent, TreeFiles, kind_color};

/// Width the file tree starts at, in rems; it is resizable.
const TREE_REMS: f32 = 12.0;

/// `text` cut to `limit` characters, with an ellipsis when cut.
fn shorten(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", text.get(..end).unwrap_or(text)),
        None => text.to_owned(),
    }
}

/// What the Changes tab compares: the work tree against Git, or the
/// snapshots around one run of a session (M4b spec §6.2).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChangesScope {
    /// The work tree against the diff base or the index.
    Git(WorkspaceDiffScope),
    /// What one run of a session changed.
    Run {
        /// The session the run belongs to.
        session: SessionId,
        /// The run.
        run: RunId,
    },
}

impl Default for ChangesScope {
    /// The work tree against the diff base.
    fn default() -> Self {
        Self::Git(WorkspaceDiffScope::Base)
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

/// The file headers painted since the view last looked, which tells it
/// which patches are in view.
#[derive(Default)]
pub struct SeenHeaders {
    /// Paths of the painted headers.
    paths: RefCell<Vec<String>>,
    /// Whether the view is already due to look.
    scheduled: Cell<bool>,
}

impl SeenHeaders {
    /// Notes a painted header; the view looks once, after the frame.
    fn saw(
        self: &Rc<Self>,
        path: &str,
        view: &WeakEntity<ChangesView>,
        cx: &mut App,
    ) {
        self.paths.borrow_mut().push(path.to_owned());
        if self.scheduled.replace(true) {
            return;
        }
        let view = view.clone();
        cx.defer(move |cx| {
            let _ = view.update(cx, |view, cx| view.load_seen(cx));
        });
    }

    /// The paths noted since the last look.
    fn take(&self) -> Vec<String> {
        self.scheduled.set(false);
        std::mem::take(&mut *self.paths.borrow_mut())
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
    scope: ChangesScope,
    /// Content per scope, so switching back shows the last read at once.
    #[builder(default)]
    contents: HashMap<ChangesScope, ScopeContent>,
    /// The session whose runs the tab offers, when it belongs to one.
    #[builder(default)]
    session: Option<Entity<SessionHandle>>,
    /// Follows the session's list of runs that changed the workspace.
    #[builder(default)]
    _session_changes: Option<Subscription>,
    /// The workspace's Git status as the shell last read it.
    #[builder(default)]
    git: Option<GitSummary>,
    /// The list read in flight; a newer one replaces it.
    #[builder(default)]
    _load: Option<Task<()>>,
    /// Patch reads in flight.
    #[builder(default)]
    _patches: Vec<Task<()>>,
    /// File headers painted since the last look.
    #[builder(default)]
    seen: Rc<SeenHeaders>,
    /// The headers painted at the last look that saw any, which pick the
    /// file kept in view when the diff's files are replaced.
    #[builder(default)]
    last_seen: Vec<String>,
    /// A file picked in the tree whose patch is on the way; it stays in view
    /// until it lands.
    #[builder(default)]
    pending_reveal: Option<String>,
    /// The folder tree of the shown scope's files.
    tree: Entity<TreeState>,
    /// Folders the user folded, by path; they stay folded as the tree is
    /// rebuilt for new reads.
    #[builder(default)]
    collapsed: HashSet<String>,
    /// Tracks folders folding and unfolding.
    _tree_events: Subscription,
    /// The files the tree shows, as the shown scope last listed them.
    #[builder(default)]
    tree_files: TreeFiles,
    /// Whether the file tree shows beside the patches.
    #[builder(default = true)]
    tree_open: bool,
    /// The sizes of the tree and the patches as the user drags between them.
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
            .map(|content| Rc::clone(&content.tree_files))
            .unwrap_or_default();
        if Rc::ptr_eq(&self.tree_files, &files) && !files.is_empty() {
            return;
        }
        let changes = self
            .contents
            .get(&self.scope)
            .map(|content| content.loader.changes())
            .unwrap_or_default();
        let items: Vec<TreeItem> =
            ChangeNode::tree(changes.iter().map(|change| change.path.as_str()))
                .iter()
                .map(|node| self.tree_item(node))
                .collect();
        self.tree_files = files;
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
        scope: ChangesScope,
        cx: &mut Context<'_, Self>,
    ) {
        // Only the Git scopes and the shown run keep their content: each run
        // looked at would otherwise hold its diff for the tab's lifetime.
        self.contents.retain(|kept, _| {
            matches!(kept, ChangesScope::Git(_)) || *kept == scope
        });
        self.scope = scope;
        self.sync_tree(cx);
        self.reload(cx);
        cx.notify();
    }

    /// Reads the shown scope's file list again; a run's list is the one the
    /// session read with its runs.
    pub fn reload(&mut self, cx: &mut Context<'_, Self>) {
        if !self.is_repository() {
            return;
        }
        let scope = self.scope.clone();
        if let ChangesScope::Run { run, .. } = &scope {
            let listed = self.session.as_ref().and_then(|session| {
                session
                    .read(cx)
                    .checkpoints()
                    .iter()
                    .find(|checkpoint| checkpoint.run_id == *run)
                    .map(|checkpoint| checkpoint.changes.clone())
            });
            // A run no longer listed changed nothing that is left to show.
            self.show_changes(scope, Ok(listed.unwrap_or_default()), cx);
            self.sync_tree(cx);
            cx.notify();
            return;
        }
        let ChangesScope::Git(git_scope) = scope else {
            return;
        };
        let content = self.contents.entry(scope.clone()).or_default();
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
            let result = client.git_changes(&workspace, git_scope).await;
            let _ = this.update(cx, |this, cx| {
                this.show_changes(scope.clone(), result, cx);
                if this.scope == scope {
                    this.sync_tree(cx);
                }
                cx.notify();
            });
        }));
    }

    /// Reads everything again at the user's request: the file list and every
    /// patch, which the automatic reloads keep while a file's change looks
    /// the same.
    fn refresh(&mut self, cx: &mut Context<'_, Self>) {
        let scope = self.scope.clone();
        if let Some(content) = self.contents.get_mut(&scope) {
            content.forget_patches();
            self.install(scope, cx);
        }
        self.reload(cx);
    }

    /// Shows what `run` changed, and `path` within it when given; the tab
    /// opens on it from a turn's change card.
    pub fn show_run(
        &mut self,
        run: RunId,
        path: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(session) = self.session_id(cx) else {
            return;
        };
        self.set_scope(ChangesScope::Run { session, run }, cx);
        if let Some(path) = path {
            self.reveal_file(&path, cx);
        }
    }

    /// The id of the session whose runs the tab offers.
    fn session_id(&self, cx: &App) -> Option<SessionId> {
        Some(self.session.as_ref()?.read(cx).id().clone())
    }

    /// Follows `session`'s runs, which the turn picker lists.
    pub fn set_session(
        &mut self,
        session: Option<Entity<SessionHandle>>,
        cx: &mut Context<'_, Self>,
    ) {
        if self.session == session {
            return;
        }
        self._session_changes = session.as_ref().map(|session| {
            cx.subscribe(session, |this, _, change: &ProjectionChange, cx| {
                if change.checkpoints {
                    // A shown run may have been read again.
                    if matches!(this.scope, ChangesScope::Run { .. }) {
                        this.reload(cx);
                    }
                    cx.notify();
                }
            })
        });
        self.session = session;
        cx.notify();
    }

    /// Shows one scope's file list: placeholders for files without a patch
    /// yet, whose patches then load as they come into view.
    fn show_changes(
        &mut self,
        scope: ChangesScope,
        result: Result<WorkspaceChanges, BackendError>,
        cx: &mut Context<'_, Self>,
    ) {
        let content = self.contents.entry(scope.clone()).or_default();
        match result {
            Ok(changes) if changes.changes.is_empty() => {
                *content = ScopeContent::default();
                content.state = Some(ViewerState::Notice(
                    rust_i18n::t!("Changes.clean").into_owned(),
                ));
            }
            Ok(changes) => {
                let changed = content.take_files(&changes);
                content.state = Some(ViewerState::Ready);
                if changed {
                    self.install(scope, cx);
                }
            }
            Err(error) => {
                tracing::warn!(
                    "failed to list changes of {}: {}",
                    self.workspace.display(),
                    error
                );
                content.state = Some(ViewerState::Failed(error.to_string()));
            }
        }
    }

    /// Puts the scope's current files into its diff, then brings a file
    /// back into view, since replacing the files scrolls the diff to its top:
    /// the file the user picked, else the first of the headers painted last.
    /// Both are read now, not when the patches were asked for, so a scroll
    /// made meanwhile is kept.
    fn install(&mut self, scope: ChangesScope, cx: &mut Context<'_, Self>) {
        let Some(content) = self.contents.get_mut(&scope) else {
            return;
        };
        let anchor = self
            .pending_reveal
            .clone()
            .or_else(|| content.loader.anchor(&self.last_seen));
        content.refresh_headers();
        let files = content.diff_files();
        match &content.diff {
            Some(diff) => diff.update(cx, |diff, cx| {
                diff.set_files(files, cx);
                if let Some(anchor) = &anchor {
                    diff.scroll_to_file(anchor, cx);
                }
            }),
            None => content.diff = Some(cx.new(|cx| DiffState::new(files, cx))),
        }
        cx.notify();
    }

    /// Loads the patches of the files whose headers were painted, a batch
    /// at a time.
    fn load_seen(&mut self, cx: &mut Context<'_, Self>) {
        let seen = self.seen.take();
        if !seen.is_empty() {
            self.last_seen.clone_from(&seen);
        }
        let scope = self.scope.clone();
        let Some(content) = self.contents.get_mut(&scope) else {
            return;
        };
        let batch = content.loader.next_batch(&seen);
        if !batch.is_empty() {
            self.request(scope, batch, cx);
        }
    }

    /// Reads the patches of `paths` together and shows them: against Git,
    /// or between a run's snapshots.
    fn request(
        &mut self,
        scope: ChangesScope,
        paths: Vec<String>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let workspace = self.workspace.clone();
        // Each read goes by the file's change: its raw name and old path.
        let files: Vec<WorkspaceChange> = self
            .contents
            .get(&scope)
            .map(|content| {
                paths
                    .iter()
                    .filter_map(|path| content.loader.change(path).cloned())
                    .collect()
            })
            .unwrap_or_default();
        // Drop finished reads; the list stays as long as the reads in flight.
        self._patches.retain(|task| !task.is_ready());
        self._patches.push(cx.spawn(async move |this, cx| {
            let reads = files.iter().map(|change| {
                let client = client.clone();
                let workspace = workspace.clone();
                let scope = scope.clone();
                async move {
                    match &scope {
                        ChangesScope::Git(git_scope) => {
                            client
                                .git_diff(&workspace, change, *git_scope)
                                .await
                        }
                        ChangesScope::Run { session, run } => {
                            client
                                .checkpoint_diff(
                                    &workspace, session, run, change,
                                )
                                .await
                        }
                    }
                }
            });
            let results = futures::future::join_all(reads).await;
            let _ = this.update(cx, |this, cx| {
                let Some(content) = this.contents.get_mut(&scope) else {
                    return;
                };
                for (change, result) in files.iter().zip(results) {
                    content.take_patch(&change.path, result);
                }
                this.install(scope, cx);
                // The picked file arrived and is in view; scrolling is the user's again.
                if this
                    .pending_reveal
                    .as_ref()
                    .is_some_and(|picked| paths.contains(picked))
                {
                    this.pending_reveal = None;
                }
            });
        }));
    }

    /// Scrolls the diff to one file at once, then loads its patch if it has
    /// not arrived; the file stays in view when the patch lands.
    pub fn reveal_file(&mut self, path: &str, cx: &mut Context<'_, Self>) {
        let scope = self.scope.clone();
        let Some(content) = self.contents.get_mut(&scope) else {
            return;
        };
        if let Some(diff) = content.diff.clone() {
            diff.update(cx, |diff, cx| diff.scroll_to_file(path, cx));
        }
        let requested = content.loader.request_now(path);
        let arrived = content.notes.contains_key(path);
        self.pending_reveal = (!arrived).then(|| path.to_owned());
        if requested {
            self.request(scope, vec![path.to_owned()], cx);
        }
    }

    /// The scope switch, totals, and tree and refresh buttons.
    fn render_header(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let turns = self.render_turns(cx);
        let theme = cx.theme();
        let base = match &self.git {
            Some(GitSummary::Repository { base }) if !base.is_empty() => {
                base.clone()
            }
            _ => "HEAD".to_string(),
        };
        let (additions, deletions) = self
            .contents
            .get(&self.scope)
            .map(|content| content.totals)
            .unwrap_or_default();
        let scope_button = |id: &'static str, label: String, scope| {
            Button::new(id)
                .ghost()
                .small()
                .label(label)
                .selected(self.scope == ChangesScope::Git(scope))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_scope(ChangesScope::Git(scope), cx)
                }))
        };
        h_flex()
            .flex_none()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            // The scopes give way in a narrow panel; the totals and the
            // tree and refresh buttons stay.
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .gap_1()
                    .child(scope_button(
                        "changes-scope-base",
                        rust_i18n::t!("Changes.against", base = base)
                            .into_owned(),
                        WorkspaceDiffScope::Base,
                    ))
                    .child(scope_button(
                        "changes-scope-unstaged",
                        rust_i18n::t!("Changes.unstaged").into_owned(),
                        WorkspaceDiffScope::Unstaged,
                    ))
                    .child(turns),
            )
            .child(
                h_flex()
                    .id("changes-totals")
                    .test_support()
                    .font_family(theme.mono_font_family.clone())
                    .font_weight(crate::theme::FontWeights::get(cx).code)
                    .child(LineCounts::new(additions, deletions)),
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
                        this.refresh(cx)
                    })),
            )
    }

    /// The turn picker: the session's runs that changed the workspace and
    /// whose reply is in its transcript, newest first.
    fn render_turns(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        // Each run with its label, newest first.
        let runs: Vec<(RunId, String)> = self
            .session
            .as_ref()
            .map(|session| {
                let session = session.read(cx);
                session
                    .projection()
                    .shown_runs(session.checkpoints())
                    .into_iter()
                    .enumerate()
                    .map(|(index, shown)| {
                        let label = if index == 0 {
                            rust_i18n::t!("Changes.latest_turn").into_owned()
                        } else {
                            rust_i18n::t!(
                                "Changes.turn",
                                n = shown.ordinal,
                                prompt = shorten(&shown.prompt, 40)
                            )
                            .into_owned()
                        };
                        (shown.run.run_id.clone(), label)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let selected = match &self.scope {
            ChangesScope::Run { run, .. } => Some(run.clone()),
            ChangesScope::Git(_) => None,
        };
        let session = self.session_id(cx);
        let empty = runs.is_empty();
        let view = cx.entity().downgrade();
        Button::new("changes-scope-turns")
            .ghost()
            .small()
            .label(rust_i18n::t!("Changes.turns").into_owned())
            .dropdown_caret(true)
            .selected(selected.is_some())
            .disabled(empty)
            .when(empty, |this| {
                this.tooltip(rust_i18n::t!("Changes.no_turns").into_owned())
            })
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu.check_side(Side::Right);
                let Some(session) = &session else {
                    return menu;
                };
                for (run, label) in &runs {
                    let view = view.clone();
                    let scope = ChangesScope::Run {
                        session: session.clone(),
                        run: run.clone(),
                    };
                    menu = menu.item(
                        PopupMenuItem::new(label.clone())
                            .checked(selected.as_ref() == Some(run))
                            .on_click(move |_, _, cx| {
                                let scope = scope.clone();
                                let _ = view.update(cx, |this, cx| {
                                    this.set_scope(scope, cx)
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }

    /// The shown files as a folder tree under a count: each file in its
    /// status color with its added and removed lines; a click loads it and
    /// scrolls the diff to it.
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
            let color = kind_color(file.kind, &theme);
            let letter = file.kind.letter().to_string();
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
                                .font_family(theme.mono_font_family.clone())
                                .child(LineCounts::of(file))
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
                    let _ =
                        view.update(cx, |this, cx| this.reveal_file(&path, cx));
                })
        });
        div()
            .id("changes-tree")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .border_r_1()
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

    /// The diff of the shown scope with its own file headers: path, the
    /// list's line counts, and the patch's state. Painting a header also
    /// tells the view the file is in view, so its patch loads.
    fn render_diff(
        &self,
        diff: &Entity<DiffState>,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let headers = self
            .contents
            .get(&self.scope)
            .map(|content| Rc::clone(&content.headers))
            .unwrap_or_default();
        let seen = Rc::clone(&self.seen);
        let view = cx.entity().downgrade();
        let weight = crate::theme::FontWeights::get(cx).code;
        div()
            .id("changes-diff")
            .test_support()
            .size_full()
            .font_weight(weight)
            .child(
                Diff::new(diff)
                    .line_number(true)
                    .syntax_highlight(true)
                    .render_header(move |file, _, cx| {
                        let path = file.path().to_string();
                        seen.saw(&path, &view, cx);
                        Self::render_file_header(file, headers.get(&path), cx)
                    }),
            )
            .into_any_element()
    }

    /// One file's header: its path (old → new for a rename), counts, and a
    /// note while its patch is missing, cut, or binary.
    fn render_file_header(
        file: &DiffFile,
        info: Option<&(WorkspaceChange, FileNote)>,
        cx: &App,
    ) -> AnyElement {
        let theme = cx.theme();
        let name: SharedString =
            match (file.original_path(), file.modified_path()) {
                (Some(old), Some(new)) if old != new => {
                    format!("{old} → {new}").into()
                }
                _ => file.path().clone(),
            };
        let note = info.and_then(|(_, note)| match note {
            FileNote::Loading => Some(rust_i18n::t!("Changes.loading_file")),
            FileNote::TooLarge => Some(rust_i18n::t!("Changes.file_too_large")),
            FileNote::Binary => Some(rust_i18n::t!("Changes.binary")),
            FileNote::Failed(error) => {
                Some(std::borrow::Cow::Owned(error.clone()))
            }
            FileNote::Loaded => None,
        });
        let line = h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .font_medium()
                    .child(name),
            )
            .when_some(info, |this, (change, _)| {
                this.child(LineCounts::of(change))
            });
        // The note goes under the path, so a narrow panel still shows it.
        v_flex()
            .w_full()
            .min_w_0()
            .child(line)
            .when_some(note, |this, note| {
                this.child(
                    Label::new(note.into_owned())
                        .text_xs()
                        .text_color(theme.muted_foreground),
                )
            })
            .into_any_element()
    }
}

impl Render for ChangesView {
    /// The header above the file tree and the diff, or the state that
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
        let ready = ready
            .map(|(diff, truncated)| (self.render_diff(&diff, cx), truncated));
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
                // The tree keeps its width on the left, the diff takes the rest.
                let mut split =
                    h_resizable("changes-split").with_state(&self.split);
                if let Some(tree) = tree {
                    split = split.child(
                        resizable_panel()
                            .size(window.rem_size() * TREE_REMS)
                            .size_range(px(160.)..Pixels::MAX)
                            .child(tree),
                    );
                }
                split = split.child(resizable_panel().child(diff));
                this.when(truncated, |this| {
                    this.child(
                        div().flex_none().px_3().py_1().child(
                            Label::new(
                                rust_i18n::t!("Changes.too_many_files")
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
