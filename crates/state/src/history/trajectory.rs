//! Trajectory pages (spec §3.4): the active branch of one Session turned
//! into typed records, paged by branch entry.

mod assemble;
mod branch;

use std::collections::{HashMap, HashSet};

use protocol::{
    ContentHash, EntryId, ModelUsage, QueueKind, RequestContent,
    RequestSnapshot, RunId, SessionId, ToolDefinition, TrajectoryCursor,
    TrajectoryPage, UserOrigin,
};
use sqlx::SqliteConnection;
use store::SessionEntry;

use self::assemble::{PageEntry, TrajectoryAssembler};
use self::branch::{BranchEntry, BranchIndex, PageWindow};
use crate::{StateError, StateRuntime};

/// Branch entries per page when the client names no limit.
const DEFAULT_LIMIT: u32 = 80;

/// Most branch entries one page may cover.
const MAX_LIMIT: u32 = 200;

/// The lane whose leaf is the active branch.
const MAIN_LANE: &str = "main";

/// Ancestors of the leaf without their JSON, root first.
const BRANCH_INDEX_SQL: &str = "WITH RECURSIVE chain(entry_id, parent_id, sequence, message_type, run_id) AS ( \
     SELECT entry_id, parent_id, sequence, message_type, run_id FROM entries \
     WHERE session_id = ?1 AND entry_id = ?2 \
     UNION ALL \
     SELECT e.entry_id, e.parent_id, e.sequence, e.message_type, e.run_id FROM entries e JOIN chain \
     ON e.session_id = ?1 AND e.entry_id = chain.parent_id AND e.sequence < chain.sequence) \
     SELECT entry_id, sequence, message_type, run_id FROM chain ORDER BY sequence";

/// The JSON of the listed entries.
const ENTRIES_SQL: &str = "SELECT entry_json FROM entries WHERE session_id = ? \
     AND entry_id IN (SELECT value FROM json_each(?)) ORDER BY sequence";

/// Queue marks of the listed user entries.
const ORIGINS_SQL: &str = "SELECT json_extract(record_json, '$.entryId'), json_extract(record_json, '$.target') \
     FROM records WHERE session_id = ? AND kind = 'queue_enqueued' \
     AND json_extract(record_json, '$.entryId') IN (SELECT value FROM json_each(?))";

/// The request snapshot of one assistant entry.
const SNAPSHOT_SQL: &str = "SELECT json_extract(entry_json, '$.content.metadata.request') \
     FROM entries WHERE session_id = ? AND entry_id = ?";

/// The usage of the listed assistant entries.
const USAGE_SQL: &str = "SELECT usage_json \
     FROM entries WHERE session_id = ? AND entry_id IN (SELECT value FROM json_each(?))";

/// The listed request contents.
const CONTENTS_SQL: &str = "SELECT hash, kind, content FROM request_contents \
     WHERE session_id = ? AND hash IN (SELECT value FROM json_each(?))";

/// One row of [`BRANCH_INDEX_SQL`].
#[derive(sqlx::FromRow)]
struct BranchRow {
    entry_id: String,
    sequence: i64,
    message_type: Option<String>,
    run_id: Option<String>,
}

impl TryFrom<BranchRow> for BranchEntry {
    type Error = StateError;

    /// Types the identifiers of one branch row.
    fn try_from(row: BranchRow) -> Result<Self, Self::Error> {
        Ok(Self::builder()
            .id(EntryId::try_from(row.entry_id)?)
            .sequence(u64::try_from(row.sequence)?)
            .message_type(row.message_type)
            .run_id(row.run_id.map(RunId::try_from).transpose()?)
            .build())
    }
}

/// The lossless `entry_json` column.
#[derive(sqlx::FromRow)]
struct EntryRow {
    #[sqlx(json)]
    entry_json: SessionEntry,
}

/// One row of [`CONTENTS_SQL`].
#[derive(sqlx::FromRow)]
struct ContentRow {
    hash: String,
    kind: String,
    content: String,
}

impl TryFrom<ContentRow> for RequestContent {
    type Error = StateError;

    /// Types the hash and kind of one stored content.
    fn try_from(row: ContentRow) -> Result<Self, Self::Error> {
        Ok(Self {
            hash: ContentHash::try_from(row.hash)?,
            kind: row.kind.parse()?,
            content: row.content,
        })
    }
}

/// Encodes identifiers as the JSON array `json_each` reads.
fn json_list<T: serde::Serialize>(
    items: impl Iterator<Item = T>,
) -> Result<String, StateError> {
    Ok(serde_json::to_string(&items.collect::<Vec<_>>())?)
}

