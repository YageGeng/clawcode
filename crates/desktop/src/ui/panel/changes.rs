//! The Changes tab (M3b spec §6.2): a workspace's changed files above the
//! patch of every one of them, against the diff base or the index.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{
    Diff, DiffFile, DiffFileStatus, DiffParseOptions, DiffState,
};
use gpui_kit::component::label::Label;
use gpui_kit::component::{
    ActiveTheme as _, Selectable as _, Sizable as _, StyledExt as _, Theme,
    h_flex,
};
use gpui_kit::{
    AppContext as _, ClickEvent, Context, Entity, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task,
    TestSupportExt as _, Window, div, prelude::FluentBuilder as _, rems,
    uniform_list,
};
use protocol::{WorkspaceDiff, WorkspaceDiffScope, WorkspaceGitStatus};

use typed_builder::TypedBuilder;

use super::ViewerState;
use crate::backend::{AcpClient, BackendError};

/// Height of one file row in rems: small text plus its vertical padding.
const ROW_REMS: f32 = 1.75;
/// Tallest the file list grows before it scrolls, in rems.
const LIST_MAX_REMS: f32 = 12.0;

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
    /// The status letter and its theme color (M3 plan Global Constraints).
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
}

impl ChangesView {
    /// A Changes tab of `workspace` that starts reading right away.
    pub fn new(
        workspace: PathBuf,
        client: Option<AcpClient>,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut view =
            Self::builder().workspace(workspace).client(client).build();
        view.reload(cx);
        view
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

    /// The changed files within `range`, as the list scrolls.
    fn render_rows(
        &self,
        range: std::ops::Range<usize>,
        cx: &mut Context<'_, Self>,
    ) -> Vec<gpui_kit::AnyElement> {
        let theme = cx.theme().clone();
        let start = range.start;
        // Only the rows in view are drawn; the file list itself is borrowed, not copied.
        self.contents
            .get(&self.scope)
            .and_then(|content| content.files.get(range))
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(offset, file)| {
                let (letter, color) = file.letter(&theme);
                let (directory, name) = super::split_path(&file.path);
                let (directory, name) = (directory.to_owned(), name.to_owned());
                let path = file.path.clone();
                div()
                    .id(SharedString::from(format!(
                        "changed-file-{}",
                        start + offset
                    )))
                    .test_support()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_0p5()
                    .rounded(theme.radius)
                    .text_sm()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.accent))
                    .child(
                        div().w_3().flex_none().child(
                            Label::new(letter)
                                .text_sm()
                                .font_semibold()
                                .text_color(color),
                        ),
                    )
                    .child(div().flex_none().child(Label::new(name).text_sm()))
                    // Files at the workspace root have no directory to show.
                    .child(div().flex_1().min_w_0().truncate().when(
                        !directory.is_empty(),
                        |this| {
                            this.child(
                                Label::new(directory)
                                    .text_xs()
                                    .text_color(theme.muted_foreground),
                            )
                        },
                    ))
                    .child(
                        Label::new(format!(
                            "+{} −{}",
                            file.additions, file.deletions
                        ))
                        .text_xs()
                        .text_color(theme.muted_foreground),
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.reveal(&path, cx)
                        },
                    ))
                    .into_any_element()
            })
            .collect()
    }
}

impl Render for ChangesView {
    /// The header above the file list and the patch, or the state that replaces them.
    fn render(
        &mut self,
        _window: &mut Window,
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
                    Some((
                        content.diff.clone()?,
                        content.files.len(),
                        content.truncated,
                    ))
                });
        let warning = cx.theme().warning;
        let code_weight = crate::theme::FontWeights::get(cx).code;
        div()
            .id("changes-view")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .when_some(placeholder, |this, placeholder| this.child(placeholder))
            .when_some(ready, |this, (diff, count, truncated)| {
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
                    // Only the rows in view are built: a large change set must not slow every frame.
                    uniform_list(
                        "changes-files",
                        count,
                        cx.processor(|this, range, _, cx| {
                            this.render_rows(range, cx)
                        }),
                    )
                    .flex_none()
                    // A virtual list needs a definite height: its rows, up to 12rem.
                    .h(rems((count as f32 * ROW_REMS).min(LIST_MAX_REMS)))
                    .px_1(),
                )
                .child(
                    div()
                        .id("changes-diff")
                        .test_support()
                        .flex_1()
                        .min_h_0()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .font_weight(code_weight)
                        .child(
                            Diff::new(&diff)
                                .line_number(true)
                                .syntax_highlight(true),
                        ),
                )
            })
    }
}
