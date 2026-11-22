//! Reading the trajectory: the tail on open, earlier pages on demand,
//! debounced refreshes as the history grows, and request contents by hash.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::{Context, Task};
use protocol::{ContentHash, TrajectoryCursor, TrajectoryPage};
use typed_builder::TypedBuilder;

use super::{ContentState, LoadState, TrajectoryView};
use crate::backend::{AcpClient, BackendError};
use crate::trajectory::RefreshGate;

/// Notifications closer together than this share one refresh.
const REFRESH_DEBOUNCE: Duration = Duration::from_millis(150);

/// The reads of one trajectory and what they left behind.
#[derive(TypedBuilder)]
pub(super) struct Loader {
    /// The agent connection; `None` until the backend is ready.
    #[builder(default)]
    client: Option<AcpClient>,
    /// Whether the first page could be read.
    #[builder(default)]
    pub(super) load: LoadState,
    /// Why the last refresh failed; loaded records stay.
    #[builder(default)]
    pub(super) refresh_error: Option<String>,
    /// Keeps paging back until nothing earlier is left.
    #[builder(default)]
    pub(super) loading_all: bool,
    /// The tail or earlier page being read.
    #[builder(default)]
    _load: Option<Task<()>>,
    /// The wait before a scheduled refresh reads. Kept apart from the read
    /// itself: a read replacing it would drop a scheduled refresh that the
    /// gate still counts as pending, and refreshes would stop for good.
    #[builder(default)]
    _debounce: Option<Task<()>>,
    /// The running read of newer records.
    #[builder(default)]
    _refresh: Option<Task<()>>,
    /// Keeps notifications during a refresh from being lost.
    #[builder(default)]
    gate: RefreshGate,
}

impl Loader {
    /// Nothing read yet; `client` reads once the backend is ready.
    pub(super) fn new(client: Option<AcpClient>) -> Self {
        Self::builder().client(client).build()
    }
}

impl TrajectoryView {
    /// Whether the trajectory could be read.
    pub fn load_state(&self) -> &LoadState {
        &self.loader.load
    }

    /// Takes the client once the backend is ready and reads what was missed.
    pub fn set_client(
        &mut self,
        client: Option<&AcpClient>,
        cx: &mut Context<'_, Self>,
    ) {
        if self.loader.client.is_none() && client.is_some() {
            self.loader.client = client.cloned();
            self.refresh(cx);
        }
    }

