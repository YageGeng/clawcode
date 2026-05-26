# TUI Subagent 列表与上下文切换 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 TUI 中通过 `/agent` 打开输入框下方的 agent picker，展示 `Main [default]` 与 subagent 列表，并允许在 root/subagent 的 ACP session 上下文之间切换。

**Architecture:** 保持 `AppState` 单 session reducer 语义不变，在 TUI 上层新增 session router，按 ACP `session_id` 分发 transcript notification。ACP schema 不扩展标准字段，只用 `ToolCallUpdate._meta.clawcode.subagents` 传递 UI agent list metadata；`_meta` 只维护列表，不承载 transcript。

**Tech Stack:** Rust, ACP `agent-client-protocol`, ratatui, crossterm, typed-builder, serde/serde_json, existing kernel `AgentControl`/`AgentRegistry`, existing TUI `AppState`/`ViewState`/`Composer`。

---

## 前置约束

- 本计划基于已 review 通过的 spec：`docs/superpowers/specs/2026-05-26-tui-subagent-switching-design.md`。
- Plan 和执行说明使用中文；代码注释必须使用英文。
- 新增函数必须有函数级英文注释；非平凡逻辑必须有英文注释。
- 字段数超过 3 的新增 struct 必须使用 `typed-builder`，`Option` 字段使用 `#[builder(default, setter(strip_option))]`。
- `Arc<T>` 字段 clone 必须写成 `Arc::clone(&self.field)`。
- 不修改 ACP 标准 schema，不新增除 `/agent` 之外的 subagent 专用 slash command。
- 不解析 model-facing `list_agents` 文本作为 UI 数据源。
- 不执行 `git commit`，除非用户明确授权；实现完成后只报告验证结果和建议提交范围。
- 在 `/home/isbest/Documents/WorkSpace/clawcode` 中运行验证命令时使用 `rtk` 前缀。

## 目标文件结构

### 新增文件

- `crates/protocol/src/agent_ui.rs`
  - 定义跨 kernel/ACP 共享的 UI-only agent metadata：`AgentUiMetadata`、`AgentUiMetadataPatch`、`AgentUiEventKind`。
  - 只表达 UI 列表所需字段，不包含 transcript。

- `crates/tui/src/ui/agent_navigation.rs`
  - 定义 `AgentNavigationState`、`AgentPickerEntry`、`AgentPickerStatus`。
  - 负责 first-seen 顺序、root 第一项、label/status 显示、picker 上下移动。

- `crates/tui/src/ui/agent_picker.rs`
  - 定义 `AgentPickerPanelState` 与 picker render helpers。
  - 负责输入框下方列表的行生成、截断和焦点/选中渲染。

- `crates/tui/src/ui/session_router.rs`
  - 定义 `SessionRouterState`、`PromptTaskState`、`SelectAgentSessionError`。
  - 负责 `HashMap<SessionId, AppState>`、active session、per-session `ViewState`/`Composer` snapshot、metadata-only notification 拦截。

### 修改文件

- `crates/protocol/src/lib.rs`
  - `pub mod agent_ui;` 并 re-export UI metadata 类型。

- `crates/protocol/src/kernel.rs`
  - 在 `AgentKernel` trait 上增加 `agent_ui_snapshot(&self, root_session_id: &SessionId)`，供 ACP adapter 发送 snapshot。

- `crates/kernel/src/agent/control.rs`
  - 增加 `AgentControl::agent_ui_snapshot(root_session_id)`，从 `AgentRegistry` 生成包含 root 的 UI metadata。
  - 在 spawn/status/close 路径补齐可转换为 UI metadata 的状态。

- `crates/kernel/src/agent/registry.rs`
  - 增加 root + registered agent snapshot 的只读方法，避免 ACP adapter 直接访问 registry 内部细节。

- `crates/kernel/src/lib.rs`
  - 实现 `AgentKernel::agent_ui_snapshot`。
  - 在必要时发射 `Event::AgentSpawned` / `Event::AgentStatusChange`，让 ACP adapter 可以实时 upsert/status。

- `crates/acp/src/agent.rs`
  - 增加 `_meta.clawcode.subagents` 构造与发送函数。
  - `handle_new_session` / `handle_load_session` 发送 snapshot。
  - `handle_prompt` 将 `AgentSpawned` / `AgentStatusChange` 转换为 metadata-only `ToolCallUpdate`。
  - Fake kernel test double 补齐新 trait 方法。

- `crates/tui/src/acp/client.rs`
  - `AppEvent::PromptFinished` / `PromptFailed` 携带 `SessionId`。
  - 增加 `OpenAgentPicker` / `SelectAgentSession(SessionId)` 本地事件，或直接在 `app.rs` 中调用 router 方法；推荐事件化，便于测试。

- `crates/tui/src/app.rs`
  - 用 `SessionRouterState` 替换裸 `AppState`。
  - `/agent` 命令打开 picker。
  - prompt submit 使用 `router.active_session_id()`。
  - prompt task 记录所属 session，完成/失败只落回所属 session。
  - 方向键在 picker 聚焦时交给 picker，未聚焦时保持现有 scroll/composer 行为。

- `crates/tui/src/ui/layout.rs`
  - `FrameRows` 增加 `agent_picker: Option<Rect>` 或固定 `agent_picker: Rect`。
  - 根据 picker 可见状态给输入框下方预留 1 到 5 行。

- `crates/tui/src/ui/render.rs`
  - render 入口读取 router active state/view/composer。
  - composer 下方渲染 inline agent picker。

- `crates/tui/src/ui/status.rs`
  - bottom status 增加 active agent label，例如 `agent: Main [default]` 或 `agent: finder [worker]`。

- `crates/tui/src/ui/mod.rs`
  - re-export 新增 UI 模块。

## Task 1: 定义 UI Agent Metadata 契约

**Files:**
- Create: `crates/protocol/src/agent_ui.rs`
- Modify: `crates/protocol/src/lib.rs`
- Modify: `crates/protocol/src/kernel.rs`
- Test: `crates/protocol/src/agent_ui.rs`

- [ ] **Step 1: 写 metadata 序列化失败测试**

