# 实现计划：Store trait 抽象 + Subagent 持久化

## Context

基于 spec `docs/superpowers/specs/subagent-persistence-and-store-trait.md`，将 `kernel::persistence` 模块搬出为独立 `store` crate，抽象出 `SessionStore` + `SessionRecorder` trait，并实现 subagent spawn/close/load 的持久化与恢复。

## 执行顺序

分 12 步，严格按序执行，每步有编译验证点。

---

### Step 1: 创建 `crates/store/` 目录和 `Cargo.toml`

- `mkdir -p crates/store/src`
- 写 `crates/store/Cargo.toml`，依赖：`protocol`, `tokio`, `serde`, `serde_json`, `chrono`, `typed-builder`, `async-trait`, `tracing`；dev：`tempfile`

**验证**: `cargo metadata` 不报错。

---

### Step 2: 写 `crates/store/src/record.rs`

**从哪搬**: `crates/kernel/src/persistence/record.rs`（逐字复制）

**改动**:
- 所有 `pub(crate)` 改为 `pub`
- `CreateSessionParams` 新增三个字段（`parent_session_id`, `agent_role`, `agent_nickname`）

**验证**: `cargo build -p store`

---

### Step 3: 写 `crates/store/src/recorder.rs`

**从哪搬**: `crates/kernel/src/persistence/recorder.rs`

**改动**:
- 定义 `pub trait SessionRecorder: Send + Sync`（`append`, `flush`）
- 原 struct 改名为 `FileSessionRecorder`，实现 trait
- `pub(crate)` → `pub`

**验证**: `cargo build -p store`

---

### Step 4: 写 `crates/store/src/manifest.rs`

**从哪搬**: `crates/kernel/src/persistence/manifest.rs`

**改动**:
- `pub(crate)` → `pub`
- `active_manifest_record` 增加 `parent_session_id` 参数
- 导入改为引用本 crate

**验证**: `cargo build -p store`

---

### Step 5: 写 `crates/store/src/replay.rs`

**从哪搬**: `crates/kernel/src/persistence/replay.rs`

**改动**:
- `ReplayedSession` 增加字段 `pub agent_edges: Vec<AgentEdgeRecord>`
- `replay_session_file` 中对 `AgentEdge` 从忽略改为收集
- `pub(crate)` → `pub`

**验证**: `cargo build -p store`

---

### Step 6: 写 `crates/store/src/traits.rs`

**新建**。定义 `SessionStore` trait：

```rust
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create_session(&self, params: CreateSessionParams)
        -> io::Result<Option<Box<dyn SessionRecorder>>>;
    fn load_session(&self, session_id: &SessionId)
        -> io::Result<Option<(ReplayedSession, Box<dyn SessionRecorder>)>>;
    async fn close_session(&self, session_id: &SessionId, recorder: Option<&dyn SessionRecorder>)
        -> io::Result<()>;
    fn list_sessions(&self, cwd: Option<&Path>) -> io::Result<Vec<SessionInfo>>;
}
```

**验证**: `cargo build -p store`

---

### Step 7: 写 `crates/store/src/file_store.rs`

**从哪搬**: `crates/kernel/src/persistence/store.rs`

**改动**:
- 改名为 `FileSessionStore`
- `impl SessionStore for FileSessionStore`
- `create_session` 中 `SessionMetaRecord` 填入新增字段
- 测试保留（更新 struct 名）

**验证**: `cargo build -p store && cargo test -p store`

---

### Step 8: 写 `crates/store/src/lib.rs`

`pub mod` + `pub use` 导出所有公开类型。

**验证**: `cargo build -p store && cargo test -p store`

---

### Step 9: 修改 `crates/kernel/`

#### 9a. 更新 `Cargo.toml` — 新增 `store = { path = "../store" }`

#### 9b. 更新 `turn.rs`
- 导入改为 `use store::...`
- `TurnContext.recorder`: `Option<Arc<dyn SessionRecorder>>`

#### 9c. 更新 `session.rs`
- 导入改为 `use store::...`
- `Thread.recorder` / `Session.recorder` / `spawn_thread` 参数: `Option<Arc<dyn SessionRecorder>>`

#### 9d. 更新 `agent/registry.rs`
- 新增 `restore_agent(...)` 方法（跳过 slot counting）

#### 9e. 更新 `agent/control.rs`
- 新增 `session_store` + `recorders` 字段
- 新增 `register_recorder` / `unregister_recorder`
- `spawn()` 中写入 `AgentEdgeRecord(Open)`
- `close_agent()` 中写入 `AgentEdgeRecord(Closed)` + 关闭 manifest

#### 9f. 更新 `lib.rs`
- `Kernel.session_store`: `Arc<dyn SessionStore>`
- `new_session()` 中注册 recorder
- `load_session()` 新增 subagent 树恢复
- 新增 `restore_subagent_tree` 递归方法

---

### Step 10: 删除 `crates/kernel/src/persistence/`

```bash
rm -rf crates/kernel/src/persistence/
```
并从 `lib.rs` 中删除 `pub(crate) mod persistence;`。

**验证**: `cargo build`

---

### Step 11: 全量编译 + 修复

`cargo build`，处理遗留的导入路径问题。

---

### Step 12: 运行测试

```bash
cargo test -p store
cargo test -p kernel
cargo test
```

---

## 涉及的核心文件

| 文件 | 操作 |
|---|---|
| `crates/kernel/src/persistence/record.rs` | → 搬到 `crates/store/src/record.rs` |
| `crates/kernel/src/persistence/recorder.rs` | → 搬到 `crates/store/src/recorder.rs` |
| `crates/kernel/src/persistence/manifest.rs` | → 搬到 `crates/store/src/manifest.rs` |
| `crates/kernel/src/persistence/replay.rs` | → 搬到 `crates/store/src/replay.rs` |
| `crates/kernel/src/persistence/store.rs` | → 搬到 `crates/store/src/file_store.rs` |
| `crates/kernel/src/persistence/mod.rs` | 删除 |
| `crates/kernel/src/lib.rs` | 修改：导入 + restore_subagent_tree |
| `crates/kernel/src/session.rs` | 修改：类型变更 |
| `crates/kernel/src/turn.rs` | 修改：类型变更 |
| `crates/kernel/src/agent/control.rs` | 修改：持久化 + store 注入 |
| `crates/kernel/src/agent/registry.rs` | 修改：新增 restore_agent |
| `crates/kernel/Cargo.toml` | 修改：新增 store 依赖 |
