# SQLite State and Session History Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Mirror every JSONL session into two rebuildable SQLite databases, following Codex `codex-rs/state`. The databases replace the store's full-file scans in the kernel and let a future GUI read session lists, subagent trees, and history without an ACP connection.

**Architecture:** A new `state` crate owns `StateRuntime`, which holds two sqlx pools: `state_1.sqlite` for session metadata and backfill state, and `session_history_1.sqlite` for runs, entries, and lanes. Each database has its own embedded migrations. A `SessionProjector` reads each JSONL file incrementally from per-database cursors and applies every line exactly once under `BEGIN IMMEDIATE`. The store stays synchronous and SQLite-free: it gains a public `MutationReader` and a `SyncObserver` hook. The kernel routes every metadata lookup through a `SessionCatalog`, which falls back to today's file scans whenever SQLite is unavailable or the backfill has not finished.

**Tech Stack:** Rust (protocol / store / state / kernel / acp / app crates), sqlx 0.9 with bundled SQLite, tokio, serde, typed-builder; Node e2e scripts over CDP for WebUI acceptance.

**Spec:** [SQLite state design](../specs/2026-11-17-sqlite-state-design.md)

## Global Constraints

- Database files live in the existing `data_root` (`session_persistence.data_home`, default `~/.local/share/clawcode/`). They are named exactly `state_1.sqlite` and `session_history_1.sqlite`. Migrations live in `crates/state/migrations/` and `crates/state/session_history_migrations/`. Both initial migrations copy the DDL in spec §2.1 and §2.2 verbatim, comments included.
- Connection settings: `journal_mode = WAL`, `synchronous = NORMAL`, `auto_vacuum = INCREMENTAL`, `foreign_keys = ON`, `busy_timeout = 5s`, pool `max_connections = 5`. The migrators are built with `sqlx::migrate!` and use `set_ignore_missing(true)`.
- A corrupt database (SQLite primary result code 11 `SQLITE_CORRUPT` or 26 `SQLITE_NOTADB`) is renamed to `<filename>.corrupt-<unix ms>`, together with its `-wal` and `-shm` files, and opened again once.
- Startup open and migration retry on `SQLITE_BUSY` (primary code 5) after 100 ms and then 200 ms, for 3 attempts in total (Codex `sqlite/busy_retry.rs`).
- Bundled SQLite must be at least 3.54. The Codex patch provides it: `[patch.crates-io]` points `sqlx-core` and `sqlx-sqlite` at `https://github.com/transact-rs/sqlx` rev `10462fa4414c6c7f1ed1d39bb31678e3b726878c`, with `libsqlite3-sys = { version = "0.39.1", default-features = false }`. `crates/state/src/lib.rs` asserts `libsqlite3_sys::SQLITE_VERSION_NUMBER >= 3_054_000` in a `const _: () = assert!(..)`. Stock sqlx 0.9.0 bundles 3.51.3, which is exposed to RUSTSEC-2026-0459.
- Every operation that writes both databases (projection, reprojection, `delete_session`) opens its `BEGIN IMMEDIATE` transaction on `state` first and `session_history` second.
- Cursor: a missing `projection_state` row is offset 0 and sequence 0. The line at offset 0 must be the header, after which `next_sequence` is 1. Every later line's seq must equal `next_sequence`, and applying it adds 1. A header-only Session gets `updated_at_ms = recency_at_ms = created_at_ms`.
- Backfill lease is `900_000` ms (Codex `BACKFILL_LEASE_SECONDS = 900`). The watermark and heartbeat are written every `100` files.
- `preview` holds the first 200 Unicode chars of the first user message. A `user` message contributes its text blocks joined with `\n`; an `expanded_user` message contributes `expansion.invocation.original`.
- `tokens_used` accumulates `total_tokens` from `ModelUsage` decoded through protocol types. Unreported counts add 0.
- `session_mode` changes only for a custom entry with `extensionId == ProductIdentity::SLUG` and `customType == "session_mode"`. The value is `data.mode`.
- An open run has `runs.status = 'in_progress'`. On finish it gets the verbatim `operation_finished.status`.
- Git probe commands are `git rev-parse HEAD`, `git rev-parse --abbrev-ref HEAD`, and `git remote get-url origin`, each with a 5 s timeout (Codex `GIT_COMMAND_TIMEOUT`). `ProductIdentity::VERSION = env!("CARGO_PKG_VERSION")`.
- Header fields are `git` (`sha`, `branch`, `originUrl`) and `appVersion`. Both are optional and omitted when `None`, and legacy headers must still parse.
- The ACP `session/list` wire shape is unchanged. `modifiedAtMs` now carries `updated_at_ms`.
- AGENTS.md applies, as summarized in spec §6:
  - English comments on non-trivial logic and on every new function.
  - `typed-builder` for structs with more than 3 fields, with `#[builder(default)]` on `Option` fields.
  - `Arc::clone(&x)`; no single-argument free helpers over project types.
  - Tests only in crate `tests/` or `#[cfg(test)] mod tests`.
  - Fully qualified `tracing::` macros with plain format args.
  - Context-holding structs per rule 15, and standard traits (`From`/`TryFrom`) at conversion boundaries.
  - Cargo tables grouped per rule 11 and declared in root `[workspace.dependencies]` per rule 12.
  - Bound SQL parameters only.
- No per-task commits. Everything ships in one commit on `main`, and only when the user explicitly asks (Task 10).

## Review Focus

- A sync event still queued for a Session that is then deleted must not bring its row back: after `record_deleted`, flushing the worker leaves no `sessions` row. Test in Task 7.
- An entry appended on lane `side` while lane `main` has an open run must get `run_id = NULL`, not the other lane's run. Test in Task 5.
- A search term containing `%` or `_` matches those characters literally. Test in Task 6.
- A backfill that dies after a checkpoint and resumes after its lease expires must not double-count tokens. Test in Task 7.
- A first user message of 300 CJK characters with emoji yields a `preview` of exactly 200 chars that is valid UTF-8 and never splits a character. Test in Task 5.

---

### Task 1: Session provenance in protocol and the JSONL header

**Files:**
- Modify: `crates/protocol/src/session.rs` (new types next to `SessionOwnership`)
- Modify: `crates/protocol/src/identity.rs` (add `VERSION`)
- Modify: `crates/store/src/model.rs:31-41` (`SessionCreateOptions`)
- Modify: `crates/store/src/factory.rs` (`V4Header`, `create_with`)
- Modify: every `SessionCreateOptions { .. }` literal in the workspace (about 148 sites, most of them in tests)
- Test: `crates/store/tests/jsonl.rs`