/// What one page is assembled from, read from one history snapshot.
#[derive(typed_builder::TypedBuilder)]
struct PageInputs {
    leaf: Option<EntryId>,
    branch: BranchIndex,
    window: PageWindow,
    branch_changed: bool,
    entries: Vec<PageEntry>,
    origins: HashMap<EntryId, UserOrigin>,
    usage_before: ModelUsage,
    previous: Option<RequestSnapshot>,
    tool_names: HashMap<ContentHash, Vec<String>>,
}

impl PageInputs {
    /// Assembles the records and the page's cursors.
    fn into_page(self) -> TrajectoryPage {
        let (_, in_window) = self.branch.split(&self.window);
        let (first, last) = (
            in_window.first().map(|entry| entry.sequence),
            in_window.last().map(|entry| entry.sequence),
        );
        let records = TrajectoryAssembler::builder()
            .branch(&self.branch)
            .window(self.window.range.clone())
            .origins(&self.origins)
            .tool_names(&self.tool_names)
            .previous(self.previous)
            .build()
            .assemble(self.entries);
        TrajectoryPage::builder()
            .records(records)
            .leaf_id(self.leaf)
            .has_more_before(self.window.has_more_before)
            .has_more_after(self.window.has_more_after)
            .first_sequence(first)
            .last_sequence(last)
            .usage_before(self.usage_before)
            .branch_changed(self.branch_changed)
            .build()
    }
}

/// Reads one Session's trajectory inputs from one history snapshot.
struct TrajectoryReader<'c> {
    connection: &'c mut SqliteConnection,
    session: &'c SessionId,
}

