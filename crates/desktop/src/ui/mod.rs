//! The workspace shell: chrome (title bar, sidebar) holding two inset cards, the rail and the content card.

mod composer;
mod details;
mod file_icon;
mod format;
mod interactions;
mod panel;
mod session;
mod settings;
mod shell;
mod sidebar;
mod status;
mod trajectory;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use futures::channel::mpsc::UnboundedReceiver;
use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::AppMenuBar;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, ResizableState, WindowExt as _};
use gpui_kit::{
    AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    Styled as _, Subscription, Task, Window, div,
};
use typed_builder::TypedBuilder;

pub use composer::attachments::{
    AttachmentError, AttachmentSet, ImageAttachment, Outgoing,
};
pub use composer::command_palette::filter_commands;
pub use composer::{Composer, ComposerEvent};
pub use details::{DetailsDensity, EditorCatalog};
pub use format::{Elapsed, TokenCount, single_line};
pub use interactions::elicitation_form::{
    ElicitationForm, FieldError, FieldKind, FieldValue, FormField, Unrenderable,
};
pub use interactions::questions::QuestionAnswer;
pub use panel::{FileView, PanelOwner, PanelPresentation};
pub use panel::{PromptMode, RecordSection, RecordView};
pub use session::{
    MarkdownCache, MarkdownPart, SessionTarget, SessionView, SessionViewEvent,
    ViewMode,
};
pub use shell::tabs::Tabs;
pub use sidebar::{
    GroupKind, RECENT_SESSIONS, SESSIONS_PER_WORKSPACE, SessionRow,
    SidebarGroupModel, SidebarModel, SidebarRow, SidebarState, WorkspaceRow,
};
pub use status::{StatusDot, StatusIcon};
pub use trajectory::{
    ContentState, LoadState, TrajectoryEvent, TrajectoryView,
};

use crate::actions::{
    CloseSearch, FocusSearch, NewSession, ToggleSidebar, WORKSPACE_CONTEXT,
};
use crate::backend::{AcpClient, BackendError, BackendEvent, SessionId};
use crate::catalog::SessionCatalog;
use crate::i18n::LocaleGeneration;
use crate::session::{RegistryEvent, SessionRegistry, SessionStatus};
use crate::state::DesktopState;

/// One tab of the main card.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TabEntry {
    /// An existing session.
    Session(SessionId),
    /// A new session not created in the backend yet.
    Draft {
        /// Workspace the session will run in.
        workspace: Option<PathBuf>,
    },
}

impl TabEntry {
    /// Stable identity used for element ids: `session:{id}` or `draft:{workspace}`.
    pub fn key(&self) -> SharedString {
        match self {
            TabEntry::Session(id) => format!("session:{id}").into(),
            TabEntry::Draft { workspace } => format!(
                "draft:{}",
                workspace
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
            )
            .into(),
        }
    }
}

/// Where the agent connection stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendPhase {
    /// The backend is starting.
    Starting,
    /// Connected and serving.
    Ready,
    /// Startup failed; carries the error text.
    Failed(String),
    /// The connection ended; carries its error, if any.
    Stopped(Option<String>),
}

/// Everything that belongs to one agent connection.
///
/// The desktop has one local host today; remote hosts reached through a relay
/// or Tailscale will each get their own `HostState` (spec §4.6).
///
/// `HostState::builder().build()` is a host whose backend is still starting.
#[derive(TypedBuilder)]
pub struct HostState {
    #[builder(default = BackendPhase::Starting)]
    phase: BackendPhase,
    /// The in-process backend this host owns; remote hosts will have none.
    #[builder(default)]
    backend: Option<crate::backend::Backend>,
    #[builder(default)]
    client: Option<AcpClient>,
    #[builder(default)]
    registry: Option<Entity<SessionRegistry>>,
    #[builder(default)]
    catalog: SessionCatalog,
    /// Keeps the backend event pump alive.
    #[builder(default)]
    _pump: Option<Task<()>>,
}