**Interfaces:**
- Produces:
  - `protocol::SessionGitInfo { pub sha: Option<String>, pub branch: Option<String>, pub origin_url: Option<String> }`: `Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize`, camelCase, each field `skip_serializing_if = "Option::is_none"` and `default`.
  - `protocol::SessionProvenance { pub git: Option<SessionGitInfo>, pub app_version: String }`: same derives except `Default`.
  - `ProductIdentity::VERSION: &str`
  - `store::SessionCreateOptions` with `#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]` and fields `session_id: SessionId`, `cwd: PathBuf`, `#[builder(default)] parent_session_id: Option<SessionId>`, `#[builder(default, setter(strip_option))] provenance: Option<SessionProvenance>`.

- [ ] **Step 0: Record the baseline.** Run `cargo test --workspace --no-fail-fast -- --skip cancelled_write_to_unread_tty_returns 2>&1 | grep -E "^test result|FAILED|panicked"` on unmodified `main`. Save the list of failing tests; Task 10 compares against it.

- [ ] **Step 1: Write the failing tests** in `crates/store/tests/jsonl.rs`:
  - `header_persists_provenance_fields`: create a Session with `provenance(SessionProvenance { git: Some(SessionGitInfo { sha: Some("abc123".into()), branch: Some("main".into()), origin_url: Some("git@example.com:o/r.git".into()) }), app_version: "9.9.9".into() })`. Parse the file's first line as `serde_json::Value`. Assert `header["git"]["sha"] == "abc123"`, `header["git"]["originUrl"] == "git@example.com:o/r.git"`, and `header["appVersion"] == "9.9.9"`.
  - `header_omits_absent_provenance`: create a Session without provenance. Assert that the first-line object has no `git` key and no `appVersion` key.
  - `legacy_header_without_provenance_still_lists`: write `{"kind":"header","version":4,"id":"session-legacy","createdAt":"1","cwd":"/legacy"}\n` into the encoded `/legacy` directory. Assert that `factory.list(None)` contains `session-legacy`.

- [ ] **Step 2: Run `cargo test -p store --test jsonl header_` and confirm that the tests fail to compile** (no `provenance` builder method).

- [ ] **Step 3: Implement.**
  - Add the protocol types and `VERSION`.
  - Convert `SessionCreateOptions` to a builder and migrate every literal to `SessionCreateOptions::builder()...build()`.
  - Add `#[serde(skip_serializing_if = "Option::is_none")] #[builder(default)]` fields `git: Option<SessionGitInfo>` and `app_version: Option<String>` (serialized as `appVersion`) to `V4Header`, and have `create_with` copy them from `options.provenance`.

- [ ] **Step 4: Verify.** `cargo test -p store` passes. `grep -rnE '(^|[^A-Za-z])SessionCreateOptions \{' crates` prints only the struct definition. The character class keeps `OwnedSessionCreateOptions {` out of the match. `cargo check --workspace --tests` passes.

---

### Task 2: Public JSONL reader and session file listing in store

**Files:**
- Create: `crates/store/src/reader.rs`
- Modify: `crates/store/src/lib.rs` (`mod reader;`, `pub use reader::{LogHeader, LogMutation, LoggedMutation, MutationReader};`)
- Modify: `crates/store/src/factory.rs` (extract the shared directory walk; add `session_paths`, `sessions_root`)
- Modify: `crates/store/src/state.rs` (derive `Deserialize` on the name and label fact mutations if the reader decodes through them)
- Test: `crates/store/tests/reader.rs`

**Interfaces:**
- Consumes: `V4Header` fields from Task 1.
- Produces:
  - `pub struct LogHeader { pub id: SessionId, pub created_at: TimestampMs, pub cwd: PathBuf, pub parent_session_id: Option<SessionId>, pub ownership: Option<SessionOwnership>, pub git: Option<SessionGitInfo>, pub app_version: Option<String> }`: typed-builder, with `impl TryFrom<&[u8]> for LogHeader { type Error = StoreError; }`. It rejects any line whose `kind` is not `header` or whose `version` is not 4.
  - `pub enum LogMutation { Header(LogHeader), Lane { sequence: Sequence, lane: LaneId, leaf_id: Option<EntryId> }, Entry { lane: LaneId, entry: SessionEntry }, Record(SessionRecord), Name { sequence: Sequence, name: Option<String> }, Label { sequence: Sequence, target_id: EntryId, label: Option<String> } }` with `pub fn sequence(&self) -> Option<u64>` (`None` for `Header`).
  - `pub struct LoggedMutation { pub offset: u64, pub next_offset: u64, pub mutation: LogMutation }`
  - `pub struct MutationReader<'a>` with `pub fn new(bytes: &'a [u8], base_offset: u64) -> Self` and `impl Iterator for MutationReader<'_> { type Item = Result<LoggedMutation, StoreError>; }`. It yields only `\n`-terminated lines and returns `None` at a torn tail. It decodes lines without validating them; a line whose `kind` is `header` becomes `Header`.
  - `JsonlStoreFactory::session_paths(&self) -> Result<Vec<PathBuf>, StoreError>` returns every `*.jsonl` file under `sessions_root/*/`, sorted. `JsonlStoreFactory::sessions_root(&self) -> &Path`.

- [ ] **Step 1: Write the failing tests** in `crates/store/tests/reader.rs`. Each one writes a Session through `JsonlStoreFactory` (two entries, one record, a name fact, a second lane, a label) and reads the file bytes.
  - `reader_yields_header_then_every_mutation_in_order`: the first item is `Header` with the Session id. The `sequence()` values of the remaining items are `1..=n`. Each `next_offset` equals the next item's `offset`, and the last one equals the file length.
  - `reader_resumes_from_any_line_offset`: start a second reader at the third item's `offset`, passing `&bytes[offset..]` and that `base_offset`. It yields exactly the items from the third onward, with identical offsets.
  - `reader_stops_before_torn_tail`: append `{"kind":"entry"` with no newline. The last yielded `next_offset` equals the length before the append.
  - `reader_reports_invalid_complete_line`: append `not json\n`. The final item is `Err(StoreError::InvalidSession(_))` or `Err(StoreError::Json(_))`.
  - `session_paths_are_sorted_jsonl_files`: two cwds with two Sessions each, plus a stray `notes.txt`. The result holds exactly the four `.jsonl` paths in sorted order.