在 `crates/protocol/src/agent_ui.rs` 新增测试驱动目标结构。先写测试再实现类型：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentPath, AgentStatus, SessionId};

    /// Verifies UI agent metadata serializes with stable snake_case fields.
    #[test]
    fn agent_ui_metadata_serializes_root_and_child_fields() {
        let entry = AgentUiMetadata::builder()
            .session_id(SessionId("child-session".to_string()))
            .parent_session_id(SessionId("root-session".to_string()))
            .agent_path(AgentPath::root().join("inspect"))
            .nickname("finder".to_string())
            .role("worker".to_string())
            .status(AgentStatus::Running)
            .is_root(false)
            .build();

        let value = serde_json::to_value(entry).expect("metadata should serialize");

        assert_eq!(value["session_id"], "child-session");
        assert_eq!(value["parent_session_id"], "root-session");
        assert_eq!(value["agent_path"], "/root/inspect");
        assert_eq!(value["nickname"], "finder");
        assert_eq!(value["role"], "worker");
        assert_eq!(value["status"], "running");
        assert_eq!(value["is_root"], false);
    }
}
```

- [ ] **Step 2: 运行失败测试**

Run:

```bash
rtk cargo test -p protocol agent_ui_metadata_serializes_root_and_child_fields
```

Expected: 编译失败，提示 `AgentUiMetadata` 未定义。

- [ ] **Step 3: 实现 metadata 类型**

在 `crates/protocol/src/agent_ui.rs` 添加：

```rust
//! UI-only agent metadata used by ACP `_meta` extensions.

use serde::{Deserialize, Serialize};

use crate::{AgentPath, AgentStatus, SessionId};

/// Metadata required by frontends to show and switch agent sessions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct AgentUiMetadata {
    /// ACP/kernel session id for the represented agent.
    pub session_id: SessionId,
    /// Parent session id, absent only for the root agent.
    #[builder(default, setter(strip_option))]
    pub parent_session_id: Option<SessionId>,
    /// Canonical agent path used by the runtime.
    pub agent_path: AgentPath,
    /// Human-friendly nickname shown in the picker.
    #[builder(default, setter(strip_option))]
    pub nickname: Option<String>,
    /// Role name shown next to the nickname.
    #[builder(default, setter(strip_option))]
    pub role: Option<String>,
    /// Latest known runtime status.
    pub status: AgentStatus,
    /// True when this entry represents the main/root agent.
    pub is_root: bool,
}

/// Kind of UI metadata patch carried through ACP `_meta`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUiEventKind {
    Snapshot,
    Upsert,
    Status,
}

/// Versioned UI metadata patch sent to clients.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct AgentUiMetadataPatch {
    /// Extension payload version.
    pub version: u32,
    /// Patch semantics for the contained entries.
    pub event: AgentUiEventKind,
    /// Agent entries included in this patch.
    pub agents: Vec<AgentUiMetadata>,
}
```

- [ ] **Step 4: re-export 类型并扩展 trait**

在 `crates/protocol/src/lib.rs` 加：

```rust
pub mod agent_ui;
pub use agent_ui::*;
```

在 `crates/protocol/src/kernel.rs` trait 中增加：

```rust
    /// Return UI-only agent metadata for a root session tree.
    async fn agent_ui_snapshot(
        &self,
        root_session_id: &SessionId,
    ) -> Result<Vec<crate::agent_ui::AgentUiMetadata>, KernelError>;
```

- [ ] **Step 5: 运行协议测试**

Run:

```bash
rtk cargo test -p protocol agent_ui
```

Expected: PASS。

## Task 2: Kernel 暴露 root + subagent snapshot

**Files:**
- Modify: `crates/kernel/src/agent/registry.rs`
- Modify: `crates/kernel/src/agent/control.rs`
- Modify: `crates/kernel/src/lib.rs`
- Test: `crates/kernel/src/agent/control.rs`

- [ ] **Step 1: 写 snapshot 行为测试**

在 `crates/kernel/src/agent/control.rs` tests 中新增测试，复用现有 `agent_control_no_persistence()` 或测试 fixture：

```rust
/// Verifies UI snapshots always include the root session as the first entry.
#[tokio::test]
async fn agent_ui_snapshot_includes_root_first() {
    let control = agent_control_no_persistence();
    let root_id = SessionId("root-session".to_string());
    control.registry.register_root_thread(root_id.clone());

    let snapshot = control.agent_ui_snapshot(&root_id).await;

    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].session_id, root_id);
    assert!(snapshot[0].is_root);
    assert_eq!(snapshot[0].agent_path, AgentPath::root());
    assert_eq!(snapshot[0].status, AgentStatus::Running);
}

/// Verifies UI snapshots include registered children with session ids and parent ids.
#[tokio::test]
async fn agent_ui_snapshot_includes_registered_child_metadata() {
    let control = agent_control_no_persistence();
    let root_id = SessionId("root-session".to_string());
    let child_id = SessionId("child-session".to_string());
    control.registry.register_root_thread(root_id.clone());
    control
        .registry
        .restore_agent(
            child_id.clone(),
            AgentPath::root().join("inspect"),
            Some("finder".to_string()),
            Some("worker".to_string()),
            Some(root_id.clone()),
        )
        .expect("restore child");

    let snapshot = control.agent_ui_snapshot(&root_id).await;

    assert_eq!(snapshot.len(), 2);
    assert_eq!(snapshot[0].session_id, root_id);
    assert_eq!(snapshot[1].session_id, child_id);
    assert_eq!(snapshot[1].parent_session_id.as_ref(), Some(&root_id));
    assert_eq!(snapshot[1].nickname.as_deref(), Some("finder"));
    assert_eq!(snapshot[1].role.as_deref(), Some("worker"));
}
```

- [ ] **Step 2: 运行失败测试**

Run:

```bash
rtk cargo test -p kernel agent_ui_snapshot
```

Expected: 编译失败，提示 `agent_ui_snapshot` 未定义。

- [ ] **Step 3: 给 registry 增加只读 snapshot 方法**

在 `AgentRegistry` impl 中增加：

```rust
    /// Return registered agent metadata for UI snapshots, including final agents.
    pub(crate) fn registered_agent_metadata(&self) -> Vec<AgentMetadata> {
        self.active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .agent_tree
            .values()
            .filter(|metadata| metadata.agent_id.is_some())
            .cloned()
            .collect()
    }
```

注意：这是 struct 方法，不新建 free helper；调用方负责 root first 排序。

- [ ] **Step 4: 实现 `AgentControl::agent_ui_snapshot`**

在 `AgentControl` impl 中增加：

```rust
    /// Build UI-only metadata for the root session tree.
    pub async fn agent_ui_snapshot(&self, root_session_id: &SessionId) -> Vec<protocol::AgentUiMetadata> {
        let mut entries = self
            .registry
            .registered_agent_metadata()
            .into_iter()
            .filter_map(protocol::AgentUiMetadata::try_from)
            .collect::<Vec<_>>();

        // The picker must always offer the main agent as the first switch target.
        if !entries.iter().any(|entry| entry.session_id == *root_session_id && entry.is_root) {
            entries.push(
                protocol::AgentUiMetadata::builder()
                    .session_id(root_session_id.clone())
                    .agent_path(AgentPath::root())
                    .status(AgentStatus::Running)
                    .is_root(true)
                    .build(),
            );
        }

        entries.sort_by(|left, right| match (left.is_root, right.is_root) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => left.agent_path.as_str().cmp(right.agent_path.as_str()),
        });
        entries
    }
