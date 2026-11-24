//! Headless shell windows and sidebar helpers shared by the shell test binaries.

use std::path::{Path, PathBuf};

use agent_client_protocol::v2::schema as wire;
use desktop::backend::SessionId;
use desktop::catalog::SessionSummary;
use desktop::projection::{Role, TranscriptRow};
use desktop::session::{AttachPhase, SessionHandle, SessionStatus};
use desktop::state::DesktopState;
use desktop::ui::{AppView, SessionRow, SidebarModel, SidebarRow};
use futures::channel::mpsc::unbounded;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Entity, TestAppContext,
    Window, WindowBounds, WindowOptions, px, size,
};

use gpui_kit::test::TestWindowExt as _;
use typed_builder::TypedBuilder;

use super::ScriptedBackend;
pub use super::eventually;

/// Opens the production shell in a headless 1280×820 window.
pub fn open(
    cx: &mut TestAppContext,
    state: DesktopState,
) -> (AnyWindowHandle, Entity<AppView>, tempfile::TempDir) {
    cx.update(gpui_kit::init);
    cx.update(desktop::theme::register_builtin_themes);
    cx.update(desktop::actions::bind_keys);
    let dir = tempfile::tempdir().expect("state dir");
    let path = dir.path().join("desktop.json");
    let (handle, view) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(1280.), px(820.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| AppView::new(state, path, window, cx)),
        )
        .expect("open window")
    });
    (handle, view, dir)
}

/// Workspace state with these directories registered and expanded.
pub fn with_workspaces(paths: &[&str]) -> DesktopState {
    let mut state = DesktopState::default();
    for path in paths {
        state.workspaces.add(Path::new(path));
    }
    state
}

/// A top-level session summary.
pub fn session(
    id: &str,
    cwd: &str,
    title: Option<&str>,
    modified: u64,
) -> SessionSummary {
    SessionSummary::builder()
        .session_id(wire::SessionId::from(id.to_string()))
        .cwd(PathBuf::from(cwd))
        .title(title.map(str::to_owned))
        .modified_at_ms(Some(modified))
        .build()
}

/// Visible text painted in the last frame, in paint order.
pub fn painted(
    window: &Window,
) -> Vec<gpui_kit::base::test_support::TextSnapshot> {
    gpui_kit::base::test_support::texts(window)
        .into_iter()
        .filter(|text| text.visible())
        .collect()
}

/// Visible text painted inside the element with id `id`, left to right.
pub fn row_texts(window: &Window, id: &str) -> Vec<String> {
    let mut texts: Vec<_> = painted(window)
        .into_iter()
        .filter(|text| {
            text.path().iter().any(|element| {
                matches!(element, gpui_kit::ElementId::Name(name) if name.as_ref() == id)
            })
        })
        .collect();
    texts.sort_by(|a, b| {
        f32::from(a.bounds().origin.x)
            .total_cmp(&f32::from(b.bounds().origin.x))
    });
    texts
        .into_iter()
        .map(|text| text.text().to_owned())
        .collect()
}

/// Whether some visible text reads exactly `text`.
pub fn shows(window: &Window, text: &str) -> bool {
    painted(window).iter().any(|painted| painted.text() == text)
}

/// The sidebar model the shell renders right now.
pub fn model(cx: &mut TestAppContext, view: &Entity<AppView>) -> SidebarModel {
    cx.update(|cx| view.read(cx).sidebar_model(cx))
}

/// Session titles of a list of rows, in order.
pub fn titles(rows: &[SidebarRow]) -> Vec<String> {
    rows.iter()
        .filter_map(|row| match row {
            SidebarRow::Session(SessionRow { title, .. }) => {
                Some(title.to_string())
            }
            _ => None,
        })
        .collect()
}

/// Session id from a string.
pub fn id(value: &str) -> wire::SessionId {
    wire::SessionId::from(value.to_string())
}

/// Clicks the element with this accessible label at its center through native pointer events.
pub fn click_label(window: &mut Window, label: &str, cx: &mut gpui_kit::App) {
    use gpui_kit::test::{TestEventExt as _, TestQueryExt as _};
    let center = window.find_by_label(label).bounds().center();
    window.pointer_move(center, None, cx);
    window.pointer_down(center, gpui_kit::MouseButton::Left, cx);
    window.pointer_up(center, gpui_kit::MouseButton::Left, cx);
}

/// A production shell connected to a scripted backend.
#[derive(TypedBuilder)]
pub struct Live {
    pub handle: AnyWindowHandle,
    pub view: Entity<AppView>,
    pub backend: ScriptedBackend,
    pub _dir: tempfile::TempDir,
}

/// Hands the backend's event stream and client to the shell.
pub fn connect(
    cx: &mut TestAppContext,
    view: &Entity<AppView>,
    backend: &mut ScriptedBackend,
) {
    cx.executor().allow_parking();
    let events = std::mem::replace(&mut backend.events, unbounded().1);
    let client = backend.client.clone();
    view.update(cx, |view, cx| view.set_backend(Ok((client, events)), cx));
}

/// Desktop state with the backend's fixture workspace registered.
pub fn fixture_state(backend: &ScriptedBackend) -> DesktopState {
    let workspace = backend.workspace.path().to_str().expect("utf8");
    with_workspaces(&[workspace])
}