- [ ] **Step 2: Run `cargo test -p store --test reader` and confirm failure** (unresolved imports).

- [ ] **Step 3: Implement** the reader and `LogHeader`. Move the directory walk that `scan_headers` and `list` duplicate into one private `JsonlStoreFactory::session_files(&self, cwd: Option<&Path>)`. Use it from `scan_headers`, `list`, and `session_paths`.

- [ ] **Step 4: Verify.** `cargo test -p store` passes.

---

### Task 3: `SyncObserver` hook in store

**Files:**
- Modify: `crates/store/src/model.rs` (trait), `crates/store/src/lib.rs` (export)
- Modify: `crates/store/src/factory.rs` (field, `with_sync_observer`, pass the observer to every store it builds, notify in `set_name_at`)
- Modify: `crates/store/src/session.rs` (builder field, notify in `sync`)
- Test: `crates/store/tests/jsonl.rs`

**Interfaces:**
- Produces:
  - `pub trait SyncObserver: Send + Sync { fn synced(&self, session_id: &SessionId, path: &Path); }`
  - `JsonlStoreFactory::with_sync_observer(self, observer: Arc<dyn SyncObserver>) -> Self` (`#[must_use]`), stored as `sync_observer: Option<Arc<dyn SyncObserver>>`.
  - `JsonlSessionStore` builder field `#[builder(default)] sync_observer: Option<Arc<dyn SyncObserver>>`. `create`, `create_owned`, `open`, and `fork` all set it from the factory.

- [ ] **Step 1: Write the failing tests** with a test-local `RecordingObserver(Mutex<Vec<(SessionId, PathBuf)>>)`:
  - `observer_fires_after_store_sync`: create, append an entry, call `sync()`. Assert one record equal to `(id, store.path())`.
  - `observer_fires_after_set_name_at_fast_path`: after a synced append, `set_name_at(path, Some("n"))`. Assert one new record.
  - `observer_fires_once_for_set_name_at_replay_path`: on a header-only log, `set_name_at` replays the log, so it must notify exactly once.
  - `observer_silent_when_set_name_at_fails`: `set_name_at` on a missing path returns `Err`. No record is added.

- [ ] **Step 2: Run `cargo test -p store --test jsonl observer_` and confirm failure.**

- [ ] **Step 3: Implement.** Notify only after `sync_data` succeeds. The replay path notifies through the opened store's `sync`, so `set_name_at` must not notify a second time.

- [ ] **Step 4: Verify.** `cargo test -p store` passes.

---

### Task 4: `state` crate foundation

**Files:**
- Modify: root `Cargo.toml` (`crates/state` in `members`; `state = { path = "crates/state" }` between `skill` and `store`; a new `# database` group after `# fs` holding `libsqlite3-sys = { version = "0.39.1", default-features = false }` and `sqlx = { version = "0.9", default-features = false, features = ["json", "macros", "migrate", "runtime-tokio", "sqlite-bundled"] }`; a new `[patch.crates-io]` table exactly as in spec §6.1)
- Modify: `Cargo.lock` (regenerated by `cargo build`)
- Create: `crates/state/Cargo.toml` (exactly as in spec §6.1)
- Create: `crates/state/migrations/0001_sessions.sql` (spec §2.1 DDL)
- Create: `crates/state/session_history_migrations/0001_session_history.sql` (spec §2.2 DDL)
- Create: `crates/state/src/lib.rs`, `src/config.rs`, `src/error.rs`, `src/database.rs`, `src/runtime.rs`
- Test: `crates/state/tests/runtime.rs`

**Interfaces:**
- Produces:
  - `pub struct SqliteConfig { home: PathBuf }` with `new(home: impl Into<PathBuf>) -> Self`, `state_db_path(&self) -> PathBuf`, and `session_history_db_path(&self) -> PathBuf`. Use `pub const STATE_DB_FILENAME: &str = "state_1.sqlite";` and `pub const SESSION_HISTORY_DB_FILENAME: &str = "session_history_1.sqlite";`.
  - `pub enum StateError` (thiserror) with variants `Sqlx(#[from] sqlx::Error)`, `Migrate(#[from] sqlx::migrate::MigrateError)`, `Io(#[from] std::io::Error)`, `Store(#[from] store::StoreError)`, `Json(#[from] serde_json::Error)`, `Projection { session_id: SessionId, offset: u64, reason: String }`, and `NonUtf8Path(PathBuf)`. It also provides `pub fn is_corruption(&self) -> bool`: true when the primary result code (`code & 0xff`) of a wrapped database error is 11 or 26.
  - `pub(crate) struct DatabaseOpener<'a> { path: &'a Path, migrator: Migrator }` with `async fn open(self) -> Result<SqlitePool, StateError>`. It applies the connection settings and migrations, retries on busy per the Global Constraints, and on corruption backs up and retries once.
  - `pub struct StateRuntime { state: SqlitePool, history: SqlitePool, backfill_complete: AtomicBool }` with `pub async fn init(config: &SqliteConfig) -> Result<Arc<Self>, StateError>` (creates `home`, opens both databases, and seeds `backfill_complete` from `backfill_state.status`) and `pub fn is_ready(&self) -> bool`.

- [ ] **Step 1: Write the failing tests** in `crates/state/tests/runtime.rs`. They inspect the files with their own `sqlx::SqlitePool`; the schema is the external contract.
  - `bundled_sqlite_is_at_least_3_54`: `SELECT sqlite_version()` on a runtime pool parses to at least `3.54.0`. This runtime check backs up the compile-time assertion.
  - `init_creates_both_databases`: both files exist. `sqlite_master` lists the tables `sessions`, `projection_state`, `backfill_state` and all 14 `idx_sessions_*` indexes in state, and `projection_state`, `runs`, `entries`, `lanes` plus the 5 indexes in history. `PRAGMA journal_mode` returns `wal`.
  - `init_starts_with_pending_backfill`: `is_ready()` is false, and `backfill_state` holds exactly one row with `status = 'pending'`.
  - `init_opens_database_migrated_by_newer_binary`: init, then insert into `_sqlx_migrations` a row with version `9999` (copy the other columns from version 1) and drop the runtime. A second `init` succeeds.
  - `init_backs_up_corrupt_database`: write `b"not a database"` to `state_1.sqlite`. `init` succeeds, a file starting with `state_1.sqlite.corrupt-` exists, and `sessions` is empty.
  - `sessions_reject_partial_ownership`: inserting a `sessions` row with `owner_session_id` set and `agent_name` NULL fails with a CHECK violation.

