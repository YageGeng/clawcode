//! The session list as the sidebar sees it: summaries grouped by workspace, recency, and search.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use agent_client_protocol::v2::schema as wire;
use protocol::ProductIdentity;
use typed_builder::TypedBuilder;

use crate::backend::SessionId;
use crate::workspace::normalize;

/// One listed session, decoded from `session/list`.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub struct SessionSummary {
    /// Session identifier.
    pub session_id: SessionId,
    /// Lexically normalized working directory; the workspace it belongs to.
    pub cwd: PathBuf,
    /// Trimmed title, `None` when missing or blank.
    #[builder(default)]
    pub title: Option<String>,
    /// Creation time in Unix milliseconds.
    #[builder(default)]
    pub created_at_ms: Option<u64>,
    /// Last change in Unix milliseconds; drives every ordering.
    #[builder(default)]
    pub modified_at_ms: Option<u64>,
    /// Parent session for child agents; children are hidden from the lists.
    #[builder(default)]
    pub parent_session_id: Option<SessionId>,
}

impl From<&wire::SessionInfo> for SessionSummary {
    /// Reads the standard fields and the product metadata; malformed values become `None`.
    fn from(info: &wire::SessionInfo) -> Self {
        let product = info
            .meta
            .as_ref()
            .and_then(|meta| meta.get(ProductIdentity::ACP_NAMESPACE));
        // Timestamps travel as decimal strings so JavaScript clients keep full precision.
        let millis = |key: &str| {
            product
                .and_then(|product| product.get(key))
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<u64>().ok())
        };
        SessionSummary::builder()
            .session_id(info.session_id.clone())
            .cwd(normalize(info.cwd.as_ref()))
            .title(
                info.title
                    .as_deref()
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .map(str::to_owned),
            )
            .created_at_ms(millis("createdAtMs"))
            .modified_at_ms(millis("modifiedAtMs"))
            .parent_session_id(
                product
                    .and_then(|product| product.get("parentSessionId"))
                    .and_then(serde_json::Value::as_str)
                    .map(|id| SessionId::from(id.to_string())),
            )
            .build()
    }
}

impl SessionSummary {
    /// Whether this is a user-facing session rather than a child agent.
    fn is_top_level(&self) -> bool {
        self.parent_session_id.is_none()
    }

    /// Orders by the `sort` timestamp, newest first; sessions without one go last, ties by id.
    fn ordered_by(
        &self,
        other: &SessionSummary,
        sort: SessionSort,
    ) -> Ordering {
        let key = |session: &SessionSummary| match sort {
            SessionSort::Modified => session.modified_at_ms,
            SessionSort::Created => session.created_at_ms,
        };
        match (key(self), key(other)) {
            (Some(left), Some(right)) => right.cmp(&left),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
        // Compare the ids' text in place; formatting them would allocate per comparison.
        .then_with(|| self.session_id.0.cmp(&other.session_id.0))
    }
}

/// Which timestamp orders a session list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionSort {
    /// Last change, newest first.
    #[default]
    Modified,
    /// Creation time, newest first.
    Created,
}

/// All listed sessions with the queries the sidebar needs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionCatalog {
    sessions: Vec<SessionSummary>,
}

impl FromIterator<SessionSummary> for SessionCatalog {
    /// Collects summaries in any order; queries sort on demand.
    fn from_iter<I: IntoIterator<Item = SessionSummary>>(iter: I) -> Self {
        Self {
            sessions: iter.into_iter().collect(),
        }
    }
}

impl SessionCatalog {
    /// Top-level sessions ordered by `sort`, filtered by `keep`.
    fn top_level(
        &self,
        sort: SessionSort,
        keep: impl Fn(&SessionSummary) -> bool,
    ) -> Vec<&SessionSummary> {
        let mut sessions: Vec<_> = self
            .sessions
            .iter()
            .filter(|session| session.is_top_level() && keep(session))
            .collect();
        sessions.sort_by(|left, right| left.ordered_by(right, sort));
        sessions
    }

    /// Top-level sessions whose working directory is `workspace`, newest first.
    pub fn for_workspace(
        &self,
        workspace: &Path,
        sort: SessionSort,
    ) -> Vec<&SessionSummary> {
        let workspace = normalize(workspace);
        self.top_level(sort, |session| session.cwd == workspace)
    }

    /// The newest top-level sessions across every workspace.
    pub fn recent(
        &self,
        limit: usize,
        sort: SessionSort,
    ) -> Vec<&SessionSummary> {
        let mut sessions = self.top_level(sort, |_| true);
        sessions.truncate(limit);
        sessions
    }

    /// Top-level sessions whose title or path contains `query`, ignoring case.
    pub fn search(&self, query: &str) -> Vec<&SessionSummary> {
        let query = query.trim().to_lowercase();
        self.top_level(SessionSort::Modified, |session| {
            query.is_empty()
                || session
                    .title
                    .as_deref()
                    .is_some_and(|title| title.to_lowercase().contains(&query))
                || session
                    .cwd
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&query)
        })
    }

    /// Distinct working directories of top-level sessions, most recently used first.
    pub fn cwds(&self) -> Vec<&Path> {
        let mut cwds: Vec<&Path> = Vec::new();
        for session in self.top_level(SessionSort::Modified, |_| true) {
            if !cwds.contains(&session.cwd.as_path()) {
                cwds.push(&session.cwd);
            }
        }
        cwds
    }

    /// Looks one session up by id.
    pub fn get(&self, id: &SessionId) -> Option<&SessionSummary> {
        self.sessions
            .iter()
            .find(|session| &session.session_id == id)
    }

    /// Inserts a session or replaces the one with the same id.
    pub fn upsert(&mut self, summary: SessionSummary) {
        match self
            .sessions
            .iter_mut()
            .find(|session| session.session_id == summary.session_id)
        {
            Some(existing) => *existing = summary,
            None => self.sessions.push(summary),
        }
    }

    /// Drops a session from the list.
    pub fn remove(&mut self, id: &SessionId) {
        self.sessions.retain(|session| &session.session_id != id);
    }
}

/// How long ago something happened, in the coarse buckets the sidebar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelativeAge {
    /// Under a minute, or in the future.
    Now,
    /// Whole minutes under an hour.
    Minutes(u64),
    /// Whole hours under a day.
    Hours(u64),
    /// Whole days under a week.
    Days(u64),
    /// Whole weeks.
    Weeks(u64),
}

impl RelativeAge {
    /// Buckets the time from `then_ms` to `now_ms`; a future timestamp counts as now.
    pub fn between(now_ms: u64, then_ms: u64) -> Self {
        const MINUTE: u64 = 60_000;
        const HOUR: u64 = 60 * MINUTE;
        const DAY: u64 = 24 * HOUR;
        const WEEK: u64 = 7 * DAY;
        let elapsed = now_ms.saturating_sub(then_ms);
        match elapsed {
            _ if elapsed < MINUTE => RelativeAge::Now,
            _ if elapsed < HOUR => RelativeAge::Minutes(elapsed / MINUTE),
            _ if elapsed < DAY => RelativeAge::Hours(elapsed / HOUR),
            _ if elapsed < WEEK => RelativeAge::Days(elapsed / DAY),
            _ => RelativeAge::Weeks(elapsed / WEEK),
        }
    }
}
