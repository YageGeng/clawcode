# SQLite 状态库与会话历史设计

## 目标

参照 Codex `codex-rs/state`，为 clawcode 引入 SQLite：把 JSONL 会话日志镜像成可索引的会话元数据库与会话历史库，替换 store 层的全量文件扫描，并让以后的 GUI 客户端不建立 ACP 连接也能完成会话列表、子代理树与历史浏览等基础查询。

成功标准：

- `session/list`、按 ID 定位会话、存在性检查、owner 链、子会话查询不再扫描或读取 JSONL 文件，改为 SQLite 索引查询。
- 创建、fork、改名、删除之后立即查询，结果可见（read-your-writes）。
- Rust GUI 链接 `state` crate、其他语言直接读 SQLite 文件，均能在 server 不运行时读到最新的会话列表与历史；读历史前的追赶保证新鲜度。
- SQLite 不可用、损坏或 backfill 未完成时，行为与现在完全一致（回退扫描），不丢数据、不让 kernel 操作失败。
- 两个库删除后可由 JSONL 完整重建。

约束：

- JSONL（pi v4 兼容）仍是唯一事实来源；SQLite 只保存可由 JSONL 派生的数据。
- 记录内容以 Codex 为准：Codex 记录且 clawcode 有对应数据源的全部记录，Codex 产品特有、clawcode 无对应功能的不建，不额外增加 Codex 没有的富信息表。
- 库文件命名、迁移体系、连接参数、backfill 与读修复流程照搬 Codex 规则。
- 代码风格遵守 `AGENTS.md`，见第 6 节。

非目标：

- HTTP 只读接口、CLI 子命令：GUI 形态未定，`state` crate 的 API 与库文件本身即接口，需要时在 axum router 上加路由即可。
- kernel 自身的 transcript 读取改走 SQLite：kernel 仍使用内存中的 JSONL 重放状态，history 库只服务外部读者。
- Codex 中 clawcode 无对应功能的数据：archived、pinned、sections、projects、read receipts、memory_mode、history_mode、daybreak、sandbox_policy、originator / creator 身份、guardian、remote control、external imports、realtime items、session_model_ids、dynamic tools。
- Codex 历史包袱：秒级时间戳列及其索引、`thread_items.updated_at_ordinal`（clawcode entry 不可变）、spawn edge 的 status（clawcode ownership 写在 header 中不可变）。
- 运行期间外部手工拷入 JSONL 的即时可见性：下次启动的对账才收录（见 3.3）。

## 与 Codex 的对应关系

| Codex | clawcode | 说明 |
|---|---|---|
| `codex-state` crate | `crates/state` | 同名同职责 |
| `StateRuntime` | `state::StateRuntime` | 持有两个连接池 |
| `SqliteConfig`、`state_5.sqlite`、`thread_history_1.sqlite` | `state::SqliteConfig`、`state_1.sqlite`、`session_history_1.sqlite` | `<用途>_<代号>.sqlite`，不兼容变更时代号加一，新文件由 backfill 重建 |
| `migrations/`、`thread_history_migrations/` | `crates/state/migrations/state/`、`crates/state/migrations/history/` | `sqlx::migrate!` 分别嵌入对应目录，两个 migrator 均设置 `ignore_missing = true` |
| `threads` | `sessions` | 逐列映射见 2.1 |
| `thread_spawn_edges` | `sessions.owner_session_id` + `idx_sessions_owner` | ownership 不可变，无需独立边表 |
| `backfill_state` | `backfill_state` | 同结构，时间戳改毫秒 |
| `thread_history_projection_state` | 两库各一张 `projection_state` | 游标与数据同库，见 3.1 |
| `thread_turns` | `runs` | Codex 的用户 turn 对应 clawcode 的 Run（`operation_started` 到 `operation_finished`） |
| `thread_items` | `entries` | 另加树结构必需的 `lane`、`parent_id` |
| — | `lanes` | clawcode 树结构必需，用于还原 `SessionTreeSnapshot` |
| `rollout/src/state_db.rs`（`reconcile_rollout`、`read_repair_rollout_path`、`apply_rollout_items`） | kernel `SessionCatalog` + `state::SessionProjector` | 读修复与增量应用 |
| `rollout/src/metadata.rs` `backfill_sessions`、`rollout/src/state_db.rs` `wait_for_backfill_gate` | `StateRuntime::sync_logs`（内含 `backfill`） | 租约 + 水位 + 完成标记；租约被占时轮询；完成后对账 |
| `Option<StateDbHandle>` | `Option<Arc<StateRuntime>>` | 初始化失败即 `None`，全程回退扫描 |
| `recovery::backup_runtime_db_for_fresh_start` | 同名职责 | 损坏库备份后重建 |
| `collect_git_info`、`SessionMeta.git`、`cli_version` | 创建时采集，写入 JSONL header 的 `git`、`appVersion` | 保持可重建 |

## 1 架构与数据流

### 1.1 组件

```
crates/store    同步、JSONL、pi v4 兼容；不依赖 SQLite
  ├─ MutationReader   新增公开只读解析器：从给定字节偏移解析完整行
  ├─ LogHeader        V4Header 的公开只读视图（含新增 git / appVersion）
  ├─ SyncObserver     新增 trait：sync 成功后回调 (session_id, path)
  └─ JsonlStoreFactory::session_paths  公开排序后的会话文件列表，供 backfill 与现有扫描共用

crates/state    sqlx + SQLite
  ├─ SqliteConfig     库目录、带代号的文件名、连接参数
  ├─ StateRuntime     两个池；元数据读写、history 查询、backfill、删除
  ├─ SessionProjector 单个会话的增量投影（一次读文件，分发到两个库）
  └─ StateWorker      SyncObserver 实现 + 后台任务：合并 sync 事件并追赶投影

crates/kernel
  └─ SessionCatalog { store_factory, state: Option<Arc<StateRuntime>> }
        kernel 内所有 list / headers / header / exists 调用改走此处；
        身份变化后 await refresh / record_deleted

crates/app      打开 StateRuntime（失败 warn 并传 None）、安装 SyncObserver、kernel 构建后后台启动 sync_logs
```