- [ ] **Step 2: Run `cargo test -p state --test runtime` and confirm failure.**

- [ ] **Step 3: Implement** the files listed above. Log `tracing::info!` on each successful open (with the path) and `tracing::warn!` on each backup (with the original and backup paths).

- [ ] **Step 4: Verify.** `cargo test -p state` passes, `cargo clippy -p state --tests -- -Dwarnings` is clean, and `cargo tree -i libsqlite3-sys` shows the single version `0.39.1` under the patched `sqlx-sqlite`.

---

### Task 5: `SessionProjector` and `StateRuntime::refresh`

**Files:**
- Create: `crates/state/src/projector.rs` (`SessionProjector`, cursor handling, reprojection)
- Create: `crates/state/src/projector/state_sink.rs` (state-database apply rules)
- Create: `crates/state/src/projector/history_sink.rs` (history-database apply rules)
- Modify: `crates/state/src/runtime.rs` (add `refresh`)
- Create: `crates/state/tests/support/mod.rs` (`SessionLog` writer helper)
- Test: `crates/state/tests/projection.rs`

**Interfaces:**
- Consumes: `MutationReader`, `LogHeader`, and `LogMutation` (Task 2); `StateRuntime` (Task 4).
- Produces:
  - `StateRuntime::refresh(&self, session_id: &SessionId, path: &Path) -> Result<(), StateError>`
  - `pub(crate) struct SessionProjector<'a> { runtime: &'a StateRuntime, session_id: &'a SessionId, path: &'a Path }` with `async fn run(self) -> Result<(), StateError>`
  - `pub(crate) struct Cursor { next_byte_offset: u64, next_sequence: u64 }`: one database's `projection_state` row (offset 0 and sequence 0 when the row is missing).
  - `pub(crate) struct StateSink<'t>` and `pub(crate) struct HistorySink<'t>`. Each wraps its database's `Transaction<'t, Sqlite>` and offers `async fn apply(&mut self, line: &LoggedMutation) -> Result<(), StateError>` and `async fn commit(self, cursor: Cursor) -> Result<(), StateError>`.
  - Test helper `SessionLog` (tests only). It wraps a `Box<dyn SessionStore>` created through `JsonlStoreFactory` and offers: `user(text)`, `expanded_user(original)`, `assistant(provider, model, total_tokens)`, `tool_result(call_id)`, `model_change(provider, model)`, `thinking(level)`, `custom(extension_id, custom_type, data)`, `run_started(run_id, operation)`, `run_finished(run_id, status, error: Option<&str>)`, `on_lane(lane)`, `name(Option<&str>)`, `label(entry_id, Option<&str>)`, and `sync()`. Messages are built from protocol `AgentMessage` values serialized into `NewEntry` payloads, matching what the kernel writes. Run records carry `{"operation": ...}` and `{"status": ...[, "error": ...]}` payloads, as in `crates/kernel/src/runtime/run.rs:245` and `run/settlement.rs:14`.

Algorithm for `SessionProjector::run`. It follows spec §3.1 and fixes the transaction order:

```text
state_tx   = state.begin_with("BEGIN IMMEDIATE")      // always state first
history_tx = history.begin_with("BEGIN IMMEDIATE")
read both projection_state rows inside their transactions (missing = offset 0, sequence 0; the header at offset 0 moves next_sequence to 1)
if history.path differs from `path`, or the file is shorter than either offset → reset both (delete the rows; FK cascades) and use offset 0
read the file from min(offsets) with tokio::fs; iterate MutationReader::new(bytes, min_offset)
  for each line: a sequence mismatch against a sink's next_sequence → reset both and restart once from 0
                 skip a sink whose cursor is already past line.offset; otherwise apply
  on the first apply/parse error: stop, keep the cursors at that line's offset, remember the error
commit state_tx with its cursor, then history_tx with its cursor; return the remembered error as StateError::Projection
```

- [ ] **Step 1: Write the failing tests** in `crates/state/tests/projection.rs`. Assertions read both database files directly with sqlx.
  - `refresh_projects_session_metadata`. The log is: owned header (owner `session-owner`, agent `worker`, nickname `Ada`), with provenance `abc123` / `main` / `git@example.com:o/r.git` / `9.9.9`; `run_started(run-1, "agent")`; `user("hello\nworld")`; `assistant("deepseek", "deepseek-flash", 120)`; `tool_result`; `assistant("deepseek", "deepseek-flash", 30)`; `run_finished(run-1, "completed", None)`; `thinking("high")`; `custom("clawcode", "session_mode", {"mode":"plan","turnId":"turn-x"})`; `name(Some("Demo"))`; `sync()`. Assert:
    - `name = 'Demo'`, `preview = 'hello\nworld'`, `has_user_message = 1`
    - `provider_id = 'deepseek'`, `model_id = 'deepseek-flash'`, `thinking_level = 'high'`, `session_mode = 'plan'`, `tokens_used = 150`
    - `recency_at_ms` equals the user entry's timestamp; `updated_at_ms` equals the last timestamped mutation's timestamp
    - `owner_session_id = 'session-owner'`, `agent_name = 'worker'`, `agent_nickname = 'Ada'`
    - `git_sha = 'abc123'`, `app_version = '9.9.9'`
  - `refresh_projects_history_matching_store`. On the same log, `entries` ordered by `sequence` match `store.entries()` in `entry_id`, `parent_id`, and `kind`. The user row has `message_type = 'user'`, and its `entry_json` deserializes to the store's `SessionEntry`. `lanes` holds one row `main` whose `leaf_id` equals `store.lane("main")`. `runs` holds one row with `operation = 'agent'`, `status = 'completed'`, `first_user_entry_id` = the user entry, `final_assistant_entry_id` = the second assistant entry, and `duration_ms = completed_at_ms - started_at_ms`. Every entry appended inside the run carries `run_id = 'run-1'`.
  - `refresh_is_incremental_and_idempotent`: refresh twice and `tokens_used` stays 150. Append `assistant(.., 7)` and sync; after a refresh it is 157, and the entry count grew by exactly 1.
  - `refresh_ignores_torn_tail_until_complete`: append a half line with no newline. Refresh succeeds with the cursor before it. Complete the line through the store (reopen to repair, then append) and it is projected.
  - `refresh_reprojects_rewritten_file`: overwrite the file with a different, shorter, valid log for the same id. After refresh the rows reflect only the new log.
  - `concurrent_refresh_counts_tokens_once`: two `StateRuntime::init` on the same home; `tokio::join!(a.refresh(..), b.refresh(..))`. Both are `Ok` and `tokens_used = 150`.
  - `third_party_session_mode_is_ignored`: `custom("other-ext", "session_mode", {"mode":"plan"})` leaves `session_mode = 'default'`.
  - `entry_on_other_lane_has_no_run`: open `run-1` on `main`, then append a user entry on lane `side`. That entry's `run_id` IS NULL (Review Focus).
  - `header_only_session_gets_initial_timestamps`: right after `create` (no entries), refresh. The row exists with `updated_at_ms = recency_at_ms = created_at_ms`, `preview = ''`, and `has_user_message = 0`; the state cursor's `next_sequence` is 1.
  - `preview_keeps_200_chars_on_char_boundaries`: the first user message is 300 characters mixing `汉` and `😀`. `preview` has exactly 200 chars (`chars().count()`) and equals the message's first 200 chars (Review Focus).
  - `invalid_line_stops_at_last_good_line`: append `{"kind":"bogus","seq":99}\n`. `refresh` returns `Err(StateError::Projection { .. })`, every line before it is projected, and the state cursor's `next_byte_offset` equals the bad line's offset.

- [ ] **Step 2: Run `cargo test -p state --test projection` and confirm failure.**

- [ ] **Step 3: Implement** the projector and both sinks following the spec §3.1 apply tables and the Global Constraints. Decode message entries with `serde_json::from_value::<AgentMessage>`. Log `tracing::warn!` once per reset (with session id, path, and reason) and once per projection error (with session id, path, and offset).

- [ ] **Step 4: Verify.** `cargo test -p state` passes.

---

### Task 6: Session and history queries

**Files:**
- Create: `crates/state/src/sessions.rs` (`SessionRow`, `SessionFilter`, list SQL builder, session queries; its `#[cfg(test)] mod tests` holds the `EXPLAIN QUERY PLAN` checks)
- Create: `crates/state/src/history.rs` (`RunRow`, `EntryRow`, history queries; its `#[cfg(test)] mod tests` holds the user-message index check)
- Modify: `crates/state/src/lib.rs` (exports)
- Test: `crates/state/tests/queries.rs`

