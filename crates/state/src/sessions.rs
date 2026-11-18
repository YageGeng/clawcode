//! Session metadata queries over the state database (Codex `threads` listing).

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use protocol::{
    AgentName, SessionGitInfo, SessionId, SessionOwnership, SessionSummary,
    TimestampMs,
};
use sqlx::{QueryBuilder, Sqlite};

use crate::error::SqlText;
use crate::{StateError, StateRuntime};

/// Children of one owner, served by `idx_sessions_owner`.
const CHILDREN_SQL: &str =
    "SELECT * FROM sessions WHERE owner_session_id = ? ORDER BY agent_name";

/// One `sessions` row with typed values.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct SessionRow {
    /// Stable Session identifier.
    pub id: SessionId,
    /// Canonical JSONL log.
    pub path: PathBuf,
    /// Working directory recorded at creation.
    pub cwd: PathBuf,
    /// Creation time from the header.
    pub created_at_ms: TimestampMs,
    /// Time of the last timestamped mutation.
    pub updated_at_ms: TimestampMs,
    /// Time of the last user message, or creation.
    pub recency_at_ms: TimestampMs,
    /// Latest name fact.
    #[builder(default)]
    pub name: Option<String>,
    /// First user message text, at most 200 characters.
    pub preview: String,
    /// Whether any user message was recorded.
    pub has_user_message: bool,
    /// Latest provider.
    #[builder(default)]
    pub provider_id: Option<String>,
    /// Latest model.
    #[builder(default)]
    pub model_id: Option<String>,
    /// Latest thinking level.
    #[builder(default)]
    pub thinking_level: Option<String>,
    /// Latest Session mode.
    pub session_mode: String,
    /// Sum of reported assistant tokens.
    pub tokens_used: u64,
    /// Fork source.
    #[builder(default)]
    pub parent_session_id: Option<SessionId>,
    /// Owner edge of a delegated Session.
    #[builder(default)]
    pub ownership: Option<SessionOwnership>,
    /// Git state captured at creation.
    #[builder(default)]
    pub git: Option<SessionGitInfo>,
    /// Product version that created the Session.
    #[builder(default)]
    pub app_version: Option<String>,
}

/// Raw `sessions` columns as SQLite returns them.
#[derive(sqlx::FromRow)]
pub(crate) struct SessionColumns {
    id: String,
    path: String,
    cwd: String,
    created_at_ms: u64,
    updated_at_ms: u64,
    recency_at_ms: u64,
    name: Option<String>,
    preview: String,
    has_user_message: bool,
    provider_id: Option<String>,
    model_id: Option<String>,
    thinking_level: Option<String>,
    session_mode: String,
    tokens_used: u64,
    parent_session_id: Option<String>,
    owner_session_id: Option<String>,
    agent_name: Option<String>,
    agent_nickname: Option<String>,
    git_sha: Option<String>,
    git_branch: Option<String>,
    git_origin_url: Option<String>,
    app_version: Option<String>,
}

impl TryFrom<SessionColumns> for SessionRow {
    type Error = StateError;

    /// Validates identifiers and regroups ownership and git columns.
    fn try_from(columns: SessionColumns) -> Result<Self, Self::Error> {
        let ownership = match (columns.owner_session_id, columns.agent_name) {
            (Some(owner), Some(agent)) => Some(SessionOwnership {
                session_id: SessionId::try_from(owner)?,
                name: AgentName::try_from(agent)
                    .map_err(StateError::AgentName)?,
                nickname: columns.agent_nickname.unwrap_or_default(),
            }),
            _ => None,
        };
        let git = SessionGitInfo {
            sha: columns.git_sha,
            branch: columns.git_branch,
            origin_url: columns.git_origin_url,
        };
        Ok(Self::builder()
            .id(SessionId::try_from(columns.id)?)
            .path(columns.path.into())
            .cwd(columns.cwd.into())
            .created_at_ms(columns.created_at_ms.into())
            .updated_at_ms(columns.updated_at_ms.into())
            .recency_at_ms(columns.recency_at_ms.into())
            .name(columns.name)
            .preview(columns.preview)
            .has_user_message(columns.has_user_message)
            .provider_id(columns.provider_id)
            .model_id(columns.model_id)
            .thinking_level(columns.thinking_level)
            .session_mode(columns.session_mode)
            .tokens_used(columns.tokens_used)
            .parent_session_id(
                columns
                    .parent_session_id
                    .map(SessionId::try_from)
                    .transpose()?,
            )
            .ownership(ownership)
            .git((git != SessionGitInfo::default()).then_some(git))
            .app_version(columns.app_version)
            .build())
    }
}