```

同时为 `protocol::AgentUiMetadata` 增加 `TryFrom<AgentMetadata>`，优先实现为关联转换而不是 helper 函数：

```rust
impl TryFrom<AgentMetadata> for protocol::AgentUiMetadata {
    type Error = ();

    /// Convert registry metadata into UI metadata when it has a session id and path.
    fn try_from(metadata: AgentMetadata) -> Result<Self, Self::Error> {
        let session_id = metadata.agent_id.ok_or(())?;
        let agent_path = metadata.agent_path.ok_or(())?;
        let is_root = agent_path.is_root();
        let mut builder = protocol::AgentUiMetadata::builder()
            .session_id(session_id)
            .agent_path(agent_path)
            .status(metadata.agent_status)
            .is_root(is_root);
        if let Some(parent_session_id) = metadata.parent_session_id {
            builder = builder.parent_session_id(parent_session_id);
        }
        if let Some(nickname) = metadata.agent_nickname {
            builder = builder.nickname(nickname);
        }
        if let Some(role) = metadata.agent_role {
            builder = builder.role(role);
        }
        Ok(builder.build())
    }
}
```

- [ ] **Step 5: 实现 `Kernel::agent_ui_snapshot`**

在 `impl AgentKernel for Kernel` 中增加：

```rust
    async fn agent_ui_snapshot(
        &self,
        root_session_id: &SessionId,
    ) -> Result<Vec<protocol::AgentUiMetadata>, KernelError> {
        Ok(self.agent_control.agent_ui_snapshot(root_session_id).await)
    }
```

- [ ] **Step 6: 运行 kernel 测试**

Run:

```bash
rtk cargo test -p kernel agent_ui_snapshot
```

Expected: PASS。

## Task 3: ACP Adapter 发送 `_meta.clawcode.subagents`

**Files:**
- Modify: `crates/acp/src/agent.rs`
- Test: `crates/acp/src/agent.rs`

- [ ] **Step 1: 写 `_meta` 构造测试**

在 `crates/acp/src/agent.rs` tests 中新增：

```rust
/// Verifies subagent metadata is carried on ToolCallUpdate._meta and has no visible content.
#[test]
fn subagent_snapshot_update_uses_tool_call_meta_without_visible_content() {
    let root = protocol::AgentUiMetadata::builder()
        .session_id(protocol::SessionId("root-session".to_string()))
        .agent_path(protocol::AgentPath::root())
        .status(protocol::AgentStatus::Running)
        .is_root(true)
        .build();

    let update = ClawcodeAgent::subagent_metadata_update(
        protocol::AgentUiEventKind::Snapshot,
        vec![root],
    );

    let SessionUpdate::ToolCallUpdate(update) = update else {
        panic!("subagent metadata should be a ToolCallUpdate");
    };
    assert_eq!(update.tool_call_id.0.as_ref(), "clawcode-subagents");
    assert!(update.fields.content.is_none());
    assert_eq!(
        update.meta.as_ref().unwrap()["clawcode"]["subagents"]["event"],
        "snapshot"
    );
}
```

- [ ] **Step 2: 运行失败测试**

Run:

```bash
rtk cargo test -p acp subagent_snapshot_update_uses_tool_call_meta_without_visible_content
```

Expected: 编译失败，提示 `subagent_metadata_update` 未定义。

- [ ] **Step 3: 实现 metadata update 构造方法**

在 `impl ClawcodeAgent` 中增加：

```rust
const SUBAGENT_METADATA_TOOL_CALL_ID: &str = "clawcode-subagents";

/// Build a metadata-only ACP update for the TUI agent picker.
fn subagent_metadata_update(
    event: protocol::AgentUiEventKind,
    agents: Vec<protocol::AgentUiMetadata>,
) -> SessionUpdate {
    let patch = protocol::AgentUiMetadataPatch::builder()
        .version(1)
        .event(event)
        .agents(agents)
        .build();
    let meta = serde_json::json!({
        "clawcode": {
            "subagents": patch,
        }
    });
    let meta = meta
        .as_object()
        .cloned()
        .expect("metadata root must be an object");
    SessionUpdate::ToolCallUpdate(
        ToolCallUpdate::new(
            ToolCallId::new(Self::SUBAGENT_METADATA_TOOL_CALL_ID),
            ToolCallUpdateFields::default(),
        )
        .meta(meta),
    )
}
```

- [ ] **Step 4: new/load session 后发送 snapshot**

在 `handle_new_session` 里创建 session 后先保存 `root_session_id`，再执行会 move `created.session_id` 的 router registration；异步发送 available commands 的同一延迟 task 中追加 snapshot：

```rust
let kernel = Arc::clone(&self.kernel);
let root_session_id = created.session_id.clone();
let acp_snapshot_session_id = acp_session_id.clone();
tokio::spawn(async move {
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let _ = Self::send_available_commands(&sid, &cx_for_cmds);
    if let Ok(snapshot) = kernel.agent_ui_snapshot(&root_session_id).await {
        let update = Self::subagent_metadata_update(protocol::AgentUiEventKind::Snapshot, snapshot);
        let _ = cx_for_cmds.send_notification(SessionNotification::new(acp_snapshot_session_id, update));
    }
});
```

在 `handle_load_session` 使用同样方式发送 load 后 snapshot。必须在 `Self::replay_history` 之后发送，避免 metadata-only update 进入 replay transcript。

- [ ] **Step 5: fake kernel 补齐 trait 方法**

在 `crates/acp/src/agent.rs` 的 fake kernel impl 中增加：

```rust
async fn agent_ui_snapshot(
    &self,
    root_session_id: &protocol::SessionId,
) -> Result<Vec<protocol::AgentUiMetadata>, protocol::KernelError> {
    Ok(vec![
        protocol::AgentUiMetadata::builder()
            .session_id(root_session_id.clone())
            .agent_path(protocol::AgentPath::root())
            .status(protocol::AgentStatus::Running)
            .is_root(true)
            .build(),
    ])
}
```

- [ ] **Step 6: prompt event 转换为 metadata upsert/status**

在 `handle_prompt` match 中替换当前 `_ => {}` 对 subagent lifecycle 的吞掉行为：

```rust
Event::AgentSpawned {
    session_id,
    agent_path,
    agent_nickname,
    agent_role,
} => {
    let metadata = protocol::AgentUiMetadata::builder()
        .session_id(session_id)
        .parent_session_id(protocol::SessionId(acp_sid.0.to_string()))
        .agent_path(agent_path)
        .nickname(agent_nickname)
        .role(agent_role)
        .status(protocol::AgentStatus::Running)
        .is_root(false)
        .build();
    let update = Self::subagent_metadata_update(protocol::AgentUiEventKind::Upsert, vec![metadata]);
    let _ = cx.send_notification(SessionNotification::new(acp_sid.clone(), update));
}
Event::AgentStatusChange {
    session_id,
    agent_path,
    status,
} => {
    let metadata = protocol::AgentUiMetadata::builder()
        .session_id(session_id)
        .agent_path(agent_path)
        .status(status)
        .is_root(false)
        .build();
    let update = Self::subagent_metadata_update(protocol::AgentUiEventKind::Status, vec![metadata]);
    let _ = cx.send_notification(SessionNotification::new(acp_sid.clone(), update));
}
```

如果当前 `AgentSpawned.session_id` 仍表示 parent session 而不是 child session，执行本步骤前必须先修正 kernel event 定义或发射点，使 `session_id` 表示被 upsert 的 agent session；parent 放在 `parent_session_id`。

- [ ] **Step 7: 运行 ACP 测试**

Run:

```bash
rtk cargo test -p acp subagent_snapshot_update_uses_tool_call_meta_without_visible_content
```

Expected: PASS。

## Task 4: TUI AgentNavigationState 与 metadata 解析

**Files:**
- Create: `crates/tui/src/ui/agent_navigation.rs`
- Modify: `crates/tui/src/ui/mod.rs`
- Test: `crates/tui/src/ui/agent_navigation.rs`

- [ ] **Step 1: 写导航状态测试**

新增测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::SessionId;

    /// Verifies root is always first and labeled as Main [default].
    #[test]
    fn root_entry_is_first_and_labeled_main_default() {
        let root = SessionId::new("root-session");
        let child = SessionId::new("child-session");
        let mut state = AgentNavigationState::new(root.clone());

        state.upsert(AgentPickerEntry::builder()
            .session_id(child)
            .parent_session_id(root.clone())
            .agent_path("/root/inspect".to_string())
            .nickname("finder".to_string())
            .role("worker".to_string())
            .status(AgentPickerStatus::Running)
            .is_root(false)
            .build());

        let entries = state.ordered_entries();
        assert_eq!(entries[0].session_id(), &root);
        assert_eq!(entries[0].label(), "Main [default]");
        assert_eq!(entries[1].label(), "finder [worker]");
    }

    /// Verifies status updates do not change first-seen ordering.
    #[test]
    fn status_update_preserves_first_seen_order() {
        let root = SessionId::new("root-session");
        let child = SessionId::new("child-session");
        let mut state = AgentNavigationState::new(root.clone());
        state.upsert(AgentPickerEntry::child(child.clone(), root, "/root/inspect"));
        state.upsert(AgentPickerEntry::status_only(child.clone(), AgentPickerStatus::Completed));

        assert_eq!(state.ordered_entries()[1].session_id(), &child);
        assert_eq!(state.ordered_entries()[1].status(), AgentPickerStatus::Completed);
    }
}
```

