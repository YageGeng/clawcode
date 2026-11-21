//! Loading state shared by the panel's file and changes viewers.

use std::path::PathBuf;

use gpui_kit::component::button::Button;
use gpui_kit::component::label::Label;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement as _,
    Styled as _, Task, Window, div,
};
use typed_builder::TypedBuilder;

use crate::backend::AcpClient;
use crate::ui::session::loading_skeleton;

/// Where a viewer's content stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerState {
    /// The backend has not answered yet.
    Loading,
    /// The content is shown.
    Ready,
    /// Nothing to show, and why (binary, too large, no change).
    Notice(String),
    /// The request failed; carries the error.
    Failed(String),
}

impl ViewerState {
    /// The body for every state but `Ready`; its retry button calls the view's `reload`.
    pub(super) fn render_placeholder<V: 'static>(
        &self,
        reload: fn(&mut V, &mut Window, &mut Context<'_, V>),
        cx: &mut Context<'_, V>,
    ) -> Option<AnyElement> {
        let view = cx.entity().downgrade();
        let retry = move |_: &ClickEvent,
                          window: &mut Window,
                          cx: &mut gpui_kit::App| {
            let _ = view.update(cx, |this, cx| reload(this, window, cx));
        };
        let muted = cx.theme().muted_foreground;
        let centered = || {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .p_8()
        };
        match self {
            ViewerState::Ready => None,
            ViewerState::Loading => Some(loading_skeleton().into_any_element()),
            ViewerState::Notice(text) => Some(
                centered()
                    .child(Label::new(text.clone()).text_color(muted))
                    .into_any_element(),
            ),
            ViewerState::Failed(error) => Some(
                centered()
                    .child(Label::new(error.clone()).text_color(muted))
                    .child(
                        Button::new("viewer-retry")
                            .small()
                            .label(rust_i18n::t!("Viewer.retry").into_owned())
                            .on_click(retry),
                    )
                    .into_any_element(),
            ),
        }
    }
}

/// The file a viewer shows and how far loading it got.
#[derive(TypedBuilder)]
pub struct ViewerSource {
    /// Workspace the file belongs to.
    pub(super) workspace: PathBuf,
    /// Root-relative path of the file.
    #[builder(default)]
    pub(super) path: String,
    /// The agent connection; `None` before the backend is ready.
    pub(super) client: Option<AcpClient>,
    /// Where the content stands.
    #[builder(default = ViewerState::Loading)]
    pub(super) state: ViewerState,
    /// The load in flight; replacing it cancels the previous one.
    #[builder(default)]
    pub(super) _load: Option<Task<()>>,
}

impl ViewerSource {
    /// Starts a reload: the client, workspace, and path to request with, or
    /// `None` after recording that there is no backend. Content already
    /// `shown` stays on screen instead of the loading state.
    pub(super) fn begin(
        &mut self,
        shown: bool,
    ) -> Option<(AcpClient, PathBuf, String)> {
        let Some(client) = self.client.clone() else {
            self.state = ViewerState::Failed(
                rust_i18n::t!("Viewer.no_backend").into_owned(),
            );
            return None;
        };
        if !shown {
            self.state = ViewerState::Loading;
        }
        Some((client, self.workspace.clone(), self.path.clone()))
    }

    /// The state of a request that failed while doing `what`, logged with the file.
    pub(super) fn failed(
        &self,
        what: &str,
        error: impl std::fmt::Display,
    ) -> ViewerState {
        tracing::warn!(
            "failed to {} {} in {}: {}",
            what,
            self.path,
            self.workspace.display(),
            error
        );
        ViewerState::Failed(error.to_string())
    }
}