impl From<SessionRow> for SessionSummary {
    /// Keeps the ACP listing shape; `modified_at_ms` carries `updated_at_ms`.
    fn from(row: SessionRow) -> Self {
        Self::builder()
            .session_id(row.id)
            .cwd(row.cwd)
            .created_at_ms(row.created_at_ms)
            .modified_at_ms(row.updated_at_ms)
            .parent_session_id(row.parent_session_id)
            .name(row.name)
            .ownership(row.ownership)
            .build()
    }
}

impl From<SessionRow> for store::SessionHeader {
    /// Exposes the identity a store needs to open or delete the log.
    fn from(row: SessionRow) -> Self {
        Self::builder()
            .id(row.id)
            .cwd(row.cwd)
            .ownership(row.ownership)
            .path(row.path)
            .build()
    }
}

/// Timestamp that orders a listing (Codex `SortKey`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortKey {
    /// Creation time.
    CreatedAt,
    /// Last timestamped mutation.
    #[default]
    UpdatedAt,
    /// Last user message.
    RecencyAt,
}

impl SortKey {
    /// Returns the column this key sorts by.
    fn column(self) -> &'static str {
        match self {
            Self::CreatedAt => "created_at_ms",
            Self::UpdatedAt => "updated_at_ms",
            Self::RecencyAt => "recency_at_ms",
        }
    }

    /// Returns this key's timestamp of one row.
    fn of(self, row: &SessionRow) -> TimestampMs {
        match self {
            Self::CreatedAt => row.created_at_ms,
            Self::UpdatedAt => row.updated_at_ms,
            Self::RecencyAt => row.recency_at_ms,
        }
    }
}

/// Which Sessions a listing includes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionScope {
    /// Every Session.
    #[default]
    All,
    /// Sessions that received a user message.
    Visible,
    /// Visible Sessions that are not owned subagents.
    Interactive,
}

/// Keyset position after the last row of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAnchor {
    /// Sort-key timestamp of the last row.
    pub at_ms: TimestampMs,
    /// Identifier of the last row, breaking timestamp ties.
    pub id: SessionId,
}

/// Listing filters (a subset of Codex `ThreadFilterOptions`).
#[derive(Debug, Clone, Default, typed_builder::TypedBuilder)]
pub struct SessionFilter<'a> {
    /// Only Sessions created in this working directory.
    #[builder(default)]
    pub cwd: Option<&'a Path>,
    /// Visibility scope.
    #[builder(default)]
    pub scope: SessionScope,
    /// Only Sessions whose latest provider matches.
    #[builder(default)]
    pub provider_id: Option<&'a str>,
    /// Substring matched literally against name and preview.
    #[builder(default)]
    pub search_term: Option<&'a str>,
    /// Ordering timestamp.
    #[builder(default)]
    pub sort_key: SortKey,
    /// Continue after this position.
    #[builder(default)]
    pub anchor: Option<SessionAnchor>,
    /// Maximum rows in the page; all rows when absent.
    #[builder(default)]
    pub limit: Option<NonZeroU32>,
}

/// One page of a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionsPage {
    /// Rows in sort order.
    pub sessions: Vec<SessionRow>,
    /// Position to request the next page from, when more rows exist.
    pub next_anchor: Option<SessionAnchor>,
}