    /// Reads what the history database gained, once per burst of
    /// notifications; with nothing loaded, reads the tail.
    pub fn refresh(&mut self, cx: &mut Context<'_, Self>) {
        if !self.loader.gate.request() {
            return;
        }
        self.loader._debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REFRESH_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                let cursor = this
                    .model
                    .last_sequence()
                    .map_or(TrajectoryCursor::Tail, TrajectoryCursor::After);
                // Without a client nothing runs; the next notification retries.
                if this.loader.client.is_some() {
                    this.loader.gate.start();
                    this.fetch(cursor, cx);
                } else {
                    this.loader.gate = RefreshGate::default();
                }
            });
        }));
    }

    /// Reads the tail again after a failure.
    pub fn retry(&mut self, cx: &mut Context<'_, Self>) {
        self.loader.refresh_error = None;
        if self.model.records().is_empty() {
            self.fetch(TrajectoryCursor::Tail, cx);
        } else {
            self.refresh(cx);
        }
        cx.notify();
    }

    /// Reads the page before the loaded records.
    pub fn load_earlier(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(first) = self.model.first_sequence()
            && self.model.has_more_before()
            && self.loader._load.is_none()
        {
            self.fetch(TrajectoryCursor::Before(first), cx);
        }
    }

    /// Pages back until no earlier record is left.
    pub fn load_all(&mut self, cx: &mut Context<'_, Self>) {
        self.loader.loading_all = self.model.has_more_before();
        self.load_earlier(cx);
        cx.notify();
    }

    /// Folds a page read at `cursor` into the ledger: a tail shows its end,
    /// an earlier page keeps the view, and newer records are followed while
    /// the ledger is at its end.
    pub fn apply_page(
        &mut self,
        cursor: TrajectoryCursor,
        page: TrajectoryPage,
        cx: &mut Context<'_, Self>,
    ) {
        self.model.apply(cursor, page);
        self.loader.load = LoadState::Idle;
        match cursor {
            TrajectoryCursor::Tail => {
                self.sync_rows(cx);
                self.scroll_to_latest(cx);
            }
            TrajectoryCursor::Before(_) => self.sync_rows_keeping_view(cx),
            TrajectoryCursor::After(_) => self.sync_following(cx),
        }
    }

    /// The request content `hash`, read the first time it is asked for.
    pub fn request_content(
        &mut self,
        hash: &ContentHash,
        cx: &mut Context<'_, Self>,
    ) -> ContentState {
        if let Some(state) = self.contents.get(hash) {
            return state.clone();
        }
        let Some(client) = self.loader.client.clone() else {
            return ContentState::Loading;
        };
        self.contents.insert(hash.clone(), ContentState::Loading);
        let (id, hash) = (self.session_id(cx), hash.clone());
        cx.spawn(async move |this, cx| {
            let state = match client.request_content(&id, &hash).await {
                Ok(Some(content)) => ContentState::Ready(Arc::new(content)),
                Ok(None) => ContentState::Missing,
                Err(error) => ContentState::Failed(error.to_string()),
            };
            let _ = this.update(cx, |this, cx| {
                this.contents.insert(hash, state);
                cx.notify();
            });
        })
        .detach();
        ContentState::Loading
    }

    /// Reads one page at `cursor`; refreshes and the tail replace a read of
    /// their own kind still running.
    pub(super) fn fetch(
        &mut self,
        cursor: TrajectoryCursor,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(client) = self.loader.client.clone() else {
            return;
        };
        if self.model.records().is_empty()
            && matches!(cursor, TrajectoryCursor::Tail)
            && !matches!(self.loader.load, LoadState::Unavailable)
        {
            self.loader.load = LoadState::Loading;
        }
        let id = self.session_id(cx);
        let task = cx.spawn(async move |this, cx| {
            let result = client.trajectory(&id, cursor).await;
            let _ = this.update(cx, |this, cx| this.loaded(cursor, result, cx));
        });
        if matches!(cursor, TrajectoryCursor::After(_)) {
            self.loader._refresh = Some(task);
        } else {
            self.loader._load = Some(task);
        }
        cx.notify();
    }

    /// Applies a read page, continues a paging that has more to read, or
    /// records why the read failed.
    fn loaded(
        &mut self,
        cursor: TrajectoryCursor,
        result: Result<TrajectoryPage, BackendError>,
        cx: &mut Context<'_, Self>,
    ) {
        match cursor {
            TrajectoryCursor::After(_) => self.loader._refresh = None,
            _ => self.loader._load = None,
        }
        // A refresh's read ended; one announced meanwhile reads again.
        let refreshed = matches!(cursor, TrajectoryCursor::After(_))
            || (matches!(cursor, TrajectoryCursor::Tail)
                && self.loader.gate.running());
        if refreshed && self.loader.gate.finish() {
            self.refresh(cx);
        }
        match result {
            Ok(page) => {
                let more_after = page.has_more_after;
                self.loader.refresh_error = None;
                // The history answers again, so contents it could not read
                // (it may not have been ready) are asked for anew.
                self.contents.retain(|_, state| {
                    !matches!(state, ContentState::Failed(_))
                });
                self.apply_page(cursor, page, cx);
                if more_after && let Some(last) = self.model.last_sequence() {
                    self.fetch(TrajectoryCursor::After(last), cx);
                }
                if self.loader.loading_all {
                    self.loader.loading_all = self.model.has_more_before();
                    self.load_earlier(cx);
                }
            }
            Err(error) => {
                tracing::warn!("could not read the trajectory: {}", error);
                self.loader.loading_all = false;
                if !self.model.records().is_empty() {
                    self.loader.refresh_error = Some(error.to_string());
                } else if matches!(error, BackendError::HistoryUnavailable) {
                    self.loader.load = LoadState::Unavailable;
                } else {
                    self.loader.load = LoadState::Failed(error.to_string());
                }
            }
        }
        cx.notify();
    }
}