**Interfaces:**
- Consumes: `StateRuntime::refresh` (Task 5).
- Produces (all exported from `state`):
  - `pub struct SessionRow` (typed-builder) holds every `sessions` column as typed values: `id: SessionId`, `path: PathBuf`, `cwd: PathBuf`, timestamps as `TimestampMs`, `ownership: Option<SessionOwnership>` rebuilt from the three owner columns, `git: Option<SessionGitInfo>`, `tokens_used: u64`, `has_user_message: bool`, and so on. It implements `TryFrom<&SqliteRow>`, `From<SessionRow> for protocol::SessionSummary` (`modified_at_ms = updated_at_ms`), and `From<SessionRow> for store::SessionHeader`.
  - `pub enum SortKey { CreatedAt, UpdatedAt, RecencyAt }` (default `UpdatedAt`); `pub enum SessionScope { All, Visible, Interactive }` (default `All`).
  - `pub struct SessionAnchor { pub at_ms: TimestampMs, pub id: SessionId }`
  - `pub struct SessionFilter<'a>` (typed-builder, all fields defaulted): `cwd: Option<&'a Path>`, `scope: SessionScope`, `provider_id: Option<&'a str>`, `search_term: Option<&'a str>`, `sort_key: SortKey`, `anchor: Option<SessionAnchor>`, `limit: Option<NonZeroU32>`.
  - `pub struct SessionsPage { pub sessions: Vec<SessionRow>, pub next_anchor: Option<SessionAnchor> }`
  - `StateRuntime` methods:
    - `session(&self, id: &SessionId) -> Result<Option<SessionRow>, StateError>`
    - `list_sessions(&self, filter: SessionFilter<'_>) -> Result<SessionsPage, StateError>`
    - `children(&self, owner: &SessionId) -> Result<Vec<SessionRow>, StateError>` (ordered by `agent_name`)
    - `delete_session(&self, id: &SessionId) -> Result<(), StateError>` (both databases, opening the state transaction first per the Global Constraints)
  - `pub struct RunRow` (typed-builder) mirrors the `runs` columns with typed ids. `EntryRow` is crate-private and converts with `TryFrom<&SqliteRow>` and `From<EntryRow> for SessionTreeEntry`; its payload is the `entry_json` entry's kind-specific `payload` map.
  - `StateRuntime` history methods. Each first calls `refresh` with the row's `path` and returns empty or `None` for an unknown Session.
    - `tree_snapshot(&self, id: &SessionId, lane: &LaneId) -> Result<Option<SessionTreeSnapshot>, StateError>`
    - `branch(&self, id: &SessionId, leaf: &EntryId) -> Result<Vec<SessionTreeEntry>, StateError>` (root to leaf, via a recursive CTE over the primary key)
    - `runs(&self, id: &SessionId, after_start_sequence: Option<u64>, limit: u32) -> Result<Vec<RunRow>, StateError>`
    - `run_entries(&self, id: &SessionId, run: &RunId) -> Result<Vec<SessionTreeEntry>, StateError>`
    - `user_entries(&self, id: &SessionId) -> Result<Vec<SessionTreeEntry>, StateError>`