### 1.2 数据流

- 身份变化（create、create_owned、fork、改名、delete）：kernel 在 async 调用点 await 写入 SQLite，保证 read-your-writes。
- 高频变化（`updated_at_ms`、tokens、history）：transcript 在持锁状态下调用 `sync()`，不能 await；`JsonlSessionStore::sync` 成功后调用 `SyncObserver`，`StateWorker` 经 channel 收到事件后追赶，最终一致，延迟毫秒级。
- 外部读者：`StateRuntime` 的 history 查询先追赶再读；其他语言直接读库文件（WAL 支持多进程并发读）。
- Codex 的 recorder 是 async，可直接 await 写库；clawcode store 是同步的，因此拆成「身份 await + 高频 channel」两类，这是与 Codex 的唯一结构性差异。

## 2 表结构

所有业务列均可由 JSONL 派生。约束策略与 Codex 一致：结构性约束（单行表、布尔、跨列一致性）使用 `CHECK`，表间从属使用外键 `ON DELETE CASCADE`；由 JSONL 原样镜像的枚举值（`runs.status`、`runs.operation`、`entries.kind`、`entries.message_type`）不加枚举 `CHECK`，否则一个新取值会让整个会话投影失败。Codex 只对自己写入的数据（如 `thread_goals.status`）使用枚举 `CHECK`。外键依赖 sqlx `SqliteConnectOptions` 默认开启的 `foreign_keys`，与 Codex 相同。SQLite 不支持跨库外键，两库之间以 `session_id` 在应用层关联。

### 2.1 `state_1.sqlite` 的[元数据迁移](../../../crates/state/migrations/state/)

`sessions` 与 Codex `threads` 的列映射：

| Codex 列 | clawcode 列 | 来源 |
|---|---|---|
| `id` / `rollout_path` / `cwd` | `id` / `path` / `cwd` | header；`path` 为该 ID 已选定的规范文件 |
| `created_at_ms` | `created_at_ms` | header `createdAt` |
| `updated_at_ms` | `updated_at_ms` | 最后一条带时间戳的 mutation |
| `recency_at_ms` | `recency_at_ms` | 最后一条用户消息，无则 `created_at_ms` |
| `title` / `name` | `name` | name fact（clawcode 只有一种） |
| `first_user_message` / `preview` | `preview` | 首条用户消息文本（clawcode 无 goal，两列合一） |
| `has_user_event` | `has_user_message` | 存在用户消息 entry |
| `model_provider` / `model` | `provider_id` / `model_id` | 最近的 `model_change` 或 assistant metadata |
| `reasoning_effort` | `thinking_level` | 最近的 `thinking_level_change` |
| `approval_mode` | `session_mode` | 最近的 `session_mode` custom entry |
| `tokens_used` | `tokens_used` | assistant usage `total_tokens` 累加 |
| `attached_thread_parent_id` / `agent_path` / `agent_nickname` | `owner_session_id` / `agent_name` / `agent_nickname` | header ownership；`agent_path` 即 `/root/{agent_name}` |
| — | `parent_session_id` | header fork 来源（现有 `SessionSummary` 字段） |
| `git_sha` / `git_branch` / `git_origin_url` | 同名 | header `git`（新增采集，见 3.6） |
| `cli_version` | `app_version` | header `appVersion`（新增采集） |