- [ ] **Step 2: 运行失败测试**

Run:

```bash
rtk cargo test -p tui agent_navigation
```

Expected: 编译失败，提示模块和类型未定义。

- [ ] **Step 3: 实现导航类型**

在 `agent_navigation.rs` 实现：

```rust
//! Agent picker ordering, labels, and status state.

use std::collections::HashMap;

use agent_client_protocol::schema::SessionId;

/// Display status used by the TUI agent picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentPickerStatus {
    Pending,
    Running,
    Completed,
    Errored,
    Closed,
    Unknown,
}

/// One selectable agent row in the picker.
#[derive(Clone, Debug, PartialEq, Eq, typed_builder::TypedBuilder)]
pub(crate) struct AgentPickerEntry {
    /// ACP session id used for prompt routing and transcript switching.
    session_id: SessionId,
    /// Parent session id for non-root agents.
    #[builder(default, setter(strip_option))]
    parent_session_id: Option<SessionId>,
    /// Canonical runtime path.
    #[builder(default, setter(strip_option))]
    agent_path: Option<String>,
    /// Human-friendly nickname.
    #[builder(default, setter(strip_option))]
    nickname: Option<String>,
    /// Runtime role name.
    #[builder(default, setter(strip_option))]
    role: Option<String>,
    /// Latest display status.
    #[builder(default = AgentPickerStatus::Unknown)]
    status: AgentPickerStatus,
    /// True for the main/root session.
    is_root: bool,
}

impl AgentPickerEntry {
    /// Returns the ACP session id for this picker entry.
    pub(crate) fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Returns the latest picker status.
    pub(crate) fn status(&self) -> AgentPickerStatus {
        self.status
    }

    /// Builds the label shown in the picker.
    pub(crate) fn label(&self) -> String {
        if self.is_root {
            return "Main [default]".to_string();
        }
        match (self.nickname.as_deref(), self.role.as_deref()) {
            (Some(nickname), Some(role)) => format!("{nickname} [{role}]"),
            (Some(nickname), None) => nickname.to_string(),
            (None, Some(role)) => format!("Agent [{role}]"),
            (None, None) => self
                .agent_path
                .as_deref()
                .and_then(|path| path.rsplit('/').next())
                .filter(|name| !name.is_empty())
                .unwrap_or("Agent")
                .to_string(),
        }
    }
}

/// Stable first-seen order and metadata for the agent picker.
#[derive(Clone, Debug)]
pub(crate) struct AgentNavigationState {
    root_session_id: SessionId,
    agents: HashMap<SessionId, AgentPickerEntry>,
    order: Vec<SessionId>,
}
```

实现 `new` 时必须插入 root entry，并保证 `order[0] == root_session_id`。实现 `upsert` 时只在新 session 出现时追加 order；root upsert 仍保持第一项。

- [ ] **Step 4: 实现 metadata patch 转换**

实现 `TryFrom<protocol::AgentUiMetadata> for AgentPickerEntry`，并把 `protocol::AgentStatus` 映射到 `AgentPickerStatus`：

```rust
impl From<protocol::AgentStatus> for AgentPickerStatus {
    /// Convert kernel status into picker status tokens.
    fn from(status: protocol::AgentStatus) -> Self {
        match status {
            protocol::AgentStatus::PendingInit => Self::Pending,
            protocol::AgentStatus::Running => Self::Running,
            protocol::AgentStatus::Completed { .. } => Self::Completed,
            protocol::AgentStatus::Errored { .. } => Self::Errored,
            protocol::AgentStatus::Interrupted | protocol::AgentStatus::Shutdown => Self::Closed,
            protocol::AgentStatus::NotFound => Self::Unknown,
        }
    }
}
```

- [ ] **Step 5: 导出模块并运行测试**

在 `crates/tui/src/ui/mod.rs` 增加：

```rust
pub mod agent_navigation;
```

Run:

```bash
rtk cargo test -p tui agent_navigation
```

Expected: PASS。

## Task 5: TUI SessionRouterState 分发 notification 与保存 per-session UI 状态