List SQL rules. The partial-index predicates must appear literally, as in spec §2.3:
- `Visible` adds `preview <> ''`.
- `Interactive` adds `preview <> '' AND owner_session_id IS NULL`.
- A cwd filter is `cwd = ?`.
- The search term is `(name LIKE ? ESCAPE '\' OR preview LIKE ? ESCAPE '\')`, with `\`, `%`, and `_` escaped and the term wrapped in `%`.
- The anchor is `(<col> < ? OR (<col> = ? AND id < ?))`.
- Order by `<col> DESC, id DESC`. Fetch `limit + 1` rows to compute `next_anchor`.

- [ ] **Step 1: Write the failing tests.**
  - In `crates/state/tests/queries.rs`, seed five Sessions through `SessionLog` and `refresh`: two cwds, one owned child, one with no user message, and distinct providers. Then:
    - `list_orders_by_each_sort_key`
    - `list_filters_by_cwd_scope_and_provider`: `Visible` excludes the empty Session; `Interactive` also excludes the child.
    - `list_pages_with_anchor`: limit 2 walks all five without duplicates, and the last page has `next_anchor = None`.
    - `search_matches_percent_and_underscore_literally`: names `100%_done` and `100xydone`; searching `0%_` returns only the first (Review Focus).
    - `children_lists_owned_sessions_by_agent_name`
    - `delete_session_clears_both_databases`: no rows remain in `sessions`, either `projection_state`, `runs`, `entries`, or `lanes`.
    - `tree_snapshot_matches_store`: entries, leaf, and name match the store's state.
    - `branch_walks_root_to_leaf`
    - `runs_and_run_entries_page_in_log_order`
    - `history_reads_refresh_first`: append and sync without calling `refresh`; `tree_snapshot` already shows the new entry.
  - In `crates/state/src/sessions.rs`, `#[cfg(test)] mod tests` gets `list_queries_use_expected_indexes`. It runs `EXPLAIN QUERY PLAN` on the built SQL and asserts that the plan text contains:
    - `idx_sessions_updated_at_ms` for `All` + `UpdatedAt`
    - `idx_sessions_cwd_recency_at_ms` for a cwd filter + `RecencyAt`
    - `idx_sessions_visible_created_at_ms` for `Visible` + `CreatedAt`
    - `idx_sessions_interactive_updated_at_ms` for `Interactive` + `UpdatedAt`
    - `idx_sessions_owner` for the children query
  - In `crates/state/src/history.rs`, `#[cfg(test)] mod tests` asserts that `user_entries` uses `idx_entries_user_messages`.

- [ ] **Step 2: Run `cargo test -p state` and confirm the new tests fail.**

- [ ] **Step 3: Implement** the queries. Bind every value through `QueryBuilder::push_bind`. A `SessionListQuery<'a> { filter: &'a SessionFilter<'a> }` owns SQL construction, so the list method and the unit test build identical SQL.

- [ ] **Step 4: Verify.** `cargo test -p state` passes.

---

### Task 7: Backfill and `StateWorker`

**Files:**
- Create: `crates/state/src/backfill.rs`
- Create: `crates/state/src/worker.rs`
- Modify: `crates/state/src/lib.rs` (exports)
- Test: `crates/state/tests/backfill.rs`, `crates/state/tests/worker.rs`

**Interfaces:**
- Consumes: `refresh`, `session`, and `delete_session` (Tasks 5–6); `store::SyncObserver` and `JsonlStoreFactory::session_paths` (Tasks 2–3).
- Produces:
  - `pub enum BackfillOutcome { AlreadyComplete, LeaseHeld, Completed { scanned: usize, projected: usize, failed: usize } }`
  - `StateRuntime::backfill(&self, sessions_root: &Path, paths: &[PathBuf]) -> Result<BackfillOutcome, StateError>`. It is internally a `Backfill<'a> { runtime: &'a StateRuntime, sessions_root: &'a Path, now_ms: u64 }`, per rule 15. It claims the lease with `UPDATE backfill_state SET status = 'running', updated_at_ms = ? WHERE id = 1 AND (status = 'pending' OR (status = 'running' AND updated_at_ms < ?))` and requires `rows_affected == 1`. Before reading anything it sets `backfill_complete` when the status is already complete. The watermark is the path relative to `sessions_root`, as a UTF-8 string. A file whose header id already has a row with a different `path` is skipped, as is a non-UTF-8 path (with a warning).
  - `pub struct StateWorker { sender: mpsc::UnboundedSender<WorkerMessage> }` with `pub fn spawn(runtime: Arc<StateRuntime>) -> (Arc<Self>, tokio::task::JoinHandle<()>)`, `pub async fn flush(&self)` (returns after every message queued before it has been processed), and `impl store::SyncObserver for StateWorker`. `enum WorkerMessage { Synced(SessionId, PathBuf), Flush(oneshot::Sender<()>) }` is crate-private. The loop takes one message, drains the queued ones with `try_recv`, merges `Synced` by id (last path wins), refreshes each id with `tracing::warn!` on error, and answers the `Flush` messages last.

- [ ] **Step 1: Write the failing tests.**
  - `crates/state/tests/backfill.rs`:
    - `backfill_projects_every_file_and_completes`: three Sessions on disk. The result is `Completed { scanned: 3, projected: 3, failed: 0 }`; `is_ready()` is true and the status is `complete`. A second call returns `AlreadyComplete`.
    - `backfill_skips_when_lease_is_held`: set `status = 'running'` with a fresh `updated_at_ms`. The result is `LeaseHeld` and nothing is projected.
    - `backfill_resumes_after_expired_lease_without_double_counting`: run backfill, then set `status = 'running'`, `last_watermark` = the first file's relative path, and `updated_at_ms = 0`. Run again; `tokens_used` for every Session equals the single-run value (Review Focus).
    - `backfill_skips_duplicate_id_at_other_path`: copy one log to a second cwd directory. After backfill the row's `path` is the first-sorted file and `scanned = 2, projected = 1`.
    - `backfill_counts_unreadable_file_as_failed`: one file contains only `garbage\n`. The result is `failed = 1` and it still completes.
  - `crates/state/tests/worker.rs`:
    - `worker_refreshes_after_store_sync`: a factory `with_sync_observer(worker)`; create, append a user message, sync, `flush().await`. The `sessions` row has `has_user_message = 1`.
    - `worker_does_not_resurrect_deleted_session`: sync a Session, delete its file, `delete_session`, enqueue `synced(id, path)` again, `flush().await`. `session(id)` is `None` (Review Focus).

- [ ] **Step 2: Run `cargo test -p state --test backfill --test worker` and confirm failure.**

- [ ] **Step 3: Implement.** Logging: `tracing::info!` at backfill start (with file count), at each checkpoint (with the watermark), and at completion (scanned / projected / failed); `tracing::warn!` on each failed file and when the worker channel closes with messages still pending.

- [ ] **Step 4: Verify.** `cargo test -p state` passes.

---

### Task 8: Kernel `SessionCatalog` and call-site migration