/// Root view of the desktop window.
#[derive(TypedBuilder)]
pub struct AppView {
    state: DesktopState,
    state_path: PathBuf,
    host: HostState,
    #[builder(default)]
    tabs: Tabs,
    /// Scroll state of the tab strip, so the active tab can be scrolled into view.
    ///
    /// ponytail: gpui drops a scroll request made before the strip's first
    /// layout, so a restored active tab is not scrolled to at startup; retry
    /// the request after the first frame if that matters.
    #[builder(default)]
    tab_scroll: ScrollHandle,
    /// Content views of the tabs shown so far, created on first render.
    #[builder(default)]
    views: HashMap<TabEntry, Entity<SessionView>>,
    /// The right panels' views and what showed on the last frame.
    panels: panel::PanelHost,
    /// The details card's editors and fold state.
    #[builder(default)]
    details: details::DetailsHost,
    /// The settings dialog's state.
    #[builder(default)]
    settings: settings::SettingsHost,
    /// Catppuccin icons already decoded, by name and flavor.
    #[builder(default)]
    file_icon_cache: file_icon::FileIconCache,
    /// The last Git status read per workspace: the Changes list, rail count, and row branch.
    #[builder(default)]
    git: HashMap<PathBuf, panel::GitSnapshot>,
    search: Entity<InputState>,
    /// Search, filter, and expansion state of the session sidebar.
    #[builder(default)]
    sidebar: SidebarState,
    /// Registered workspace directories that no longer exist.
    #[builder(default)]
    missing: HashSet<PathBuf>,
    sidebar_split: Entity<ResizableState>,
    menu_bar: Entity<AppMenuBar>,
    focus_handle: FocusHandle,
    #[builder(default)]
    _subscriptions: Vec<Subscription>,
    /// Pending debounced save of `desktop.json`.
    #[builder(default)]
    _save_task: Option<Task<()>>,
    /// Redraws the shell while a run is active and refreshes relative ages every minute.
    #[builder(default)]
    _ticker: Option<Task<()>>,
    /// Set once quitting started, so a second request does nothing.
    #[builder(default)]
    quitting: bool,
    /// Agent notices waiting to become toasts on the next frame, which has the window.
    #[builder(default)]
    notices: Vec<(String, Option<String>)>,
}