/// Builds the listing SQL for one filter, shared by the query and its plan test.
struct SessionListQuery<'f> {
    filter: &'f SessionFilter<'f>,
}

impl SessionListQuery<'_> {
    /// Assembles the statement; predicates match the partial indexes literally.
    fn build(&self, prefix: &str) -> Result<QueryBuilder<Sqlite>, StateError> {
        let column = self.filter.sort_key.column();
        let mut query =
            QueryBuilder::new(format!("{prefix}SELECT * FROM sessions"));
        let mut joiner = " WHERE ";
        let mut condition = |query: &mut QueryBuilder<Sqlite>, sql: &str| {
            query.push(joiner).push(sql);
            joiner = " AND ";
        };
        if let Some(cwd) = self.filter.cwd {
            condition(&mut query, "cwd = ");
            query.push_bind(cwd.sql_text()?.to_owned());
        }
        match self.filter.scope {
            SessionScope::All => {}
            SessionScope::Visible => {
                condition(&mut query, "has_user_message = 1")
            }
            SessionScope::Interactive => {
                condition(
                    &mut query,
                    "has_user_message = 1 AND owner_session_id IS NULL",
                );
            }
        }
        if let Some(provider) = self.filter.provider_id {
            condition(&mut query, "provider_id = ");
            query.push_bind(provider.to_owned());
        }
        if let Some(term) = self.filter.search_term {
            let pattern = format!(
                "%{}%",
                term.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            );
            condition(&mut query, "(name LIKE ");
            query
                .push_bind(pattern.clone())
                .push(" ESCAPE '\\' OR preview LIKE ")
                .push_bind(pattern)
                .push(" ESCAPE '\\')");
        }
        if let Some(anchor) = &self.filter.anchor {
            let at = i64::try_from(anchor.at_ms.get())?;
            // The ordering index can seek directly to this compound key.
            condition(&mut query, &format!("({column}, id) < ("));
            query
                .push_bind(at)
                .push(", ")
                .push_bind(anchor.id.as_str().to_owned())
                .push(")");
        }
        query.push(format!(" ORDER BY {column} DESC, id DESC"));
        if let Some(limit) = self.filter.limit {
            // One extra row tells whether another page exists.
            query.push(" LIMIT ").push_bind(i64::from(limit.get()) + 1);
        }
        Ok(query)
    }
}

impl StateRuntime {
    /// Looks up one Session by identifier.
    pub async fn session(
        &self,
        id: &SessionId,
    ) -> Result<Option<SessionRow>, StateError> {
        sqlx::query_as::<_, SessionColumns>(
            "SELECT * FROM sessions WHERE id = ?",
        )
        .bind(id.as_str())
        .fetch_optional(&self.state)
        .await?
        .map(SessionRow::try_from)
        .transpose()
    }