**Files:**
- Modify: `crates/kernel/Cargo.toml` (`state = { workspace = true }` in `# workspace crates`)
- Create: `crates/kernel/src/runtime/session/catalog.rs`
- Modify: `crates/kernel/src/runtime.rs` (`KernelFactory` gains `#[builder(default)] state: Option<Arc<state::StateRuntime>>`; `Kernel` gains `catalog: SessionCatalog`)
- Modify: the call sites in spec §3.5. These are `crates/kernel/src/runtime/session.rs` (`list_sessions`, `session_runtime`, rollback delete), `session/open.rs`, `session/ownership.rs`, `session/close.rs`, `session/fork.rs`, `session/rename.rs`, `subagent/mail.rs`, `subagent/roster.rs`, `session/assembly.rs` (roster seeding), and `extension/host.rs`
- Modify: `crates/acp/src/server.rs` (list handler and `session_runtime` caller), `crates/acp/src/extension.rs:348`, `crates/acp/src/projection.rs` tests, and every kernel or acp test calling `list_sessions` / `session_runtime` (add `.await`)
- Modify: `crates/kernel/tests/subagent/fixture.rs` (`TestKernel::with_state`, async `owned_children`)
- Test: `crates/kernel/tests/subagent/state.rs` (registered in `crates/kernel/tests/subagent/main.rs`)

**Interfaces:**
- Consumes: `StateRuntime::{is_ready, session, list_sessions, children, refresh, delete_session}` and `From<SessionRow>` for `SessionSummary` / `SessionHeader` (Tasks 4–7).
- Produces:
  - `pub(in crate::runtime) struct SessionCatalog { store_factory: Arc<dyn StoreFactory>, state: Option<Arc<StateRuntime>> }`. Each method uses SQLite only when `state` is `Some` and `is_ready()`; otherwise it delegates to today's `StoreFactory` call:
    - `async fn list(&self, cwd: Option<&Path>) -> Result<Vec<SessionSummary>, KernelError>`: `SessionFilter` with `cwd`, scope `All`, sort `UpdatedAt`, no limit
    - `async fn summary(&self, id: &SessionId) -> Result<Option<SessionSummary>, KernelError>`
    - `async fn header(&self, id: &SessionId) -> Result<Option<SessionHeader>, KernelError>`: read repair per spec §3.4 (a row whose path is missing is deleted, then fall back to scan; a scan hit is `refresh`ed)
    - `async fn exists(&self, id: &SessionId) -> Result<bool, KernelError>`
    - `async fn children(&self, owner: &SessionId, cwd: &Path) -> Result<Vec<SessionHeader>, KernelError>`: SQLite `children` when ready, otherwise `scan_children`
    - `fn scan_children(&self, owner: &SessionId, cwd: &Path) -> Result<Vec<SessionHeader>, KernelError>`: synchronous `headers(Some(cwd))` filtered by owner. It is the one scan that `with_children`'s fallback and the async `children` share, so every store lookup stays inside `catalog.rs`.
    - `async fn refresh(&self, id: &SessionId, path: &Path)` and `async fn record_deleted(&self, id: &SessionId)`: never fail; `tracing::warn!` on error
  - `pub async fn Kernel::list_sessions(&self, cwd: Option<&Path>) -> Result<Vec<SessionSummary>, KernelError>`
  - `pub async fn Kernel::session_summary(&self, id: &SessionId) -> Result<Option<SessionSummary>, KernelError>`, used by `extension/host.rs` in place of `list_sessions(None).find(..)`
  - `pub async fn Kernel::session_runtime(&self, id: &SessionId) -> Result<SessionRuntimeSnapshot, KernelError>`
  - `AgentRoster::seed(&self, children: BTreeMap<AgentName, ChildEntry>)`: fills the cache only while it is `None`. `build_session` calls it with `catalog.children(..)` when the catalog is ready, so `with_children` keeps its signature. Its fallback calls `kernel.catalog.scan_children(..)` in place of `store_factory.headers(..)`.
  - Fixture: `pub async fn TestKernel::with_state(route, store: impl FnOnce(&Path, Arc<dyn Clock>) -> Arc<dyn StoreFactory>) -> Self`. It inits `StateRuntime` under `root/state` and runs `backfill` over the (empty) sessions root so the catalog is ready.

- [ ] **Step 1: Write the failing tests** in `crates/kernel/tests/subagent/state.rs`, using `TestKernel::with_state`:
  - `created_session_lists_immediately`: `create("session-a")`, then `list_sessions(None).await` contains it and `session_summary` returns it.
  - `renamed_session_lists_new_name_immediately`: `set_session_title` → `list_sessions` shows the new name.
  - `deleted_session_disappears`: `delete_session` → absent from `list_sessions`; `session_runtime` returns `SessionNotFound`.
  - `parallel_children_resume_shared_owner_with_state`: repeat the scenario of `crates/kernel/tests/subagent/delegation.rs:47` under `with_state`.
  - `roster_loads_from_state_without_header_scans`: wrap `JsonlStoreFactory` in a test-local `CountingStoreFactory` that counts `headers` calls. Spawn a child, then `close_session(&owner)` and `resume_session(owner, root)` in the same kernel (as `registry_rebuilds_from_headers_after_restart` in `crates/kernel/tests/subagent/roster.rs:386` does), and list agents with `path_prefix`. The listing still shows the child, and the `headers` count does not grow after the resume.
  - `catalog_scans_until_backfill_completes`: `StateRuntime` initialized but no backfill run → a created Session is still listed (through the scan).
  - `missing_file_row_is_repaired_on_lookup`: create and sync a Session, drop it from the live runtime, delete its JSONL file from disk. `session_runtime(id).await` returns `SessionNotFound`, and the state database no longer has its row.

- [ ] **Step 2: Run `cargo test -p kernel --test subagent state::` and confirm failure.**

- [ ] **Step 3: Implement.** Build `SessionCatalog` in `KernelFactory::build`, migrate every call site in the spec §3.5 table, and add `.await` at the async callers. The creation paths (`create_session`, `OwnedSessionSpawn::create` in `ownership.rs`, `fork`) call `catalog.refresh(&id, &path).await` right after the store returns. `set_session_title` in `rename.rs` calls it after both the live rename (after the transcript sync) and the persisted `set_name_at` rename. The delete paths (`perform_delete` in `close.rs` and the registration rollback in `session.rs`) call `catalog.record_deleted(&id).await` right after `store_factory.delete`.

- [ ] **Step 4: Verify.** `cargo test -p kernel` and `cargo test -p acp` pass, and `grep -rn "store_factory\.\(list\|headers\|header\|exists\)(" crates/kernel/src` only matches inside `catalog.rs`.

---

### Task 9: Git and app-version provenance on Session creation

**Files:**
- Create: `crates/kernel/src/runtime/session/provenance.rs`
- Modify: `crates/kernel/src/runtime/session/open.rs` (`create_session`), `session/ownership.rs` (`OwnedSessionSpawn::create`), `session/fork.rs`
- Test: `crates/kernel/tests/subagent/provenance.rs` (registered in `main.rs`)