impl AppView {
    /// Builds the shell from the saved state; the backend is attached later through [`Self::set_backend`].
    pub fn new(
        state: DesktopState,
        state_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("Sidebar.search").to_string())
        });
        let subscriptions = vec![
            cx.subscribe(
                &search,
                |_, _, _: &gpui_kit::component::input::InputEvent, cx| {
                    cx.notify()
                },
            ),
            // Cached strings such as the search placeholder follow language switches.
            cx.observe_global_in::<LocaleGeneration>(
                window,
                |this, window, cx| {
                    this.search.update(cx, |search, cx| {
                        search.set_placeholder(
                            rust_i18n::t!("Sidebar.search").to_string(),
                            window,
                            cx,
                        );
                    });
                    shell::title_bar::install_menus(&this.menu_bar, cx);
                },
            ),
        ];
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let tabs = Tabs::restored(&state.tabs);
        let mut view = AppView::builder()
            .state(state)
            .state_path(state_path)
            .host(HostState::builder().build())
            .tabs(tabs)
            .search(search)
            .sidebar_split(cx.new(|_| ResizableState::default()))
            .menu_bar(AppMenuBar::new(cx))
            .focus_handle(focus_handle)
            .panels(panel::PanelHost::new(cx.focus_handle()))
            ._subscriptions(subscriptions)
            .build();
        view.refresh_missing();
        cx.set_global(trajectory::TimelineVisible(
            view.state.layout.timeline_open,
        ));
        shell::title_bar::install_menus(&view.menu_bar, cx);
        view.observe_window(window, cx);
        view._ticker = Some(cx.spawn(async move |this, cx| {
            let mut seconds = 0u32;
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                seconds = (seconds + 1) % 60;
                let alive = this.update(cx, |this, cx| {
                    // The sidebar's relative ages advance once a minute.
                    if seconds == 0 {
                        cx.notify();
                    }
                    // Elapsed labels advance every second, in the transcript
                    // only; the card header and the sidebar repaint with it
                    // because a view's redraw also redraws its ancestors.
                    if let Some(view) = this.ticking_view(cx) {
                        view.update(cx, |_, cx| cx.notify());
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        }));
        view
    }

    /// The active tab's view while its elapsed times must advance: its
    /// session has a run in progress or a tool still executing.
    fn ticking_view(&self, cx: &gpui_kit::App) -> Option<Entity<SessionView>> {
        let Some(TabEntry::Session(id)) = self.active_entry() else {
            return None;
        };
        let handle = self.session_handle(&id, cx)?;
        let handle = handle.read(cx);
        let live = matches!(
            handle.status(),
            SessionStatus::Running | SessionStatus::Waiting
        ) || handle.projection().has_running_tools();
        live.then(|| self.active_view().cloned()).flatten()
    }

    /// Shows only running or waiting sessions in the sidebar, or everything.
    pub fn set_running_only(
        &mut self,
        running_only: bool,
        cx: &mut Context<'_, Self>,
    ) {
        self.sidebar.running_only = running_only;
        cx.notify();
    }

    /// Shows an agent notice as a toast on the next frame.
    pub fn show_notice(
        &mut self,
        title: String,
        description: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        self.notices.push((title, description));
        cx.notify();
    }

    /// Current desktop state.
    pub fn state(&self) -> &DesktopState {
        &self.state
    }

    /// Open tabs in order.
    pub fn tabs(&self) -> &[TabEntry] {
        self.tabs.entries()
    }

    /// Index of the active tab.
    pub fn active_tab(&self) -> Option<usize> {
        self.tabs.active()
    }

    /// Connects the shell to a started backend, or shows why it could not start.
    pub fn set_backend(
        &mut self,
        result: Result<
            (AcpClient, UnboundedReceiver<BackendEvent>),
            BackendError,
        >,
        cx: &mut Context<'_, Self>,
    ) {
        match result {
            Ok((client, events)) => {
                let registry = cx.new(|_| SessionRegistry::new(client.clone()));
                let subscription = cx.subscribe(
                    &registry,
                    |this, _, event: &RegistryEvent, cx| {
                        match event {
                            RegistryEvent::BackendStopped(reason) => {
                                this.host.phase =
                                    BackendPhase::Stopped(reason.clone());
                                cx.notify();
                            }
                            // Agent-set titles and fresh modification times live in the catalog.
                            // A settled run may also have changed files in its workspace.
                            RegistryEvent::SessionChanged(id) => {
                                this.refresh_catalog(cx);
                                this.refresh_session_git(id, cx);
                            }
                            RegistryEvent::Notice { title, description } => {
                                this.show_notice(
                                    title.clone(),
                                    description.clone(),
                                    cx,
                                )
                            }
                            RegistryEvent::Released(_) => {}
                        }
                    },
                );
                self._subscriptions.push(subscription);
                let pump =
                    SessionRegistry::pump(registry.downgrade(), events, cx);
                self.host.phase = BackendPhase::Ready;
                self.host.client = Some(client);
                self.host.registry = Some(registry);
                self.host._pump = Some(pump);
                // Panel views built before the backend was ready are rebuilt with it.
                self.reset_panel_views();
                self.refresh_all_git(cx);
                self.refresh_catalog(cx);
            }
            Err(error) => {
                tracing::error!("desktop backend unavailable: {}", error);
                self.host.phase = BackendPhase::Failed(error.to_string());
            }
        }
        cx.notify();
    }

    /// Reloads the session list from the backend.
    pub fn refresh_catalog(&mut self, cx: &mut Context<'_, Self>) {
        let Some(client) = self.host.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| match client.list_sessions().await {
            Ok(sessions) => {
                let catalog = sessions.iter().map(Into::into).collect();
                let _ = this
                    .update(cx, |this, cx| this.replace_catalog(catalog, cx));
            }
            Err(error) => tracing::warn!("failed to list sessions: {}", error),
        })
        .detach();
    }

    /// Installs a new session list: registers newly seen workspaces and drops vanished tabs.
    pub fn replace_catalog(
        &mut self,
        mut catalog: SessionCatalog,
        cx: &mut Context<'_, Self>,
    ) {
        // A listing that started before a session was created can miss it;
        // sessions still attached keep their tab and their known summary.
        for tab in self.tabs.entries() {
            if let TabEntry::Session(id) = tab
                && catalog.get(id).is_none()
                && self.session_handle(id, cx).is_some()
                && let Some(summary) = self.host.catalog.get(id)
            {
                catalog.upsert(summary.clone());
            }
        }
        let before = self.state.workspaces.entries().len();
        self.state.workspaces.merge_discovered(catalog.cwds());
        // A folder deleted while the app runs turns its row "missing" on the next refresh.
        self.refresh_missing();
        if self.state.workspaces.entries().len() != before {
            self.push_workspaces(cx);
        }
        self.tabs.retain(|tab| match tab {
            TabEntry::Session(id) => catalog.get(id).is_some(),
            TabEntry::Draft { .. } => true,
        });
        let tabs = &self.tabs;
        self.views.retain(|tab, _| tabs.position(tab).is_some());
        self.host.catalog = catalog;
        // Deleted sessions take their panels along (M3b spec §6.1).
        self.prune_panels(cx);
        self.attach_active(cx);
        self.persist(cx);
        cx.notify();
    }

    /// Hands every tab view the registered workspaces.
    ///
    /// Views render cached, so they are told about changes instead of
    /// reading the list on each frame.
    fn push_workspaces(&self, cx: &mut Context<'_, Self>) {
        let workspaces: Vec<PathBuf> = self
            .state
            .workspaces
            .entries()
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        for view in self.views.values() {
            view.update(cx, |view, cx| view.set_workspaces(&workspaces, cx));
        }
    }

    /// Paints file icons in the theme the settings chose for the current light or dark mode.
    pub(in crate::ui) fn file_icon_painter(
        &self,
        cx: &gpui_kit::App,
    ) -> file_icon::FileIconPainter {
        use gpui_kit::component::ActiveTheme as _;
        let appearance = &self.state.appearance;
        let theme = if cx.theme().is_dark() {
            appearance.dark_file_icons
        } else {
            appearance.light_file_icons
        };
        file_icon::FileIconPainter::new(
            theme,
            std::rc::Rc::clone(&self.file_icon_cache),
        )
    }

    /// The active tab's workspace: a draft's own, or the session's working
    /// directory once the catalog lists it. A restored session waits for the
    /// catalog rather than borrowing another workspace.
    pub(in crate::ui) fn active_workspace(&self) -> Option<PathBuf> {
        match self.active_entry()? {
            TabEntry::Session(id) => self
                .host
                .catalog
                .get(&id)
                .map(|summary| summary.cwd.clone()),
            TabEntry::Draft { workspace } => workspace,
        }
    }

    /// The current workspace (spec §5.4): the active tab's, else the
    /// sidebar's last clicked one, else the first listed one.
    pub(in crate::ui) fn current_workspace(&self) -> Option<PathBuf> {
        if let Some(workspace) = self.active_workspace() {
            return Some(workspace);
        }
        self.sidebar
            .selected_workspace
            .clone()
            .filter(|path| self.state.workspaces.contains(path))
            .or_else(|| {
                self.state
                    .workspaces
                    .entries()
                    .first()
                    .map(|entry| entry.path.clone())
            })
    }

    /// Mirrors open tabs into the saved state and schedules saving it.
    fn persist(&mut self, cx: &mut Context<'_, Self>) {
        self.state.tabs = self.tabs.persisted();
        self.schedule_save(cx);
    }

    /// `NewSession`: a draft in the current workspace.
    fn on_new_session(
        &mut self,
        _: &NewSession,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let workspace = self.current_workspace();
        self.open_draft(workspace, cx);
    }

    /// `ToggleSidebar`: shows or hides the sidebar.
    fn on_toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.state.layout.sidebar_open = !self.state.layout.sidebar_open;
        self.persist(cx);
        cx.notify();
    }

    /// `FocusSearch`: reveals the sidebar search field and focuses it.
    fn on_focus_search(
        &mut self,
        _: &FocusSearch,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.state.layout.sidebar_open = true;
        self.sidebar.search_open = true;
        self.search
            .update(cx, |search, cx| search.focus(window, cx));
        cx.notify();
    }

    /// `CloseSearch`: hides the search field, drops its query, and hands focus back to the shell.
    fn on_close_search(
        &mut self,
        _: &CloseSearch,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.sidebar.search_open = false;
        self.search
            .update(cx, |search, cx| search.set_value("", window, cx));
        // The focused field is about to disappear; keep the shell's shortcuts reachable.
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }
}