**Files:**
- Create: `crates/tui/src/ui/session_router.rs`
- Modify: `crates/tui/src/ui/mod.rs`
- Test: `crates/tui/src/ui/session_router.rs`

- [ ] **Step 1: 写 router 分发测试**

新增测试：

```rust
/// Verifies inactive session notifications are retained instead of ignored.
#[test]
fn router_keeps_inactive_session_notifications() {
    let root = SessionId::new("root-session");
    let child = SessionId::new("child-session");
    let mut router = SessionRouterState::new(
        root.clone(),
        "/tmp/project".into(),
        "provider/model".to_string(),
        Theme::dark(),
    );

    router.apply_session_notification(SessionNotification::new(
        child.clone(),
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            "child output",
        )))),
    ));

    assert_eq!(router.active_session_id(), &root);
    assert!(router.state_for(&child).unwrap().transcript().iter().any(|entry| {
        entry.text_cell().is_some_and(|cell| cell.text().contains("child output"))
    }));
}

/// Verifies metadata-only updates do not create visible tool cells.
#[test]
fn router_consumes_subagent_metadata_without_tool_cell() {
    let root = SessionId::new("root-session");
    let mut router = SessionRouterState::new(
        root.clone(),
        "/tmp/project".into(),
        "provider/model".to_string(),
        Theme::dark(),
    );

    let update = metadata_update_for_child("child-session", "finder", "worker");
    router.apply_session_notification(SessionNotification::new(root.clone(), update));

    assert_eq!(router.active_state().transcript().len(), 0);
    assert_eq!(router.agent_navigation().ordered_entries().len(), 2);
}
```

- [ ] **Step 2: 运行失败测试**

Run:

```bash
rtk cargo test -p tui session_router
```

Expected: 编译失败，提示 `SessionRouterState` 未定义。

- [ ] **Step 3: 实现 router struct**

在 `session_router.rs` 添加：

```rust
//! Multi-session routing above the single-session AppState reducer.

use std::collections::HashMap;
use std::path::PathBuf;

use agent_client_protocol::schema::{SessionId, SessionNotification, SessionUpdate, ToolCallUpdate};

use crate::ui::agent_navigation::AgentNavigationState;
use crate::ui::composer::Composer;
use crate::ui::state::AppState;
use crate::ui::theme::Theme;
use crate::ui::view::ViewState;

/// Top-level TUI state for routing multiple ACP sessions.
#[derive(Debug, typed_builder::TypedBuilder)]
pub(crate) struct SessionRouterState {
    /// Session currently shown and used for prompt submit.
    active_session_id: SessionId,
    /// Working directory used when lazily constructing session states.
    cwd: PathBuf,
    /// Model label copied into lazily constructed session states.
    model_label: String,
    /// Render theme copied into lazily constructed session states.
    theme: Theme,
    /// Per-session transcript reducers.
    states: HashMap<SessionId, AppState>,
    /// Per-session viewport state.
    view_snapshots: HashMap<SessionId, ViewState>,
    /// Per-session composer drafts.
    composer_snapshots: HashMap<SessionId, Composer>,
    /// Agent picker metadata and ordering.
    agent_navigation: AgentNavigationState,
}
```

因为字段数超过 3，必须用 `typed_builder`。`new` 方法内通过 builder 构造，并插入 root `AppState`。

- [ ] **Step 4: 实现 metadata-only 拦截**

实现 `apply_session_notification`：

```rust
    /// Route one ACP session notification to metadata state or the matching AppState.
    pub(crate) fn apply_session_notification(&mut self, notification: SessionNotification) {
        if let Some(patch) = Self::subagent_metadata_patch(&notification.update) {
            self.agent_navigation.apply_patch(patch);
            return;
        }

        let session_id = notification.session_id.clone();
        self.ensure_state(session_id.clone());
        if let Some(state) = self.states.get_mut(&session_id) {
            state.apply_session_update(notification);
        }
    }
```

实现 `subagent_metadata_patch` 时从 `ToolCallUpdate.meta` 读取：

```rust
    /// Extract clawcode subagent metadata from a metadata-only ToolCallUpdate.
    fn subagent_metadata_patch(update: &SessionUpdate) -> Option<protocol::AgentUiMetadataPatch> {
        let SessionUpdate::ToolCallUpdate(ToolCallUpdate { meta: Some(meta), .. }) = update else {
            return None;
        };
        let payload = meta.get("clawcode")?.get("subagents")?.clone();
        serde_json::from_value(payload).ok()
    }
```

解析失败只返回 `None` 并允许普通 reducer 处理；如果 update 使用内部 `clawcode-subagents` id 且解析失败，应记录 warn 并丢弃，避免污染 transcript。

- [ ] **Step 5: 实现 session 切换 API**

实现：

```rust
    /// Select a visible agent session and make it the active TUI context.
    pub(crate) fn select_agent_session(
        &mut self,
        target_session_id: SessionId,
        current_view: &mut ViewState,
        current_composer: &mut Composer,
    ) -> Result<(), SelectAgentSessionError> {
        if target_session_id == self.active_session_id {
            return Ok(());
        }
        self.view_snapshots
            .insert(self.active_session_id.clone(), current_view.clone());
        self.composer_snapshots
            .insert(self.active_session_id.clone(), current_composer.clone());
        self.ensure_state(target_session_id.clone());
        self.active_session_id = target_session_id.clone();
        *current_view = self.view_snapshots.remove(&target_session_id).unwrap_or_default();
        *current_composer = self.composer_snapshots.remove(&target_session_id).unwrap_or_default();
        Ok(())
    }
```

第一阶段先用 lazy empty `AppState` 支持 live inactive updates；历史 replay load 在 Task 9 补齐。

- [ ] **Step 6: 导出模块并运行测试**

在 `crates/tui/src/ui/mod.rs` 增加：

```rust
pub mod session_router;
```

Run:

```bash
rtk cargo test -p tui session_router
```

Expected: PASS。

## Task 6: `/agent` 命令与 picker 状态

**Files:**
- Create: `crates/tui/src/ui/agent_picker.rs`
- Modify: `crates/kernel/src/command/slash_command.rs`
- Modify: `crates/tui/src/app.rs`
- Test: `crates/tui/src/ui/agent_picker.rs`
- Test: `crates/kernel/src/command/slash_command.rs`
- Test: `crates/tui/src/app.rs`

- [ ] **Step 1: 写 `/agent` slash 解析测试**

在 `slash_command.rs` tests 中扩展：

```rust
#[test]
fn parse_agent_command() {
    assert_eq!(SlashCommand::from_str("agent"), Ok(SlashCommand::Agent));
    assert_eq!(
        SlashCommand::parse_from_text("/agent"),
        Some(SlashCommand::Agent)
    );
}
```

- [ ] **Step 2: 实现 slash command**