**Interfaces:**
- Consumes: `SessionProvenance`, `SessionGitInfo`, `ProductIdentity::VERSION`, and the `SessionCreateOptions` builder (Task 1).
- Produces: `pub(in crate::runtime) struct ProvenanceProbe<'a> { cwd: &'a Path }` with `async fn collect(&self) -> SessionProvenance`. It runs the three git commands through `tokio::process::Command` with `current_dir(cwd)`, wrapped in `tokio::time::timeout(Duration::from_secs(5), ..)`, trimming stdout. A failing or timed-out command leaves its field `None`. When all three fields are `None`, `git` is `None`. `app_version` is `ProductIdentity::VERSION`. Every creation path sets `options.provenance` when the caller left it `None`.

- [ ] **Step 1: Write the failing tests:**
  - `session_header_records_git_provenance`: in `TestKernel::root`, run `git init -b main`, set `user.email` and `user.name`, run `git commit --allow-empty -m init` and `git remote add origin https://example.com/o/r.git`, then create a Session. The header line's `git.sha` equals `git rev-parse HEAD`, `git.branch == "main"`, `git.originUrl == "https://example.com/o/r.git"`, and `appVersion == env!("CARGO_PKG_VERSION")`.
  - `session_outside_git_has_no_git_field`: the header has no `git` key and still has `appVersion`.

- [ ] **Step 2: Run `cargo test -p kernel --test subagent provenance::` and confirm failure.**

- [ ] **Step 3: Implement** the probe and wire it into the three creation paths.

- [ ] **Step 4: Verify.** `cargo test -p kernel` passes.

---

### Task 10: App wiring, end-to-end check, full verification, and the gated commit

**Files:**
- Modify: `crates/app/Cargo.toml` (`state = { workspace = true }`)
- Modify: `crates/app/src/lib.rs`:
  - `ApplicationFactory::build` becomes `pub async fn build(self)`.
  - `Application` gains `state_worker: Option<Arc<StateWorker>>` and `pub async fn flush_state(&self)`.
- Modify: `crates/app/src/main.rs` (`.build().await?`; call `application.flush_state().await` after `terminate_exec_processes`)
- Modify: `crates/app/tests/extensions.rs` (`#[tokio::test]`, `.build().await`)

**Interfaces:**
- Consumes: `SqliteConfig`, `StateRuntime::init`, `StateWorker::spawn`, `StateRuntime::backfill`, `JsonlStoreFactory::{with_sync_observer, session_paths, sessions_root}`, and `KernelFactory::state`.
- Produces: the assembly order from spec §3.7:
  1. Resolve `data_root`.
  2. `StateRuntime::init(&SqliteConfig::new(&data_root))`. On `Err`, log `tracing::warn!` (path and error) and continue with `None`.
  3. With a runtime: `StateWorker::spawn` and `JsonlStoreFactory::new(..).with_sync_observer(worker)`.
  4. Pass `.state(runtime)` to `KernelFactory`.
  5. After `KernelFactory::build` succeeds, `tokio::spawn` `StateRuntime::sync_logs(&store_factory)` and warn on error (amended by the final-review fix pass; see below).

- [ ] **Step 1: Implement the wiring and update the app test.** `cargo test -p app` passes.

- [ ] **Step 2: Manual smoke run.** Start `cargo run -p app -- serve` against a copy of the real data dir (`session_persistence.data_home` pointing at a copy of `~/.local/share/clawcode`). Then check:
  - The log shows a backfill `Completed` line with `failed = 0`.
  - `state_1.sqlite` holds one `sessions` row per JSONL file, checked with `sqlite3`, or Python's `sqlite3` run with `-I` if the CLI is absent.

- [ ] **Step 3: WebUI end-to-end against the production backend** (AGENTS.md rule 9):

Run: `cargo build -p app && node crates/app/tests/web/subagent.e2e.mjs && node crates/app/tests/web/branch_summary.e2e.mjs && node crates/app/tests/web/plan_mode.e2e.mjs`
Expected: each script prints its success line and exits 0. The session list, Session Tree, and subagent views behave as before.

- [ ] **Step 4: Full verification.**

Run: `cargo test --workspace --no-fail-fast -- --skip cancelled_write_to_unread_tty_returns 2>&1 | grep -E "^test result|FAILED|panicked"` and `pre-commit run --all-files`
Expected: the failing set is identical to the Task 1 Step 0 baseline; fmt, clippy (`--tests --examples -- -Dwarnings`), and rig-boundary pass.

- [ ] **Step 5: Ask the user for commit permission.** Only after an explicit yes, read `.gitmessage`. Then `git add` the spec, this plan, and all changed files (leave out unrelated user edits), and commit on `main` with the title `feat(state): mirror sessions into SQLite state and history databases`. Do not push.

## Final-review fix pass

The whole-branch review changed these interfaces; the spec carries the binding text.

- `StateRuntime::sync_logs(&self, store: &Arc<JsonlStoreFactory>) -> Result<(), StateError>` (`crates/state/src/backfill.rs`) replaces the app-side backfill task. It lists paths on a blocking thread, runs `backfill`, sleeps `LEASE_POLL = 1s` and re-lists while the result is `LeaseHeld`, and then runs `Backfill::reconcile(paths)`. Tests: `sync_becomes_ready_after_held_lease_completes`, `sync_reconciles_logs_changed_behind_its_back`.
- `DatabaseOpener::open_with_retry` retries `StateError::Migrate` with the busy delays. Test: `concurrent_fresh_init_both_succeed`.
- `Cursor::starts_line(&mut File)` triggers reprojection when the byte before a cursor is not `\n`. Test: `refresh_reprojects_when_cursor_lands_mid_line`.
- `SessionProjector::apply` checks the header id once; both sinks dropped their copies. Test: `foreign_header_is_refused`.
- `HistoryLine::start_run` / `finish_run` fall back to `intent.kind` / `outcome` for compaction payloads. Fixture: `SessionLog::{compaction_started, compaction_finished}` serialize the protocol types. Test: `compaction_run_records_operation_and_outcome`.
- `ProvenanceProbe::collect` drops a `HEAD` branch and passes the origin through `sanitized_remote(&str) -> Option<String>`. Tests: `sanitized_remote_strips_credentials` (unit), `session_header_drops_origin_credentials_and_detached_head`.