```sql
-- Session metadata mirrored from JSONL logs (codex `threads`); every column is rebuildable.
CREATE TABLE sessions (
    id                TEXT PRIMARY KEY,
    path              TEXT NOT NULL,
    cwd               TEXT NOT NULL,
    created_at_ms     INTEGER NOT NULL,
    updated_at_ms     INTEGER NOT NULL,
    recency_at_ms     INTEGER NOT NULL,
    name              TEXT,
    preview           TEXT NOT NULL DEFAULT '',
    has_user_message  INTEGER NOT NULL DEFAULT 0 CHECK (has_user_message IN (0, 1)),
    provider_id       TEXT,
    model_id          TEXT,
    thinking_level    TEXT,
    session_mode      TEXT NOT NULL DEFAULT 'default',
    tokens_used       INTEGER NOT NULL DEFAULT 0,
    parent_session_id TEXT,
    owner_session_id  TEXT,
    agent_name        TEXT,
    agent_nickname    TEXT,
    git_sha           TEXT,
    git_branch        TEXT,
    git_origin_url    TEXT,
    app_version       TEXT,
    -- Ownership is all-or-nothing in the header.
    CHECK ((owner_session_id IS NULL) = (agent_name IS NULL))
);

-- Global listing, one index per codex sort key.
CREATE INDEX idx_sessions_created_at_ms ON sessions(created_at_ms DESC, id DESC);
CREATE INDEX idx_sessions_updated_at_ms ON sessions(updated_at_ms DESC, id DESC);
CREATE INDEX idx_sessions_recency_at_ms ON sessions(recency_at_ms DESC, id DESC);

-- Workspace listing for session/list with cwd and kernel workspace lookups.
CREATE INDEX idx_sessions_cwd_created_at_ms ON sessions(cwd, created_at_ms DESC, id DESC);
CREATE INDEX idx_sessions_cwd_updated_at_ms ON sessions(cwd, updated_at_ms DESC, id DESC);
CREATE INDEX idx_sessions_cwd_recency_at_ms ON sessions(cwd, recency_at_ms DESC, id DESC);

-- Visible listing hides Sessions that never received a user message.
CREATE INDEX idx_sessions_visible_created_at_ms ON sessions(created_at_ms DESC, id DESC)
    WHERE has_user_message = 1;
CREATE INDEX idx_sessions_visible_updated_at_ms ON sessions(updated_at_ms DESC, id DESC)
    WHERE has_user_message = 1;
CREATE INDEX idx_sessions_visible_recency_at_ms ON sessions(recency_at_ms DESC, id DESC)
    WHERE has_user_message = 1;

-- Interactive listing additionally hides owned subagent Sessions.
CREATE INDEX idx_sessions_interactive_created_at_ms ON sessions(created_at_ms DESC, id DESC)
    WHERE has_user_message = 1 AND owner_session_id IS NULL;
CREATE INDEX idx_sessions_interactive_updated_at_ms ON sessions(updated_at_ms DESC, id DESC)
    WHERE has_user_message = 1 AND owner_session_id IS NULL;
CREATE INDEX idx_sessions_interactive_recency_at_ms ON sessions(recency_at_ms DESC, id DESC)
    WHERE has_user_message = 1 AND owner_session_id IS NULL;

-- Owned children: close cascades, rosters, and mail delivery by agent name.
CREATE INDEX idx_sessions_owner ON sessions(owner_session_id, agent_name)
    WHERE owner_session_id IS NOT NULL;

-- Provider filter.
CREATE INDEX idx_sessions_provider ON sessions(provider_id);

-- Metadata projection cursor, stored with the rows it advances.
CREATE TABLE projection_state (
    session_id       TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    next_byte_offset INTEGER NOT NULL,
    next_sequence    INTEGER NOT NULL,
    source_id        TEXT NOT NULL DEFAULT '',
    source_size      INTEGER NOT NULL DEFAULT 0,
    source_mtime     TEXT NOT NULL DEFAULT ''
);

-- One-time filesystem backfill with lease and resumable watermark (codex `backfill_state`).
CREATE TABLE backfill_state (
    id                 INTEGER PRIMARY KEY CHECK (id = 1),
    status             TEXT NOT NULL CHECK (status IN ('pending', 'running', 'complete')),
    lease_owner        TEXT,
    last_watermark     TEXT,
    last_success_at_ms INTEGER,
    updated_at_ms      INTEGER NOT NULL
);
INSERT INTO backfill_state (id, status, updated_at_ms)
VALUES (1, 'pending', CAST(strftime('%s', 'now') AS INTEGER) * 1000);
```

- `projection_state.next_byte_offset` 是第一条未投影完整行的起点，`next_sequence` 是该行应有的 seq。
- `backfill_state.last_watermark` 是已处理的最后一个文件相对 sessions 根目录的路径；`updated_at_ms` 兼作租约心跳。

### 2.2 `session_history_1.sqlite` 的[历史迁移](../../../crates/state/migrations/history/)

```sql
-- History projection cursor for one Session file (codex `thread_history_projection_state`).
CREATE TABLE projection_state (
    session_id       TEXT PRIMARY KEY,
    path             TEXT NOT NULL,
    next_byte_offset INTEGER NOT NULL,
    next_sequence    INTEGER NOT NULL,
    source_id        TEXT NOT NULL DEFAULT '',
    source_size      INTEGER NOT NULL DEFAULT 0,
    source_mtime     TEXT NOT NULL DEFAULT ''
);

-- One durable operation from operation_started to operation_finished (codex `thread_turns`).
CREATE TABLE runs (
    session_id               TEXT NOT NULL REFERENCES projection_state(session_id) ON DELETE CASCADE,
    run_id                   TEXT NOT NULL,
    lane                     TEXT NOT NULL,
    operation                TEXT NOT NULL,
    status                   TEXT NOT NULL,
    error_json               TEXT,
    start_sequence           INTEGER NOT NULL,
    start_byte_offset        INTEGER NOT NULL,
    end_sequence             INTEGER,
    end_byte_offset          INTEGER,
    started_at_ms            INTEGER NOT NULL,
    completed_at_ms          INTEGER,
    duration_ms              INTEGER,
    first_user_entry_id      TEXT,
    final_assistant_entry_id TEXT,
    PRIMARY KEY (session_id, run_id)
);
-- Runs of one Session in log order.
CREATE UNIQUE INDEX idx_runs_page ON runs(session_id, start_sequence);
-- Find the newest open Run on the entry's lane.
CREATE INDEX idx_runs_open_lane ON runs(session_id, lane, start_sequence DESC)
    WHERE status = 'in_progress';
-- Finished Runs paged by completion position.
CREATE INDEX idx_runs_end_page ON runs(session_id, end_sequence, run_id)
    WHERE end_sequence IS NOT NULL;

-- One transcript entry (codex `thread_items`) plus the tree columns clawcode needs.
CREATE TABLE entries (
    session_id      TEXT NOT NULL REFERENCES projection_state(session_id) ON DELETE CASCADE,
    entry_id        TEXT NOT NULL,
    sequence        INTEGER NOT NULL,
    lane            TEXT NOT NULL,
    parent_id       TEXT,
    run_id          TEXT,
    kind            TEXT NOT NULL,
    message_type    TEXT,
    created_at_ms   INTEGER NOT NULL,
    started_at_ms   INTEGER,
    completed_at_ms INTEGER,
    entry_json      TEXT NOT NULL,
    PRIMARY KEY (session_id, entry_id)
);
-- Whole-tree paging in log order (SessionTreeSnapshot without ACP).
CREATE UNIQUE INDEX idx_entries_page ON entries(session_id, sequence);
-- Entries of one Run in log order.
CREATE INDEX idx_entries_by_run_page ON entries(session_id, run_id, sequence);
-- User message navigation (codex `idx_thread_items_user_messages`).
CREATE INDEX idx_entries_user_messages ON entries(session_id, sequence)
    WHERE message_type IN ('user', 'expanded_user');

-- Current leaf of each lane, required to rebuild SessionTreeSnapshot.
CREATE TABLE lanes (
    session_id TEXT NOT NULL REFERENCES projection_state(session_id) ON DELETE CASCADE,
    lane       TEXT NOT NULL,
    leaf_id    TEXT,
    PRIMARY KEY (session_id, lane)
);
```