在 enum 中加入：

```rust
pub enum SlashCommand {
    Raw,
    Sessions,
    Agent,
}
```

`description` 返回：

```rust
Self::Agent => "switch between main agent and subagents",
```

`supports_inline_args` 不包含 `Agent`，因为本交互通过 picker 完成。

- [ ] **Step 3: 写 picker 状态测试**

在 `agent_picker.rs` tests 中新增：

```rust
/// Verifies picker focus opens and arrow movement wraps through entries.
#[test]
fn picker_focus_moves_selection_with_wraparound() {
    let mut picker = AgentPickerPanelState::default();
    picker.open(2);

    assert!(picker.is_focused());
    assert_eq!(picker.selected_index(), 0);

    picker.move_previous(2);
    assert_eq!(picker.selected_index(), 1);

    picker.move_next(2);
    assert_eq!(picker.selected_index(), 0);
}
```

- [ ] **Step 4: 实现 picker 状态**

在 `agent_picker.rs` 添加：

```rust
//! Inline agent picker state and rendering helpers.

/// Focus and selection state for the inline `/agent` picker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentPickerPanelState {
    visible: bool,
    focused: bool,
    selected_index: usize,
}

impl AgentPickerPanelState {
    /// Opens the picker and clamps the current selection.
    pub(crate) fn open(&mut self, entry_count: usize) {
        self.visible = true;
        self.focused = true;
        self.clamp_selection(entry_count);
    }

    /// Closes the picker and returns focus to the composer.
    pub(crate) fn close(&mut self) {
        self.visible = false;
        self.focused = false;
    }

    /// Moves selection to the previous entry with wraparound.
    pub(crate) fn move_previous(&mut self, entry_count: usize) {
        if entry_count == 0 {
            self.selected_index = 0;
            return;
        }
        self.selected_index = if self.selected_index == 0 {
            entry_count - 1
        } else {
            self.selected_index - 1
        };
    }

    /// Moves selection to the next entry with wraparound.
    pub(crate) fn move_next(&mut self, entry_count: usize) {
        if entry_count == 0 {
            self.selected_index = 0;
            return;
        }
        self.selected_index = (self.selected_index + 1) % entry_count;
    }

    /// Returns the selected entry index.
    pub(crate) fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// Returns whether the picker is focused.
    pub(crate) fn is_focused(&self) -> bool {
        self.focused
    }
}
```

- [ ] **Step 5: app.rs 识别 `/agent`**

把 local command 分发从 `handle_raw_command` 扩展为 `handle_local_command`：

```rust
fn handle_local_command(ui: &mut UiRuntime<'_>, text: &str) -> bool {
    match SlashCommand::parse_from_text(text) {
        Some(SlashCommand::Raw) => handle_raw_command(ui, text),
        Some(SlashCommand::Agent) => {
            ui.router.open_agent_picker();
            true
        }
        _ => false,
    }
}
```

保持 `/agent` 不发送给 model。

- [ ] **Step 6: 运行相关测试**

Run:

```bash
rtk cargo test -p kernel slash_command
rtk cargo test -p tui agent_picker
rtk cargo test -p tui raw_command
```

Expected: PASS。

## Task 7: Inline picker 布局与渲染

**Files:**
- Modify: `crates/tui/src/ui/layout.rs`
- Modify: `crates/tui/src/ui/render.rs`
- Modify: `crates/tui/src/ui/status.rs`
- Test: `crates/tui/src/ui/render.rs`
- Test: `crates/tui/src/ui/layout.rs`

- [ ] **Step 1: 写布局测试**

在 `layout.rs` tests 中新增：

```rust
/// Verifies visible agent picker reserves rows under the composer.
#[test]
fn frame_rows_reserves_agent_picker_height_when_visible() {
    let rows = frame_rows_with_agent_picker(Rect::new(0, 0, 80, 20), "hello", 3)
        .expect("rows");

    assert_eq!(rows.agent_picker.height, 3);
    assert!(rows.agent_picker.y > rows.composer.y);
}
```

- [ ] **Step 2: 改 layout API**

新增方法并让旧 `frame_rows` 调用它：

```rust
/// Splits the terminal frame and optionally reserves rows for the agent picker.
pub(super) fn frame_rows_with_agent_picker(
    area: Rect,
    composer_text: &str,
    agent_picker_height: u16,
) -> Option<FrameRows> {
    let picker_height = agent_picker_height.min(5);
    let composer_height = composer_height(composer_text);
    let transcript_min = if area.height >= composer_height.saturating_add(picker_height).saturating_add(5) {
        3
    } else {
        1
    };
    let constraints = vec![
        Constraint::Min(transcript_min),
        Constraint::Length(1),
        Constraint::Length(composer_height),
        Constraint::Length(picker_height),
        Constraint::Length(1),
    ];

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    match rows.as_ref() {
        [transcript, top_status, composer, agent_picker, bottom_status] => Some(
            FrameRows::builder()
                .transcript(*transcript)
                .top_status(*top_status)
                .composer(*composer)
                .agent_picker(*agent_picker)
                .bottom_status(*bottom_status)
                .build(),
        ),
        _ => None,
    }
}
```

`FrameRows` 因为字段超过 3，继续使用 `TypedBuilder`，增加：

```rust
/// Area used by the inline agent picker below the composer.
pub(super) agent_picker: Rect,
```

- [ ] **Step 3: 写 render 测试**

在 `render.rs` tests 中新增：

```rust
/// Verifies `/agent` picker renders Main [default] under the composer.
#[test]
fn render_agent_picker_shows_main_default_under_composer() {
    let backend = TestBackend::new(80, 12);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let root = sid("root-session");
    let mut router = SessionRouterState::new(
        root,
        "/tmp/project".into(),
        "provider/model".to_string(),
        Theme::dark(),
    );
    router.open_agent_picker();

    terminal
        .draw(|frame| render(frame, &router, &ViewState::default(), &composer("")))
        .expect("draw");

    let screen = rendered_screen(&terminal);
    assert!(screen.iter().any(|line| line.contains("Main [default]")));
}
```

- [ ] **Step 4: 改 render 入口**

把 render 签名改为：

```rust
pub fn render(
    frame: &mut Frame<'_>,
    router: &SessionRouterState,
    view: &ViewState,
    composer: &Composer,
)
```

内部使用：

```rust
let state = router.active_state();
let picker_height = router.agent_picker_height();
let rows = layout::frame_rows_with_agent_picker(frame.area(), composer.text(), picker_height)?;
transcript::render_transcript(frame, rows.transcript, state, view);
status::render_top_status(frame, rows.top_status, state);
render_composer(frame, rows.composer, composer, state.theme());
agent_picker::render_agent_picker(frame, rows.agent_picker, router, state.theme());
status::render_bottom_status_with_agent(
    frame,
    rows.bottom_status,
    state,
    router.active_agent_label().as_str(),
);
```