/// Starts a scripted backend and hands its event stream to a freshly opened shell.
pub fn live(cx: &mut TestAppContext, state: DesktopState) -> Live {
    live_on(cx, state, ScriptedBackend::start())
}

/// Like [`live`], with a history database behind the backend.
pub fn live_with_history(cx: &mut TestAppContext, state: DesktopState) -> Live {
    live_on(cx, state, ScriptedBackend::start_with_history())
}

/// Like [`live`], with `login` in the catalog beside the scripted model.
pub fn live_with_login(
    cx: &mut TestAppContext,
    state: DesktopState,
    login: &std::sync::Arc<super::login::LoginModel>,
) -> Live {
    live_on(cx, state, ScriptedBackend::start_with_login(login))
}

/// Hands `backend`'s event stream to a freshly opened shell.
fn live_on(
    cx: &mut TestAppContext,
    state: DesktopState,
    mut backend: ScriptedBackend,
) -> Live {
    let (handle, view, dir) = open(cx, state);
    connect(cx, &view, &mut backend);
    Live::builder()
        .handle(handle)
        .view(view)
        .backend(backend)
        ._dir(dir)
        .build()
}

/// Runs one git command in `dir` with a fixed identity, failing the test if it fails.
pub fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?}");
}

/// A live shell whose fixture workspace is a Git repository on `main` with
/// one commit holding `a.rs` (`x`) and `notes.txt` (`alpha`).
pub fn live_in_git_repo(cx: &mut TestAppContext) -> Live {
    let live = live(cx, DesktopState::default());
    let dir = live.backend.workspace.path();
    std::fs::write(dir.join("a.rs"), "x\n").expect("a.rs");
    std::fs::write(dir.join("notes.txt"), "alpha\n").expect("notes.txt");
    git(dir, &["init", "-q"]);
    git(dir, &["add", "a.rs", "notes.txt"]);
    git(dir, &["commit", "-q", "-m", "first"]);
    live
}

impl Live {
    /// Registers `path` as a workspace and makes it the sidebar's current one.
    pub fn add_workspace(&self, cx: &mut TestAppContext, path: &Path) {
        self.view
            .update(cx, |view, cx| view.register_workspace(path, cx));
    }

    /// Creates a session in the fixture workspace, opens its tab, and waits until it is attached.
    pub fn open_new_session(&self, cx: &mut TestAppContext) -> SessionId {
        let id = self.backend.new_session();
        self.open(cx, &id);
        id
    }

    /// Refreshes the session list, opens the session's tab, and waits until it is attached.
    pub fn open(&self, cx: &mut TestAppContext, id: &SessionId) {
        self.view.update(cx, |view, cx| {
            view.refresh_catalog(cx);
            view.open_session(id, cx);
        });
        eventually(cx, "session attached", |cx| {
            self.session(id, cx).is_some_and(|handle| {
                handle.read(cx).phase() == &AttachPhase::Attached
            })
        });
    }

    /// The attached session, if any.
    pub fn session(
        &self,
        id: &SessionId,
        cx: &App,
    ) -> Option<Entity<SessionHandle>> {
        self.view.read(cx).session_handle(id, cx)
    }

    /// Sends a prompt to an idle session and waits until its run settles with a reply.
    pub fn run(
        &self,
        cx: &mut TestAppContext,
        id: &SessionId,
        blocks: Vec<wire::ContentBlock>,
    ) {
        self.backend
            .block_on(self.backend.client.prompt(id, blocks))
            .expect("prompt");
        eventually(cx, "run settled", |cx| {
            self.view.read(cx).session_status(id, cx) == SessionStatus::Idle
                && self.session(id, cx).is_some_and(|handle| {
                    let projection = handle.read(cx).projection();
                    matches!(
                        projection.row(projection.len().saturating_sub(1)),
                        Some(TranscriptRow::Message(message))
                            if message.role == Role::Assistant && !message.streaming
                    )
                })
        });
    }

    /// Message id of the first user message with this text.
    pub fn user_message(
        &self,
        cx: &mut TestAppContext,
        id: &SessionId,
        text: &str,
    ) -> wire::MessageId {
        cx.update(|cx| {
            let handle = self.session(id, cx).expect("attached session");
            let projection = handle.read(cx).projection();
            (0..projection.len())
                .find_map(|index| match projection.row(index) {
                    Some(TranscriptRow::Message(message))
                        if message.role == Role::User
                            && message.text == text =>
                    {
                        Some(message.message_id.clone())
                    }
                    _ => None,
                })
                .expect("user message")
        })
    }

    /// Renders a frame and runs `f` against the window.
    pub fn window<R>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut Window, &mut App) -> R,
    ) -> R {
        cx.run_until_parked();
        cx.update_window(self.handle, |_, window, cx| {
            window.render_frame(cx);
            f(window, cx)
        })
        .expect("window")
    }

    /// Polls until a freshly rendered frame satisfies `check`.
    pub fn until(
        &self,
        cx: &mut TestAppContext,
        what: &str,
        check: impl Fn(&mut Window) -> bool,
    ) {
        let deadline = std::time::Instant::now() + super::WAIT;
        while !self.window(cx, |window, _| check(window)) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}; painted: {:?}",
                self.window(cx, |window, _| painted(window)
                    .iter()
                    .map(|text| text.text().to_string())
                    .collect::<Vec<_>>())
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