列语义：

- `runs.operation`：`operation_started.operation` 原值（agent run 写 `agent`）；compaction run 的 payload 是 `CompactionOperationStarted { sourceLeafId, intent }`，取 `intent.kind`（`compaction`）；都没有则为 `unknown`。
- `runs.status`：未结束为 `in_progress`，结束后为 `operation_finished.status` 原值（agent run 写 completed / cancelled / failed）；compaction run 的 payload 是 `CompactionOperationFinished { outcome, error }`，取 `outcome` 原值（completed / aborted / failed）；`error_json` 为 `operation_finished.error`（agent 为字符串，compaction 为 `{code, message}`）。
- `entries.kind`：entry 类型（message / model_change / thinking_level_change / active_tools_change / compaction / branch_summary / custom）。
- `entries.message_type`：message entry 的 `content.type`（user / expanded_user / assistant / tool_result / extension / system），其他 entry 为 NULL。
- `entries.run_id`：追加时该 lane 上 `status = 'in_progress'` 的 run，没有则为 NULL。
- `entries.created_at_ms` 为存储时间戳；`started_at_ms` / `completed_at_ms` 取 message 的 `started_at_ms` / `ended_at_ms`；`entry_json` 为完整 entry，无损。
- 写入有父节点的 entry 时按会话和主键确认父节点已存在且序号更小。分支查询沿父链接按序号严格递减，遇到旧索引中的断链时返回出错的 entry ID。

### 2.3 索引使用规则

- SQLite 仅当查询 WHERE 蕴含部分索引的 WHERE 时才使用该索引。列表 SQL 必须字面写出 `has_user_message = 1` 与 `owner_session_id IS NULL`（与 Codex `0064_thread_listing_indexes.sql` 的注释要求相同），并由 7.1 的 `EXPLAIN QUERY PLAN` 测试守护。
- 列表使用 `(排序时间, id) < (?, ?)` 定位下一页的起点。复合索引同时处理同一时间戳下的 ID 排序。
- 投影器查找 lane 上未结束的 run 使用 `(session_id, run_id)` 主键前缀，每会话 run 数量小，不另建索引。

### 2.4 扩展方式

新增派生列时在原库执行 SQLx 迁移，先 `ALTER TABLE ... ADD COLUMN`，再 `UPDATE backfill_state SET status = 'pending', last_watermark = NULL`，下次启动 backfill 重新抽取。元数据库和历史库的文件版本由程序分别用 `STATE_DB_VERSION` 和 `SESSION_HISTORY_DB_VERSION` 定义。某个库的文件格式发生不兼容变更时，只递增该库的版本。新版本使用对应的新文件名，旧文件留给旧版本程序。

## 3 投影、触发、backfill 与读修复

### 3.1 `SessionProjector`

输入为 `session_id` 与规范 `path`，单次追赶步骤：

1. 先只读查询两库的路径、字节偏移和序号。两库记录的文件身份、大小和修改时间也与当前日志一致，且游标都位于文件末尾的完整行边界时直接返回。需要追赶时，每批依次在 state 和 session_history 上开启 `BEGIN IMMEDIATE`，重新读取游标并打开日志。投影、重建和删除均使用相同的加锁顺序。
2. 游标语义：行不存在视为 `next_byte_offset = 0`、`next_sequence = 0`。偏移 0 处必须是 header，应用后 `next_sequence` 变为 1；之后每条 mutation 的 seq 必须等于 `next_sequence`，应用后加一。
3. 从较小的字节偏移读取到本次追赶开始时的文件末尾。每批最多处理 128 行，读完一行后累计达到 256 KiB 也会提交。超大的单行仍可完整读取，半截尾行留待下次。
4. 任一库缺少文件版本信息、文件被替换或等长改写、路径发生变化、文件变短、游标落在行内、相同偏移对应不同序号，或下一行序号不匹配时，从日志起点重建该会话。再次遇到序号错误或发现 header 属于其他会话时返回错误。
5. 每条 mutation 只交给需要它的库。两个库都接受这一行后才完成元数据的保存点，每批分别提交两库的游标并释放写锁。

state 库应用规则：

| mutation | 更新 |
|---|---|
| header | upsert `sessions` 身份列、ownership、git、`app_version`，`updated_at_ms` 与 `recency_at_ms` 初值都取 `created_at_ms`（只有 header 的新会话也满足 NOT NULL）；插入 `projection_state` |
| name fact | `name` |
| message（user / expanded_user） | `has_user_message = 1`；`preview` 为空时写入文本（user 取各 text 块以 `\n` 连接，expanded_user 取 `expansion.invocation.original`，截断为前 200 个 Unicode 字符；纯图片消息文本为空则 `preview` 保持为空）；`recency_at_ms` |
| message（assistant） | `provider_id`、`model_id`；`tokens_used += total_tokens`（未上报按 0） |
| `model_change` / `thinking_level_change` | `provider_id`、`model_id` / `thinking_level` |
| custom entry，`extensionId == ProductIdentity::SLUG` 且 `customType == "session_mode"` | `session_mode` 取 `data.mode`（与 `crates/kernel/src/runtime/session/custom.rs` 的识别规则一致，第三方扩展的同名 customType 不生效） |
| 任意带时间戳的 entry / record | 每批提交时将 `updated_at_ms` 更新为该批最后一条被两库接受的时间戳。fact 无时间戳 |