- [ ] **Step 5: bottom status 增加 active agent label**

在 `status::bottom_status_line` 增加 router-aware variant，优先保留现有测试兼容：

```rust
pub(super) fn render_bottom_status_with_agent(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    active_agent_label: &str,
)
```

底部显示示例：

```text
provider/model | /tmp/project | agent: Main [default] | tokens: 0
```

- [ ] **Step 6: 运行渲染测试**

Run:

```bash
rtk cargo test -p tui frame_rows_reserves_agent_picker_height_when_visible
rtk cargo test -p tui render_agent_picker_shows_main_default_under_composer
rtk cargo test -p tui render_
```

Expected: PASS。

## Task 8: 键盘交互与 session 切换

**Files:**
- Modify: `crates/tui/src/app.rs`
- Modify: `crates/tui/src/ui/session_router.rs`
- Test: `crates/tui/src/app.rs`
- Test: `crates/tui/src/ui/session_router.rs`

- [ ] **Step 1: 写 picker 键盘测试**

在 `app.rs` tests 中新增纯函数级测试，避免启动终端：

```rust
/// Verifies focused picker consumes Up/Down/Enter without scrolling composer state.
#[test]
fn focused_agent_picker_handles_arrow_and_enter() {
    let root = SessionId::new("root-session");
    let child = SessionId::new("child-session");
    let mut router = SessionRouterState::new(
        root.clone(),
        "/tmp/project".into(),
        "provider/model".to_string(),
        Theme::dark(),
    );
    router.agent_navigation_mut().upsert(AgentPickerEntry::child(
        child.clone(),
        root.clone(),
        "/root/inspect",
    ));
    router.open_agent_picker();

    assert!(handle_agent_picker_key(&mut router, KeyCode::Down).unwrap().is_none());
    assert_eq!(
        handle_agent_picker_key(&mut router, KeyCode::Enter).unwrap(),
        Some(child)
    );
}
```

- [ ] **Step 2: 实现 picker key handler**

在 `app.rs` 增加：

```rust
/// Handles keys while the inline agent picker has focus.
fn handle_agent_picker_key(
    router: &mut SessionRouterState,
    code: KeyCode,
) -> anyhow::Result<Option<SessionId>> {
    match code {
        KeyCode::Up => {
            router.move_agent_picker_previous();
            Ok(None)
        }
        KeyCode::Down => {
            router.move_agent_picker_next();
            Ok(None)
        }
        KeyCode::Enter => Ok(router.selected_agent_session_id().cloned()),
        KeyCode::Esc => {
            router.close_agent_picker();
            Ok(None)
        }
        _ => Ok(None),
    }
}
```

- [ ] **Step 3: 接入 `handle_key_event`**

在 approval 处理之后、全局 scroll 之前加入：

```rust
if ui.router.is_agent_picker_focused() {
    if let Some(target_session_id) = handle_agent_picker_key(ui.router, key_event.code)? {
        ui.router
            .select_agent_session(target_session_id, ui.view, ui.composer)
            .map_err(anyhow::Error::from)?;
        ui.router.close_agent_picker();
    }
    return Ok(false);
}
```

这样 picker 未聚焦时，`Up` / `Down` 继续走现有 scroll 行为。

- [ ] **Step 4: prompt 路由改用 active session**

`run_prompt` 参数从 `state: &mut AppState` 改为 `router: &mut SessionRouterState`：

```rust
let session_id = router.active_session_id().clone();
router.active_state_mut().append_user_message(&submitted);
```

`AppEvent` 改为：

```rust
PromptFinished { session_id: SessionId, stop_reason: StopReason },
PromptFailed { session_id: SessionId, message: String },
```

spawn 任务发送：

```rust
let _ = tx.send(AppEvent::PromptFinished { session_id, stop_reason });
```

- [ ] **Step 5: completion/failure 落回所属 session**

`handle_app_event` 中：

```rust
AppEvent::PromptFinished { session_id, stop_reason } => {
    router.finish_prompt_for_session(&session_id, stop_reason);
    prompt_task.clear_if_session(&session_id).await;
}
AppEvent::PromptFailed { session_id, message } => {
    router.set_error_for_session(&session_id, message);
    prompt_task.clear_if_session(&session_id).await;
}
```

第一阶段保持单个 prompt task；如果已有 prompt 正在运行，不允许新 prompt。后续可扩展为 per-session prompt tasks。

- [ ] **Step 6: 运行 app/router 测试**

Run:

```bash
rtk cargo test -p tui focused_agent_picker_handles_arrow_and_enter
rtk cargo test -p tui session_router
rtk cargo test -p tui raw_command
```

Expected: PASS。

## Task 9: child session load fallback

**Files:**
- Modify: `crates/tui/src/ui/session_router.rs`
- Modify: `crates/tui/src/app.rs`
- Modify: `crates/tui/src/acp/client.rs`
- Test: `crates/tui/src/ui/session_router.rs`

- [ ] **Step 1: 写不可见 session 选择测试**

新增测试验证当前无 transcript 的 child 会创建 state，并保留当前 session draft：

```rust
/// Verifies selecting an unseen child preserves root draft and activates the child state.
#[test]
fn selecting_unseen_child_preserves_root_composer_snapshot() {
    let root = SessionId::new("root-session");
    let child = SessionId::new("child-session");
    let mut router = SessionRouterState::new(
        root.clone(),
        "/tmp/project".into(),
        "provider/model".to_string(),
        Theme::dark(),
    );
    let mut view = ViewState::default();
    let mut composer = Composer::default();
    composer.insert_str("root draft");

    router
        .select_agent_session(child.clone(), &mut view, &mut composer)
        .expect("select child");

    assert_eq!(router.active_session_id(), &child);
    assert!(composer.is_empty());
    assert_eq!(router.composer_snapshot_text(&root), Some("root draft"));
}
```

- [ ] **Step 2: 增加 async load path**

第一阶段的 `select_agent_session` 是同步 lazy state。为了支持 persisted child history，新增 async 方法：

```rust
/// Ensure a target session has replayed state before selecting it.
pub(crate) async fn ensure_loaded_session(
    &mut self,
    client: &AcpClient,
    session_id: SessionId,
) -> anyhow::Result<()> {
    if self.states.contains_key(&session_id) && !self.state_for(&session_id).unwrap().transcript().is_empty() {
        return Ok(());
    }
    let response = client.load_session(session_id.clone(), self.cwd.clone()).await?;
    self.update_model_label_from_load(&response);
    self.ensure_state(session_id);
    Ok(())
}
```

由于 `load_session` replay 会通过 ACP notification 回流，router 只负责发起 load 并保留 state。若 load 失败，保持当前 active session。

- [ ] **Step 3: `Enter` 切换前调用 load fallback**

