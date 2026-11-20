//! The workspace shell: chrome (title bar, sidebar) holding two inset cards, the rail and the content card.

mod card;
mod composer;
mod dialogs;
mod drafts;
mod lifecycle;
mod rail;
mod rows;
mod session_view;
mod settings;
mod sidebar;
mod status;
mod title_bar;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use futures::channel::mpsc::UnboundedReceiver;
use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::AppMenuBar;
use gpui_kit::component::{ActiveTheme as _, ResizableState};
use gpui_kit::{
    AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    Styled as _, Subscription, Task, Window, div,
};
use typed_builder::TypedBuilder;

pub use composer::{Composer, ComposerEvent};
pub use session_view::{
    MarkdownCache, MarkdownPart, SessionTarget, SessionView, SessionViewEvent,
};
pub use sidebar::{
    GroupKind, RECENT_SESSIONS, SESSIONS_PER_WORKSPACE, SidebarGroupModel,
    SidebarModel, SidebarRow,
};
pub use status::StatusIcon;

use crate::actions::{
    CloseSearch, CloseTab, FocusSearch, NewSession, NextTab, SelectTab,
    ToggleSidebar, WORKSPACE_CONTEXT,
};
use crate::backend::{AcpClient, BackendError, BackendEvent, SessionId};
use crate::catalog::{SessionCatalog, SessionSort};
use crate::i18n::LocaleGeneration;
use crate::session::{
    RegistryEvent, SessionHandle, SessionRegistry, SessionStatus,
};
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
    /// The singleton settings page; it has no session view and is never persisted.
    Settings,
}