history 库应用规则：

| mutation | 更新 |
|---|---|
| header | 插入 `projection_state` |
| entry | 插入 `entries`（`run_id` 取 lane 上未结束的 run）；`lanes` 叶子移到该 entry；run 内首条用户消息写 `first_user_entry_id`，每条 assistant 消息覆盖 `final_assistant_entry_id` |
| lane | upsert `lanes` |
| record `operation_started` | 插入 `runs`（`in_progress`、起始 seq / 偏移、`started_at_ms`） |
| record `operation_finished` | 回填 `status`、`error_json`、结束 seq / 偏移、`completed_at_ms`、`duration_ms` |
| 其他 record、label fact | 不入库（Codex 亦不存原始记录；内容保留在 JSONL） |

每条 message 从借用的 payload 解码一次 `AgentMessage`，元数据和历史共用解码结果。token 计数沿用 `ModelUsage` 的旧格式兼容逻辑。预览只收集前 200 个 Unicode 字符，历史表保留完整原始 entry 和扩展字段。

单会话投影失败（坏 JSON、未知 mutation、违反 CHECK）时在第一条无法应用的行处停止：该行的改动回滚，此前已应用的行与推进到该行起点的游标照常提交；记 warn，`refresh` 返回错误，不影响其他会话。下次追赶从该行重试。

### 3.2 触发时机

| 场景 | 方式 |
|---|---|
| create / create_owned / fork 之后 | kernel 在 async 调用点 await `SessionCatalog::refresh(id, path)` |
| `set_name_at`、live 会话改名之后 | 离线改名持久化后刷新。在线改名由 `SessionRename::apply` 在持久化之后、发送事件之前刷新，覆盖命令、扩展和子会话 |
| delete | `store_factory.delete` 之后 await `SessionCatalog::record_deleted(id)`，两库各删一行，外键级联 |
| `JsonlSessionStore::sync` 成功之后 | 调用 `SyncObserver::synced(session_id, path)`；`StateWorker` 经 unbounded channel 收到事件，按 ID 合并已排队事件后逐个追赶 |
| `JsonlStoreFactory::set_name_at` 之后 | 同样调用 `SyncObserver` |
| 外部读 history 之前 | `StateRuntime` 的 history 查询先对该会话追赶 |

关闭时先请求回填任务保存水位并释放本进程的租约，再等待 `StateWorker::flush`。投影遇到 SQLite 锁时自动退避重试，其他失败保留到下次同步或 flush 重试。flush 仍有失败时会返回未投影的会话数量和示例错误。

观察者的挂载位置：`JsonlStoreFactory` 新增 `sync_observer: Option<Arc<dyn SyncObserver>>` 字段（字段数 3，不需要 builder）及 `with_sync_observer` 方法；`JsonlSessionStore` 的 builder 新增同类型字段（`#[builder(default)]`）。factory 的 `create`、`create_owned`、`open`、`fork` 产出的每个 store 都携带该观察者，因此所有 `sync` 路径都会触发通知。`SyncObserver::synced` 只往 unbounded channel 发送事件，不阻塞，也不要求在 tokio runtime 内调用，在 transcript 持锁时调用是安全的。

### 3.3 Backfill

- app 在 kernel 构建成功后持有 `StartupBackfill` 任务，开始提供服务。
- `backfill_state.status = 'complete'` 则跳过；`running` 且 `updated_at_ms` 在租约期内则等待（另一进程在跑），每 1 秒（Codex `STARTUP_BACKFILL_POLL_INTERVAL`）只查询租约状态，直到对方完成或租约过期后由本进程接手；否则写入 `running` 与心跳抢占租约。
- 按相对路径排序遍历 `JsonlStoreFactory::session_paths()` 返回的路径，跳过已处理的水位。遇到相同 ID 时保留仍然存在的规范文件。原文件已移走时，本轮直接采纳新路径。
- 每处理 100 个文件保存水位。单个长日志投影期间也定时续租。正常退出时按持有者标记保存水位并释放租约，下一进程可立即接手。全部完成后写 `complete` 与 `last_success_at_ms`。
- 每次启动都先用 JSONL 提供目录查询。回填结束后逐个比较两库保存的文件身份、大小、修改时间、路径、偏移和序号。两库游标一致且到达文件末尾时仅检查文件元数据。任一库丢失或落后时按其游标追赶。
- 对账只清除仍指向已消失路径的记录，包括中断删除时单独遗留的历史记录。有效日志全部对账成功后，`StateRuntime` 才将目录标记为就绪。

### 3.4 读修复

- `header(id)` 和 `exists` 检查索引中的路径，仅在文件系统确认路径不存在时删除记录。权限等检查错误保留索引并回退到 store 扫描。
- 目录就绪时，列表直接读取 SQLite。会话身份写入、改名或删除的同步投影失败后，列表、摘要和子会话查询改读 JSONL。读取仍然走 JSONL 时，每次后台修复会覆盖全部待处理 ID，持续失败后至少等待一秒才会再次尝试，全部修复后恢复索引查询。
- 历史树先追赶日志，再读最新标题。entry 列表和 lane 叶节点共用一个历史库读事务。
- 已知局限：运行期间外部手工删除的文件，在下次按 ID 查询或下次启动对账时清理；运行期间外部拷入的文件在下次启动对账时可见。

### 3.5 kernel 调用点迁移