impl TrajectoryReader<'_> {
    /// Everything one page at `cursor` needs, at most `limit` branch entries.
    async fn page(
        &mut self,
        cursor: TrajectoryCursor,
        limit: usize,
    ) -> Result<PageInputs, StateError> {
        let leaf = self.leaf().await?;
        let branch = match &leaf {
            Some(leaf) => self.branch(leaf).await?,
            None => BranchIndex::default(),
        };
        // An `after` cursor off the branch means navigation replaced the
        // branch the client loaded; it gets the new branch from its tail.
        let (cursor, branch_changed) = match cursor {
            TrajectoryCursor::After(sequence) if !branch.contains(sequence) => {
                (TrajectoryCursor::Tail, true)
            }
            cursor => (cursor, false),
        };
        let window = branch.window(cursor, limit);
        let (before, in_window) = branch.split(&window);
        let entries = self
            .entries(in_window.iter().map(|entry| &entry.id))
            .await?
            .into_iter()
            .map(PageEntry::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let origins = self
            .origins(in_window.iter().map(|entry| &entry.id))
            .await?;
        let earlier = || before.iter().filter(|entry| entry.is_assistant());
        // Appended pages keep the usage the client already holds.
        let usage_before = match cursor {
            TrajectoryCursor::After(_) => ModelUsage::default(),
            _ => self.usage(earlier().map(|entry| &entry.id)).await?,
        };
        // The first request of the page compares with the one before it.
        let previous = match earlier().next_back() {
            Some(entry) => self.snapshot(&entry.id).await?,
            None => None,
        };
        // Tool names matter only where catalogs differ.
        let catalogs: HashSet<&ContentHash> = previous
            .iter()
            .chain(entries.iter().filter_map(PageEntry::request))
            .map(|snapshot| &snapshot.tools)
            .collect();
        let tool_names = if catalogs.len() > 1 {
            self.tool_names(catalogs.into_iter()).await?
        } else {
            HashMap::new()
        };
        Ok(PageInputs::builder()
            .leaf(leaf)
            .branch(branch)
            .window(window)
            .branch_changed(branch_changed)
            .entries(entries)
            .origins(origins)
            .usage_before(usage_before)
            .previous(previous)
            .tool_names(tool_names)
            .build())
    }

    /// The leaf of the main lane; `None` before the first entry.
    async fn leaf(&mut self) -> Result<Option<EntryId>, StateError> {
        let leaf: Option<Option<String>> = sqlx::query_scalar(
            "SELECT leaf_id FROM lanes WHERE session_id = ? AND lane = ?",
        )
        .bind(self.session.as_str())
        .bind(MAIN_LANE)
        .fetch_optional(&mut *self.connection)
        .await?;
        Ok(leaf.flatten().map(EntryId::try_from).transpose()?)
    }

    /// The branch from the root to `leaf`.
    async fn branch(
        &mut self,
        leaf: &EntryId,
    ) -> Result<BranchIndex, StateError> {
        let rows: Vec<BranchRow> = sqlx::query_as(BRANCH_INDEX_SQL)
            .bind(self.session.as_str())
            .bind(leaf.as_str())
            .fetch_all(&mut *self.connection)
            .await?;
        rows.into_iter().map(BranchEntry::try_from).collect()
    }

    /// The listed entries with their JSON.
    async fn entries(
        &mut self,
        ids: impl Iterator<Item = &EntryId>,
    ) -> Result<Vec<SessionEntry>, StateError> {
        let rows: Vec<EntryRow> = sqlx::query_as(ENTRIES_SQL)
            .bind(self.session.as_str())
            .bind(json_list(ids)?)
            .fetch_all(&mut *self.connection)
            .await?;
        Ok(rows.into_iter().map(|row| row.entry_json).collect())
    }

    /// How the listed user entries entered the Session, from queue marks.
    async fn origins(
        &mut self,
        ids: impl Iterator<Item = &EntryId>,
    ) -> Result<HashMap<EntryId, UserOrigin>, StateError> {
        let rows: Vec<(String, String)> = sqlx::query_as(ORIGINS_SQL)
            .bind(self.session.as_str())
            .bind(json_list(ids)?)
            .fetch_all(&mut *self.connection)
            .await?;
        rows.into_iter()
            .map(|(entry, target)| {
                let origin = match target.parse::<QueueKind>()? {
                    QueueKind::Steering => UserOrigin::Steering,
                    QueueKind::FollowUp => UserOrigin::FollowUp,
                };
                Ok((EntryId::try_from(entry)?, origin))
            })
            .collect()
    }

    /// The request snapshot `entry` carries, if any.
    async fn snapshot(
        &mut self,
        entry: &EntryId,
    ) -> Result<Option<RequestSnapshot>, StateError> {
        let snapshot: Option<Option<String>> = sqlx::query_scalar(SNAPSHOT_SQL)
            .bind(self.session.as_str())
            .bind(entry.as_str())
            .fetch_optional(&mut *self.connection)
            .await?;
        Ok(snapshot
            .flatten()
            .map(|json| serde_json::from_str(&json))
            .transpose()?)
    }

    /// Sum of the usage of the listed assistant entries.
    async fn usage(
        &mut self,
        ids: impl Iterator<Item = &EntryId>,
    ) -> Result<ModelUsage, StateError> {
        let rows: Vec<Option<String>> = sqlx::query_scalar(USAGE_SQL)
            .bind(self.session.as_str())
            .bind(json_list(ids)?)
            .fetch_all(&mut *self.connection)
            .await?;
        let mut total = ModelUsage::default();
        for usage in rows.into_iter().flatten() {
            total += &serde_json::from_str::<ModelUsage>(&usage)?;
        }
        Ok(total)
    }

    /// Tool names of the listed tool-catalog hashes.
    async fn tool_names(
        &mut self,
        hashes: impl Iterator<Item = &ContentHash>,
    ) -> Result<HashMap<ContentHash, Vec<String>>, StateError> {
        let rows: Vec<ContentRow> = sqlx::query_as(CONTENTS_SQL)
            .bind(self.session.as_str())
            .bind(json_list(hashes)?)
            .fetch_all(&mut *self.connection)
            .await?;
        rows.into_iter()
            .map(|row| {
                let content = RequestContent::try_from(row)?;
                let tools: Vec<ToolDefinition> =
                    serde_json::from_str(&content.content)?;
                let names = tools.into_iter().map(|tool| tool.name).collect();
                Ok((content.hash, names))
            })
            .collect()
    }
}

impl StateRuntime {
    /// Reads one page of a Session's active-branch trajectory; `None` for an
    /// unknown Session.
    pub async fn trajectory(
        &self,
        id: &SessionId,
        cursor: TrajectoryCursor,
        limit: Option<u32>,
    ) -> Result<Option<TrajectoryPage>, StateError> {
        if !self.caught_up(id).await? {
            return Ok(None);
        }
        let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        // The leaf and every row the page reads come from one snapshot.
        let mut tx = self.history.begin().await?;
        let inputs = TrajectoryReader {
            connection: &mut tx,
            session: id,
        }
        .page(cursor, usize::try_from(limit)?)
        .await?;
        tx.commit().await?;
        Ok(Some(inputs.into_page()))
    }

    /// Reads one request content by hash: `None` when the Session is not
    /// indexed, `Some(None)` for a hash it does not hold.
    pub async fn request_content(
        &self,
        id: &SessionId,
        hash: &ContentHash,
    ) -> Result<Option<Option<RequestContent>>, StateError> {
        if !self.caught_up(id).await? {
            return Ok(None);
        }
        let row: Option<ContentRow> = sqlx::query_as(CONTENTS_SQL)
            .bind(id.as_str())
            .bind(json_list(std::iter::once(hash))?)
            .fetch_optional(&self.history)
            .await?;
        Ok(Some(row.map(RequestContent::try_from).transpose()?))
    }
}