impl TabEntry {
    /// Stable identity of the tab, independent of its position in the strip.
    pub fn key(&self) -> SharedString {
        match self {
            TabEntry::Session(id) => format!("session:{id}").into(),
            TabEntry::Settings => SharedString::new_static("settings"),
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
    tabs: Vec<TabEntry>,
    #[builder(default)]
    active_tab: Option<usize>,
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
    search: Entity<InputState>,
    #[builder(default)]
    search_open: bool,
    /// Workspaces whose full session list is shown.
    #[builder(default)]
    show_all: HashSet<PathBuf>,
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
    /// Which timestamp orders the sidebar lists; not persisted.
    #[builder(default)]
    sort: SessionSort,
    /// Sidebar shows only running or waiting sessions; not persisted.
    #[builder(default)]
    running_only: bool,
    /// Redraws the shell while a run is active and refreshes relative ages every minute.
    #[builder(default)]
    _ticker: Option<Task<()>>,
    /// Set once quitting started, so a second request does nothing.
    #[builder(default)]
    quitting: bool,
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
                    title_bar::install_menus(&this.menu_bar, cx);
                },
            ),
        ];
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let tabs = state
            .tabs
            .open
            .iter()
            .map(|id| TabEntry::Session(SessionId::from(id.clone())))
            .collect::<Vec<_>>();
        let active_tab = state.tabs.active.filter(|index| *index < tabs.len());
        let mut view = AppView::builder()
            .state(state)
            .state_path(state_path)
            .host(HostState::builder().build())
            .tabs(tabs)
            .active_tab(active_tab)
            .search(search)
            .sidebar_split(cx.new(|_| ResizableState::default()))
            .menu_bar(AppMenuBar::new(cx))
            .focus_handle(focus_handle)
            ._subscriptions(subscriptions)
            .build();
        view.refresh_missing();
        title_bar::install_menus(&view.menu_bar, cx);
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
        self.running_only = running_only;
        cx.notify();
    }

    /// Current desktop state.
    pub fn state(&self) -> &DesktopState {
        &self.state
    }

    /// Open tabs in order.
    pub fn tabs(&self) -> &[TabEntry] {
        &self.tabs
    }

    /// Index of the active tab.
    pub fn active_tab(&self) -> Option<usize> {
        self.active_tab
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
                            RegistryEvent::SessionChanged(_) => {
                                this.refresh_catalog(cx)
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
        for tab in &self.tabs {
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
        if self.state.workspaces.entries().len() != before {
            self.refresh_missing();
            self.push_workspaces(cx);
        }
        let active = self.active_entry();
        self.tabs.retain(|tab| match tab {
            TabEntry::Session(id) => catalog.get(id).is_some(),
            TabEntry::Draft { .. } | TabEntry::Settings => true,
        });
        let tabs = &self.tabs;
        self.views.retain(|tab, _| tabs.contains(tab));
        self.active_tab = active
            .and_then(|active| self.tabs.iter().position(|tab| tab == &active))
            .or_else(|| (!self.tabs.is_empty()).then_some(0));
        self.host.catalog = catalog;
        self.attach_active(cx);
        self.persist(cx);
        cx.notify();
    }

    /// Activates a session's tab, opening one if needed.
    pub fn open_session(&mut self, id: &SessionId, cx: &mut Context<'_, Self>) {
        let entry = TabEntry::Session(id.clone());
        let index = match self.tabs.iter().position(|tab| tab == &entry) {
            Some(index) => index,
            None => {
                self.tabs.push(entry);
                self.tabs.len() - 1
            }
        };
        self.activate(index, cx);
    }

    /// Opens a draft tab for a new session in `workspace`, reusing an existing draft for it.
    pub fn open_draft(
        &mut self,
        workspace: Option<PathBuf>,
        cx: &mut Context<'_, Self>,
    ) {
        let entry = TabEntry::Draft { workspace };
        let index = match self.tabs.iter().position(|tab| tab == &entry) {
            Some(index) => index,
            None => {
                self.tabs.push(entry);
                self.tabs.len() - 1
            }
        };
        self.activate(index, cx);
    }

    /// Closes a tab; a session tab releases its session, which closes it once idle.
    pub fn close_tab(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        if index >= self.tabs.len() {
            return;
        }
        let closed = self.tabs.remove(index);
        self.views.remove(&closed);
        if let (TabEntry::Session(id), Some(registry)) =
            (&closed, &self.host.registry)
        {
            registry.update(cx, |registry, cx| registry.release(id, cx));
        }
        self.active_tab = match self.active_tab {
            _ if self.tabs.is_empty() => None,
            // The right neighbor slides into the closed slot; at the end, take the left one.
            Some(active) if active == index => {
                Some(index.min(self.tabs.len() - 1))
            }
            Some(active) if active > index => Some(active - 1),
            other => other,
        };
        if let Some(active) = self.active_tab {
            self.tab_scroll.scroll_to_item(active);
        }
        self.attach_active(cx);
        self.persist(cx);
        cx.notify();
    }

    /// Current position of a tab.
    fn tab_index(&self, entry: &TabEntry) -> Option<usize> {
        self.tabs.iter().position(|tab| tab == entry)
    }

    /// Closes this tab wherever it is now; nothing happens when it is already gone.
    pub fn close_entry(
        &mut self,
        entry: &TabEntry,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(index) = self.tab_index(entry) {
            self.close_tab(index, cx);
        }
    }

    /// Moves the tab at `from` to position `to`; the active entry stays active.
    pub fn move_tab(
        &mut self,
        from: usize,
        to: usize,
        cx: &mut Context<'_, Self>,
    ) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let active = self.active_entry();
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        // Entries are unique, so the active one is found again by value.
        self.active_tab = active
            .and_then(|active| self.tabs.iter().position(|tab| *tab == active));
        if let Some(active) = self.active_tab {
            self.tab_scroll.scroll_to_item(active);
        }
        self.persist(cx);
        cx.notify();
    }

    /// Makes a tab active and attaches its session if that has not happened yet.
    fn activate(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active_tab = Some(index);
        self.tab_scroll.scroll_to_item(index);
        self.attach_active(cx);
        self.persist(cx);
        cx.notify();
    }

    /// The active tab entry.
    fn active_entry(&self) -> Option<TabEntry> {
        self.active_tab
            .and_then(|index| self.tabs.get(index))
            .cloned()
    }

    /// The attached session with this id, if any.
    pub fn session_handle(
        &self,
        id: &SessionId,
        cx: &gpui_kit::App,
    ) -> Option<Entity<SessionHandle>> {
        self.host
            .registry
            .as_ref()
            .and_then(|registry| registry.read(cx).handle(id))
    }

    /// Creates the active tab's view on first show, and points it at the
    /// session's current handle (a reattach or a backend restart replaces it).
    ///
    /// Runs at render time because views need the `Window` to build their inputs.
    fn ensure_active_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(entry) = self.active_entry() else {
            return;
        };
        let target = match &entry {
            TabEntry::Draft { workspace } => SessionTarget::Draft {
                workspace: workspace.clone(),
            },
            // The settings page renders straight from the shell state.
            TabEntry::Settings => return,
            TabEntry::Session(id) => match self.session_handle(id, cx) {
                Some(handle) => SessionTarget::Session(handle),
                // Not attached yet: the card shows its loading state.
                None => return,
            },
        };
        let view = match self.views.get(&entry) {
            Some(view) => {
                let stale = match &target {
                    SessionTarget::Session(handle) => {
                        view.read(cx).handle() != Some(handle)
                    }
                    SessionTarget::Draft { .. } => false,
                };
                if stale {
                    // The view renders cached and a notify made while drawing
                    // schedules no frame, so retarget right after this one.
                    let view = view.clone();
                    cx.defer_in(window, move |_, window, cx| {
                        view.update(cx, |view, cx| {
                            view.set_target(target, window, cx)
                        });
                    });
                }
                view.clone()
            }
            None => self.create_view(entry, target, window, cx),
        };
        let client = self.host.client.as_ref();
        view.update(cx, |view, _| view.set_client(client));
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

    /// Attaches the active session tab; restored tabs attach only when they become active.
    fn attach_active(&mut self, cx: &mut Context<'_, Self>) {
        let Some(TabEntry::Session(id)) = self.active_entry() else {
            return;
        };
        let (Some(registry), Some(summary)) =
            (&self.host.registry, self.host.catalog.get(&id))
        else {
            return;
        };
        let summary = summary.clone();
        registry.update(cx, |registry, cx| {
            registry.attach(&summary, cx);
        });
    }

    /// The workspace new sessions start in: the active tab's, else the first listed one.
    fn current_workspace(&self) -> Option<PathBuf> {
        match self.active_entry() {
            Some(TabEntry::Session(id)) => {
                if let Some(summary) = self.host.catalog.get(&id) {
                    return Some(summary.cwd.clone());
                }
            }
            Some(TabEntry::Draft {
                workspace: Some(workspace),
            }) => return Some(workspace),
            _ => {}
        }
        self.state
            .workspaces
            .entries()
            .first()
            .map(|entry| entry.path.clone())
    }

    /// Mirrors open tabs into the saved state and schedules saving it.
    fn persist(&mut self, cx: &mut Context<'_, Self>) {
        let sessions: Vec<(usize, String)> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(index, tab)| match tab {
                TabEntry::Session(id) => Some((index, id.to_string())),
                TabEntry::Draft { .. } | TabEntry::Settings => None,
            })
            .collect();
        self.state.tabs.active = self.active_tab.and_then(|active| {
            sessions.iter().position(|(index, _)| *index == active)
        });
        self.state.tabs.open = sessions.into_iter().map(|(_, id)| id).collect();
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

    /// `CloseTab`: closes the active tab.
    fn on_close_tab(
        &mut self,
        _: &CloseTab,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(index) = self.active_tab {
            self.close_tab(index, cx);
        }
    }

    /// `NextTab`: cycles forward through the tabs.
    fn on_next_tab(
        &mut self,
        _: &NextTab,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.tabs.is_empty() {
            return;
        }
        let next = self
            .active_tab
            .map_or(0, |active| (active + 1) % self.tabs.len());
        self.activate(next, cx);
    }

    /// `SelectTab`: jumps to a tab by position.
    fn on_select_tab(
        &mut self,
        action: &SelectTab,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.activate(action.index, cx);
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
        self.search_open = true;
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
        self.search_open = false;
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
        let chrome = cx.theme().title_bar;
        let foreground = cx.theme().foreground;
        div()
            .id("workspace")
            .key_context(WORKSPACE_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_new_session))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_select_tab))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_close_search))
            .on_action(cx.listener(Self::on_focus_composer))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_quit))
            .size_full()
            .flex()
            .flex_col()
            .bg(chrome)
            .text_color(foreground)
            .child(self.render_title_bar(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_rail(cx))
                    // `min_w_0`: the panes must fit the window, not their content's width.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_body(window, cx)),
                    ),
            )
    }
}