| 位置 | 现状 | 改为 |
|---|---|---|
| `crates/kernel/src/runtime/session.rs` `list_sessions` | `store_factory.list(cwd)` | async，`SessionCatalog::list(filter)`；ACP handler 已是 async |
| `crates/kernel/src/runtime/session.rs` `session_runtime` | `store_factory.exists` | async，`SessionCatalog::exists` |
| `crates/kernel/src/runtime/session/open.rs` resume | `headers(cwd).find(id)` | `SessionCatalog::header(id)` 并校验 cwd |
| `crates/kernel/src/runtime/session/ownership.rs` `resume_owner` | `headers(cwd)` 后内存逐级查找 | 逐级主键查询 |
| `crates/kernel/src/runtime/session/close.rs` `perform_delete` | `header(id)` 取 cwd；`headers(cwd).filter(owner)` 级联；`store_factory.delete` | `SessionCatalog::header(id)`；`SessionCatalog::children(owner)`；删除后 await `record_deleted` |
| `crates/kernel/src/runtime/session.rs` 注册回滚删除 | `store_factory.delete` | 删除后 await `record_deleted` |
| `crates/kernel/src/runtime/subagent/roster.rs` `with_children` | 同步、持 std `Mutex` 的懒加载，首次调用时在闭包内 `headers(cwd)`；有约 10 个同步调用方（`subagent/tools.rs`、`subagent/path.rs`、`roster.rs`） | 改为在 owner Session 构建或恢复时（async）经 `SessionCatalog::children` 预先填充 roster 缓存；`with_children` 只读缓存，签名与调用方不变，原有扫描作为缓存未填充时的回退 |
| `crates/kernel/src/runtime/subagent/mail.rs`、`crates/kernel/src/runtime/session/rename.rs` | `header(id)` | `SessionCatalog::header(id)` |
| `crates/kernel/src/runtime/extension/host.rs` `create_session` / `switch_session` / `fork_session` | `list_sessions(None)` 后 `.find(id)`：为取一条摘要列出全部会话 | `SessionCatalog::summary(id)` 按主键取一条 |
| `crates/acp/src/extension.rs`、`crates/acp/src/server.rs` 中的 `session_runtime` 调用 | 同步调用 | 均位于 async 上下文，改为 `.await` |
| `crates/kernel/src/runtime/session/open.rs`、`ownership.rs`、`fork.rs` 创建 | 仅写 JSONL | 写后 await `SessionCatalog::refresh` |

`SessionCatalog` 持有 `Arc<dyn StoreFactory>` 与 `Option<Arc<StateRuntime>>`；`state` 为 `None` 或 backfill 未完成时委托给原有 `StoreFactory::list` / `headers` / `header` / `exists`，这些 store 方法保持不变作为回退。ACP `session/list` 返回的字段不变，仍按修改时间倒序；`modifiedAtMs` 由文件 mtime 改为 `updated_at_ms`（最后一条带时间戳的 mutation）。唯一可见差异：name fact 没有时间戳，单纯改名不再把会话顶到列表最前。

### 3.6 git 与 `app_version` 采集

- kernel 在创建会话（含 owned 与 fork）前于 cwd 执行 `git rev-parse HEAD --abbrev-ref HEAD`、`git remote get-url origin`，使用 `tokio::process` 并设置超时，任一失败则对应字段为 None，非 git 目录整体为 None。
- detached HEAD 的分支留空。origin 保留 remote-helper 前缀和 SSH 约定的 `git` 用户，去除其他 userinfo。HTTP(S) 还会移除完整 query 和 fragment。含空白的 helper 命令行整体丢弃。清理发生在新会话写入 header 之前。
- `ProductIdentity` 新增 `VERSION` 常量（`env!("CARGO_PKG_VERSION")`）。
- protocol 新增 `SessionGitInfo { sha, branch, origin_url }` 与 `SessionProvenance { git, app_version }`；`SessionCreateOptions` 新增 `provenance: Option<SessionProvenance>`。
- store `V4Header` 新增可选字段 `git`（camelCase：`sha`、`branch`、`originUrl`）与 `appVersion`，`skip_serializing_if = "Option::is_none"`；不含这些字段的旧 header 照常读取。

### 3.7 app 装配

`ApplicationFactory::build` 改为 async：`data_root` 在 `build` 内部解析，`StateRuntime::init` 是 async，而 `SyncObserver` 必须在 `build` 内构造 `JsonlStoreFactory` 时装上。现有调用方只有 `crates/app/src/main.rs` 的 async `run()` 和 `crates/app/tests/extensions.rs`。`build` 按以下顺序装配：

1. 按进程启动目录将配置的 `data_root` 转为绝对路径，两个库都记录可跨工作目录读取的日志位置。
2. `StateRuntime::init(SqliteConfig)`；失败则 warn 并得到 `None`。
3. 有 runtime 时创建 `StateWorker`，用 `with_sync_observer` 装到 `JsonlStoreFactory` 上。
4. 通过 `KernelFactory` 新增的 `state: Option<Arc<StateRuntime>>`（`#[builder(default)]`）把 runtime 交给 kernel，由 kernel 构造 `SessionCatalog`。
5. Kernel 构建成功后由 `Application` 持有回填任务，关闭服务时等待任务退出。

kernel 测试构造 `KernelFactory` 时不传 `state`，`SessionCatalog` 退回扫描，现有测试不受影响。

## 4 错误处理

原则：JSONL 是事实来源，SQLite 的任何故障都不能让 kernel 操作失败。