impl Render for AppView {
    /// Lays out the chrome surface with the rail card, sidebar, and the content card.
    fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        self.ensure_active_view(window, cx);
        self.ensure_panel_view(window, cx);
        self.sync_panel_focus(window, cx);
        self.sync_details_inset(window, cx);
        // Notices arrive without a window; the frame that has one shows them.
        for (title, description) in std::mem::take(&mut self.notices) {
            let notification = match description {
                Some(description) => {
                    Notification::warning(description).title(title)
                }
                None => Notification::warning(title),
            };
            window.push_notification(notification, cx);
        }
        let chrome = cx.theme().title_bar;
        let foreground = cx.theme().foreground;
        div()
            .id("workspace")
            .key_context(WORKSPACE_CONTEXT)
            // Interface text inherits the chosen weight from the root.
            .font_weight(crate::theme::FontWeights::get(cx).ui)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_new_session))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_select_tab))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_panel))
            .on_action(cx.listener(Self::on_show_files))
            .on_action(cx.listener(Self::on_show_changes))
            .on_action(cx.listener(Self::on_add_workspace))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_close_search))
            .on_action(cx.listener(Self::on_focus_composer))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_toggle_trajectory))
            .on_action(cx.listener(Self::on_quit))
            .size_full()
            .flex()
            .flex_col()
            .bg(chrome)
            .text_color(foreground)
            // The panel sheet positions itself over the whole window.
            .relative()
            .child(self.render_title_bar(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    // `min_w_0`: the panes must fit the window, not their content's width.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_body(window, cx)),
                    ),
            )
            .children(self.render_panel_sheet(window, cx))
    }
}