    /// Lists one page of Sessions newest first by the filter's sort key.
    pub async fn list_sessions(
        &self,
        filter: SessionFilter<'_>,
    ) -> Result<SessionsPage, StateError> {
        let rows = SessionListQuery { filter: &filter }
            .build("")?
            .build_query_as::<SessionColumns>()
            .fetch_all(&self.state)
            .await?;
        let mut sessions = rows
            .into_iter()
            .map(SessionRow::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let limit = filter
            .limit
            .and_then(|limit| usize::try_from(limit.get()).ok());
        let next_anchor = match limit {
            Some(limit) if sessions.len() > limit => {
                sessions.truncate(limit);
                sessions.last().map(|row| SessionAnchor {
                    at_ms: filter.sort_key.of(row),
                    id: row.id.clone(),
                })
            }
            _ => None,
        };
        Ok(SessionsPage {
            sessions,
            next_anchor,
        })
    }

    /// Lists Sessions owned by `owner`, ordered by agent name.
    pub async fn children(
        &self,
        owner: &SessionId,
    ) -> Result<Vec<SessionRow>, StateError> {
        sqlx::query_as::<_, SessionColumns>(CHILDREN_SQL)
            .bind(owner.as_str())
            .fetch_all(&self.state)
            .await?
            .into_iter()
            .map(SessionRow::try_from)
            .collect()
    }

    /// Removes one Session from both databases, state first like every two-database write.
    pub async fn delete_session(
        &self,
        id: &SessionId,
    ) -> Result<(), StateError> {
        self.delete_session_at(id, None).await
    }

    /// A stale filesystem listing may remove only rows still pointing at the missing path.
    pub async fn delete_session_at(
        &self,
        id: &SessionId,
        path: Option<&Path>,
    ) -> Result<(), StateError> {
        let path = path.map(SqlText::sql_text).transpose()?;
        let mut state = self.state.begin_with("BEGIN IMMEDIATE").await?;
        let mut history = self.history.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "DELETE FROM sessions WHERE id = ? AND (? IS NULL OR path = ?)",
        )
        .bind(id.as_str())
        .bind(path)
        .bind(path)
        .execute(&mut *state)
        .await?;
        sqlx::query("DELETE FROM projection_state WHERE session_id = ? AND (? IS NULL OR path = ?)")
            .bind(id.as_str())
            .bind(path).bind(path)
            .execute(&mut *history)
            .await?;
        state.commit().await?;
        history.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::{Row, SqlitePool};

    use super::*;

    /// Opens an in-memory database with the state schema applied.
    async fn schema() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("memory database");
        sqlx::migrate!("./migrations/state")
            .run(&pool)
            .await
            .expect("migrate");
        pool
    }

    /// Returns the query plan text of one listing.
    async fn plan(pool: &SqlitePool, filter: SessionFilter<'_>) -> String {
        SessionListQuery { filter: &filter }
            .build("EXPLAIN QUERY PLAN ")
            .expect("sql")
            .build()
            .fetch_all(pool)
            .await
            .expect("plan")
            .iter()
            .map(|row| row.get::<String, _>("detail"))
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// Each listing shape is served by the index built for it.
    #[tokio::test]
    async fn list_queries_use_expected_indexes() {
        let pool = schema().await;
        let cwd = Path::new("/work");
        let anchored = SessionFilter::builder()
            .anchor(Some(SessionAnchor {
                at_ms: 42.into(),
                id: SessionId::try_from("anchor").expect("id"),
            }))
            .build();
        let deep_page = plan(&pool, anchored).await;
        assert!(
            deep_page.contains(
                "SEARCH sessions USING INDEX idx_sessions_updated_at_ms"
            ),
            "{deep_page}"
        );
        for (filter, index) in [
            (
                SessionFilter::builder().build(),
                "idx_sessions_updated_at_ms",
            ),
            (
                SessionFilter::builder()
                    .cwd(Some(cwd))
                    .sort_key(SortKey::RecencyAt)
                    .build(),
                "idx_sessions_cwd_recency_at_ms",
            ),
            (
                SessionFilter::builder()
                    .scope(SessionScope::Visible)
                    .sort_key(SortKey::CreatedAt)
                    .build(),
                "idx_sessions_visible_created_at_ms",
            ),
            (
                SessionFilter::builder()
                    .scope(SessionScope::Interactive)
                    .build(),
                "idx_sessions_interactive_updated_at_ms",
            ),
        ] {
            let text = plan(&pool, filter).await;
            assert!(text.contains(index), "expected {index}, got {text}");
        }
        let children: Vec<String> = sqlx::query(sqlx::AssertSqlSafe(format!(
            "EXPLAIN QUERY PLAN {CHILDREN_SQL}"
        )))
        .bind("owner")
        .fetch_all(&pool)
        .await
        .expect("children plan")
        .iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect();
        assert!(
            children.join(" ").contains("idx_sessions_owner"),
            "{children:?}"
        );
    }
}