| 情况 | 处理 |
|---|---|
| `StateRuntime::init` 打开或迁移失败 | `tracing::warn!` 记录路径与错误，返回 `None`，全程回退扫描 |
| 库文件损坏（SQLITE_CORRUPT / SQLITE_NOTADB） | 改名为 `<文件名>.corrupt-<毫秒时间戳>` 后新建，backfill 重建 |
| 单会话投影失败 | 停在第一条无法应用的行，提交此前的行与游标，warn 记录会话 ID、路径、字节偏移 |
| 创建 / 改名后的 `refresh` 失败 | warn，kernel 操作照常成功，由读修复或下次 sync 补齐 |
| SQLITE_BUSY | `busy_timeout = 5s`；启动阶段的迁移与 backfill 状态初始化按 Codex `retry_on_busy` 重试 |
| 两进程同时迁移新库 | 后到者的 `CREATE TABLE` 报已存在、事务回滚；迁移错误与 SQLITE_BUSY 一样按 100ms / 200ms 重试，重试时迁移已记录为已应用 |
| `StateWorker` channel 关闭 | 处理完剩余事件后退出 |

初始化在迁移前设置持久化的 `auto_vacuum = INCREMENTAL` 和 `journal_mode = WAL`。后续池连接使用 `synchronous = NORMAL`，连接数上限为 5。迁移使用 `ignore_missing = true`，使旧版本二进制可打开被新版本迁移过的库。

日志只记录初始化结果、backfill 开始 / 检查点 / 完成、因校验失败触发的整会话重投影与各错误路径；不按行或按 mutation 记录，不记录消息正文。

## 5 配置与依赖

- 不新增配置项。两个库文件位于现有 `session_persistence.data_home` 根目录（默认 `~/.local/share/clawcode/`），与 `sessions/`、`credentials` 并列。Codex 的 `sqlite_home` 覆盖项不引入，`data_home` 已可配置。
- 根 `Cargo.toml` 新增 `state` workspace 成员与依赖声明、`# database` 分组中的 `sqlx`（`0.9`，`default-features = false`，features：`json`、`macros`、`migrate`、`runtime-tokio`、`sqlite-bundled`）与 `libsqlite3-sys`，具体布局见 6.1。
- **SQLite 版本下限照搬 Codex**：crates.io 上的 sqlx 0.9.0 绑定 libsqlite3-sys 0.37，自带的 SQLite 是 3.51.3。Codex 在 commit 2728f99126 中把 SQLite 升到 3.54，理由是修复 `RUSTSEC-2026-0459`（打开带 WAL 的数据库时触发），同时保留 WAL-reset 损坏的修复。本设计让 server 与 GUI 多进程共享 WAL 库，正处在这个风险范围内。做法与 Codex 完全相同：
  - 根 `Cargo.toml` 用 `[patch.crates-io]` 把 `sqlx-core`、`sqlx-sqlite` 指向 Codex 审过的同一修订版 `https://github.com/transact-rs/sqlx` rev `10462fa4414c6c7f1ed1d39bb31678e3b726878c`；
  - 依赖 `libsqlite3-sys = { version = "0.39.1", default-features = false }`；
  - `crates/state/src/lib.rs` 加编译期断言 `libsqlite3_sys::SQLITE_VERSION_NUMBER >= 3_054_000`。
  - 等 sqlx 正式发布包含 transact-rs/sqlx#4502 的版本后，再去掉 patch。
- 只使用运行时检查的 `sqlx::query` / `QueryBuilder`，与 Codex 相同，不使用编译期 `query!`，因此不需要 `DATABASE_URL` 或 offline 数据。

## 6 代码风格约束

### 6.1 Cargo 依赖表（`AGENTS.md` 第 11、12 条）

- 每个依赖表按语义分组，分组名与根 `Cargo.toml` 一致；相对路径的 workspace crate 分组放第一；组间一个空行并以分类注释开头；组内按字母序。
- 所有依赖的路径或版本只在根 `[workspace.dependencies]` 声明，含内部 crate；子 crate 一律 `{ workspace = true }`，本地只允许追加 crate 专属的 features、optional、default-features 或 target 条件。

根 `Cargo.toml` 增量（`state` 按字母序位于 `skill` 与 `store` 之间；`# database` 分组位于 `# fs` 之后）：

```toml
[workspace]
members = [
  # ...
  "crates/skill",
  "crates/state",
  "crates/store",
  # ...
]

[workspace.dependencies]
# workspace crates
# ...
skill = { path = "crates/skill" }
state = { path = "crates/state" }
store = { path = "crates/store" }
# ...

# fs
# ...

# database
libsqlite3-sys = { version = "0.39.1", default-features = false }
sqlx = { version = "0.9", default-features = false, features = ["json", "macros", "migrate", "runtime-tokio", "sqlite-bundled"] }

[patch.crates-io]
# SQLx 0.9.0 excludes libsqlite3-sys 0.39; keep driver and core on the revision Codex reviewed
# until a release includes transact-rs/sqlx#4502.
sqlx-core = { git = "https://github.com/transact-rs/sqlx", rev = "10462fa4414c6c7f1ed1d39bb31678e3b726878c" }
sqlx-sqlite = { git = "https://github.com/transact-rs/sqlx", rev = "10462fa4414c6c7f1ed1d39bb31678e3b726878c" }
```

`crates/state/Cargo.toml`：

```toml
[package]
name = "state"
edition.workspace = true
version.workspace = true
description = "SQLite session state and history mirrored from JSONL sessions"

[lints]
workspace = true

[dependencies]
# workspace crates
protocol = { workspace = true }
store = { workspace = true }

# error
thiserror = { workspace = true }

# tracing & log
tracing = { workspace = true }

# async
tokio = { workspace = true, features = ["fs", "sync", "time"] }

# serde
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }

# database
libsqlite3-sys = { workspace = true }
sqlx = { workspace = true }

# builder
typed-builder = { workspace = true }

[dev-dependencies]
# fs
tempfile = { workspace = true }
```