在 `handle_key_event` 的 Enter 分支改为：

```rust
if let Some(target_session_id) = handle_agent_picker_key(ui.router, key_event.code)? {
    ui.router.ensure_loaded_session(client, target_session_id.clone()).await?;
    ui.router.select_agent_session(target_session_id, ui.view, ui.composer)?;
    ui.router.close_agent_picker();
}
```

如果 `handle_key_event` 仍是 sync，改为 async 并更新调用链；或者把 load fallback 放到 `AppEvent::SelectAgentSession` 的 async handler 中。推荐事件化，避免在 key handler 中阻塞结构变复杂。

- [ ] **Step 4: 运行 router 测试**

Run:

```bash
rtk cargo test -p tui selecting_unseen_child_preserves_root_composer_snapshot
rtk cargo test -p tui session_router
```

Expected: PASS。

## Task 10: Kernel lifecycle events 补齐实时 upsert/status

**Files:**
- Modify: `crates/kernel/src/agent/control.rs`
- Modify: `crates/kernel/src/session.rs`
- Modify: `crates/kernel/src/turn.rs`
- Test: `crates/kernel/src/lib.rs`
- Test: `crates/acp/src/agent.rs`

- [ ] **Step 1: 确认 Event 语义并写测试**

写一个 kernel 层测试，要求 spawn 后 event stream 中出现 child session 的 `AgentSpawned`：

```rust
/// Verifies spawn_agent emits UI metadata with the child session id.
#[tokio::test]
async fn spawn_agent_emits_agent_spawned_with_child_session_id() {
    // Use the existing kernel spawn fixture from kernel_spawn_agent_creates_child_thread.
    // Submit a prompt that invokes spawn_agent, then collect events until AgentSpawned.
    let spawned = events
        .into_iter()
        .find_map(|event| match event {
            Event::AgentSpawned { session_id, agent_path, .. } => Some((session_id, agent_path)),
            _ => None,
        })
        .expect("spawn event");

    assert_ne!(spawned.0, root_session_id);
    assert_eq!(spawned.1, AgentPath::root().join("inspect"));
}
```

- [ ] **Step 2: 在 spawn tool 或 control path 发射事件**

优先在 kernel turn/tool dispatch 已经拿到 `LiveAgent` 的位置发射：

```rust
Event::agent_spawned(
    live.thread_id.clone(),
    live.metadata.agent_path.clone().expect("spawned agent path"),
    live.metadata.agent_nickname.clone().unwrap_or_default(),
    live.metadata.agent_role.clone().unwrap_or_default(),
)
```

不要让 `session_id` 表示 parent；该字段必须表示 child session，parent 放 metadata 的 `parent_session_id`。

- [ ] **Step 3: terminal status 发射 `AgentStatusChange`**

在 child turn terminal 回调或 `notify_child_terminal_turn` 对应路径发射：

```rust
Event::agent_status_change(
    child_session_id.clone(),
    child_path,
    status,
)
```

如果当前事件 channel 不在 `AgentControl` 内，使用已有 turn event tx 把 status change 从 child session event stream 发出，再由 ACP adapter 按 session id 发送到 TUI。

- [ ] **Step 4: close agent 前发送 closed status**

在 `close_agent` 移除 registry 前，先更新 status 为 `Shutdown` 并让 status watcher / event path 发送 `AgentStatusChange`。TUI 收到后只更新 list 状态，不删除 entry。

- [ ] **Step 5: 运行 kernel/acp 测试**

Run:

```bash
rtk cargo test -p kernel spawn_agent_emits_agent_spawned_with_child_session_id
rtk cargo test -p acp subagent
```

Expected: PASS。

## Task 11: 验证矩阵与回归检查

**Files:**
- No code edits unless failures expose bugs.

- [ ] **Step 1: targeted tests**

Run:

```bash
rtk cargo test -p protocol agent_ui
rtk cargo test -p kernel agent_ui_snapshot
rtk cargo test -p acp subagent
rtk cargo test -p tui agent_navigation
rtk cargo test -p tui agent_picker
rtk cargo test -p tui session_router
rtk cargo test -p tui render_agent_picker
```

Expected: all PASS。

- [ ] **Step 2: broader TUI/kernel tests**

Run:

```bash
rtk cargo test -p tui
rtk cargo test -p acp
rtk cargo test -p kernel
```

Expected: all PASS。

- [ ] **Step 3: workspace compile/clippy**

Run:

```bash
rtk cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS，无 warnings。

- [ ] **Step 4: manual TUI acceptance**

Run:

```bash
rtk cargo run -p tui -- /home/isbest/Documents/WorkSpace/clawcode
```

Manual acceptance:

1. 只有 root session 时输入 `/agent`，输入框下方显示 `Main [default]`。
2. 触发一次 `spawn_agent` 后，输入框下方 picker 显示 `Main [default]` 和 child。
3. picker 聚焦时 `Up` / `Down` 改变选中项。
4. 选中 child 后按 `Enter`，active label 切到 child，prompt 提交给 child session。
5. 在 child 中再次 `/agent`，选中 `Main [default]` 后按 `Enter`，切回 root。
6. picker 未聚焦时，`Up` / `Down` 保持当前 scroll/composer 行为。
7. `_meta` update 不出现在 transcript 中。

- [ ] **Step 5: diff hygiene**

Run:

```bash
git diff --check
rg -n "UNRESOLVED|FIXME|待确认|PLACEHOLDER_MARKER" docs/superpowers/plans/2026-05-26-tui-subagent-switching.md crates/protocol crates/kernel crates/acp crates/tui
```

Expected: `git diff --check` clean；`rg` 不返回本次新增的未解决标记。

## 自检

- Spec 覆盖：
  - 主 agent 进入列表：Task 2、Task 4、Task 7、Task 8。
  - `/agent` 入口：Task 6。
  - 输入框下方 inline picker：Task 7。
  - 方向键上下选择、回车切换：Task 8。
  - root/subagent session routing：Task 5、Task 8、Task 9。
  - ACP `_meta` 扩展：Task 1、Task 3、Task 5。
  - 不污染 transcript：Task 3、Task 5、Task 11。
  - 不新增额外 slash command：Task 6 只新增 `/agent`。

- 风险点：
  - `AgentSpawned.session_id` 当前语义可能是 parent session，Task 10 必须确认并修正为 child session，否则 TUI 无法切换。
  - `load_session` replay notification 与 active session 切换有时序关系，Task 9 必须保证 load 失败时不切换。
  - 当前 prompt task 是单任务模型；本计划保持单并发 prompt，避免一次引入 per-session prompt 并发。

- 提交策略：
  - 本计划不要求自动 commit。
  - 用户明确允许提交后，先运行相关 pre-commit hooks，再按 `.gitmessage` 写 commit message。