`crates/kernel/Cargo.toml` 与 `crates/app/Cargo.toml` 在 `# workspace crates` 分组内按字母序加入 `state = { workspace = true }`。

### 6.2 SQL 迁移文件

- 文件名 `NNNN_<snake_case 描述>.sql`，四位零填充，从 `0001` 起连续编号。
- 已发布的迁移文件不得修改：sqlx 按校验和验证已应用的迁移，修改会导致已有库启动失败。任何变更都新增迁移。
- 每个迁移以英文注释说明目的；每个 `CREATE TABLE` 前注释其职责与 Codex 对应表；每个 `CREATE INDEX` 前注释它服务的查询（同 Codex 0064 的写法）。
- 关键字大写，四空格缩进，一列一行；表内列按「身份、时间、业务、关系」顺序分组。
- 索引命名 `idx_<表>_<列或用途>`；部分索引的谓词与查询 SQL 字面一致。

### 6.3 Rust 代码（`AGENTS.md` 其余相关条款）

- 第 1 条：新增或修改的非平凡逻辑写英文注释，每个新增函数写函数级注释。
- 第 5 条：超过 3 个字段的结构体使用 `typed-builder` 并以 builder 构造，`Option` 字段加 `#[builder(default)]`。`SessionCreateOptions` 新增 `provenance` 后达到 4 个字段，须改为 builder，并迁移全部构造点（含测试）；`parent_session_id`、`provenance` 加 `#[builder(default)]`。
- 第 6 条：克隆 `Arc<T>` 字段一律写 `Arc::clone(&self.field)`。
- 第 8 条：不增加单参数、参数为项目类型的自由辅助函数，改为该类型的关联方法或 trait 实现（如行到 `SessionSummary` 的转换用 `TryFrom<SessionRow>`）。
- 第 10 条：测试只放在 crate 级 `tests/` 或 `#[cfg(test)] mod tests`，生产代码不含仅为测试存在的字段、函数或模块。
- 第 13、14 条：日志位置见第 4 节；一律使用完全限定的 `tracing::info!()` / `tracing::warn!()`，用普通格式化参数，不用结构化字段语法。
- 第 15 条：操作组织在持有上下文的结构体上——`SqliteConfig` 负责路径与连接，`StateRuntime` 持有连接池并提供查询，`SessionProjector` 借用 `&StateRuntime` 与单会话输入完成一次追赶，`SessionCatalog` 持有回退所需的 store 与可选 runtime；不在自由函数链之间传递同一组上下文，不建无状态工具结构体。
- 第 16 条：本 spec 与后续 plan 引用仓库内文档时使用 `./` / `../` 相对路径。
- SQL 参数一律绑定（`?` / `push_bind`），不把任何值格式化进 SQL 字符串。

## 7 测试

遵循 TDD，先写失败测试再实现。

### 7.1 `crates/state/tests/`

- 迁移：新库建表成功；库中存在本版本未知的更新迁移记录时仍可打开（`ignore_missing`）。
- 投影正确性：用真实 `JsonlStoreFactory` 写入覆盖用户消息、带 usage 的 assistant 消息、tool_result、`model_change`、`thinking_level_change`、`session_mode`、name fact、额外 lane、compaction 操作的会话；`refresh` 后断言 `sessions` 各列与 `entries`、`lanes`、`runs` 各行；由 history 库还原的树与 store 内存中的 `entries()`、`lane()` 一致。
- 增量：追加后再次 `refresh` 只新增行，`tokens_used` 不重复累加。
- 半截尾行不投影，补全后投影。
- 文件截断或重写（含游标落在行中间）触发整会话重投影；header 属于其他会话时拒绝投影。
- 并发写者：两个 `StateRuntime` 实例同时追赶同一会话，`tokens_used` 正确、无主键冲突。
- backfill：租约有效时跳过、按水位续跑、同 ID 异路径跳过、最终 `complete`；`sync_logs` 在他进程完成后就绪；对账收录新文件、追上增长的文件、删除文件已消失的行；两个进程同时初始化新库都成功。
- list：三种排序键、cwd、visible、interactive、provider 过滤与 anchor 分页结果正确；`EXPLAIN QUERY PLAN` 断言主要列表查询命中预期索引。
- 删除：两库清理干净，子表级联删除。
- 损坏：写入垃圾文件后备份并重建。

### 7.2 `crates/kernel/tests/`

- `state` 为 `None` 与 backfill 未完成时，`SessionCatalog` 回退扫描，结果与现状一致。
- 有库时：创建后立即可 list 与查到 children；改名后 list 立即显示新名；删除后不可见；经库沿 owner 链恢复会话正常。
- git 采集：临时 git 仓库中创建的会话 header 含 git 字段；非 git 目录为 None；origin 中的凭据被去除，detached HEAD 不写分支。
- roster：owner 恢复后 `with_children` 首次调用直接命中由库预填充的缓存，不触发 `headers(cwd)` 扫描。
- 扩展宿主：`create_session` / `switch_session` / `fork_session` 返回的摘要与新建会话一致。
- `session_mode`：第三方扩展写入同名 customType 的 custom entry 不改变 `sessions.session_mode`。

### 7.3 `crates/store/tests/`

- `MutationReader` 从任意完整行偏移开始解析正确，半截尾行不产出。
- 含新增可选字段的 header 往返正确；不含这些字段的旧 header 可读。
- `SyncObserver` 在 `sync` 与 `set_name_at` 成功后被调用，失败时不调用。

### 7.4 WebUI 端到端

按 `AGENTS.md` 第 9 条，在生产 `app` 后端上运行现有 WebUI e2e，确认 `session/list` 等 ACP 行为不变。
