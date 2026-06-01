# 参考实现风格细粒度权限审批 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 clawcode 当前一次性审批升级为 参考实现风格的 `AskForApproval`、`ReviewDecision`、session `ApprovalStore`、`ToolInvocation`、`ExecApprovalRequirement` 和 execpolicy amendment 流程。

**Architecture:** 保留现有 `ToolRegistry -> Arc<dyn Tool>` 的 object-safe 边界，在具体工具内部增加 typed request/key adapter。kernel 在执行工具前构造 `ToolInvocation`，基于 `ExecApprovalRequirement`、session `ApprovalStore` 和 execpolicy 结果决定跳过、弹窗或拒绝。

**Tech Stack:** Rust, Tokio, Serde, typed-builder, async-trait, existing `protocol` / `tools` / `kernel` / `config` / `tui` crates.

---

## 文件结构

本计划新增或修改以下文件：

- Create: `crates/protocol/src/approvals.rs`
  - 参考实现风格审批协议类型：`AskForApproval`、`GranularApprovalConfig`、`ReviewDecision`、`ExecPolicyAmendment`、network approval 类型、`ExecApprovalRequestEvent`。
- Modify: `crates/protocol/src/lib.rs`
  - 导出 `approvals` 模块。
- Modify: `crates/protocol/src/config.rs`
  - 将 `ToolContext` 的 approval 字段升级为 `AskForApproval`，保留旧 `ApprovalMode` 兼容映射。
- Modify: `crates/protocol/src/permission.rs`
  - 保留旧 ACP 兼容类型，并提供到新 `ReviewDecision` 的转换。
- Modify: `crates/protocol/src/event.rs`
  - `ExecApprovalRequested` 携带 `ExecApprovalRequestEvent`，保留旧 helper 或新增兼容 helper。
- Modify: `crates/protocol/src/op.rs`
  - approval response 使用新 `ReviewDecision`。
- Modify: `crates/config/src/config.rs`
  - 加载新 `approval_policy`，旧 `approval` 映射到 `AskForApproval`。
- Modify: `crates/kernel/src/approval.rs`
  - 迁移为模块入口或继续作为 facade，暴露 `ApprovalPolicy` wrapper 和 `ApprovalStore`。
- Create: `crates/kernel/src/approval/store.rs`
  - session-scoped `ApprovalStore` 和 `with_cached_approval`。
- Create: `crates/kernel/src/exec_policy.rs`
  - 最小 execpolicy parser/evaluator/writer，支持 `prefix_rule` 和 `network_rule`。
- Modify: `crates/kernel/src/turn.rs`
  - `dispatch_tool` 改为 `build_invocation -> approval requirement -> cached approval -> execute_invocation`。
- Modify: `crates/kernel/src/session.rs`
  - session runtime 持有 `ApprovalStore`，approval response 使用新 decision。
- Modify: `crates/kernel/src/lib.rs`
  - `resolve_approval` 使用新 decision 类型。
- Modify: `crates/tools/src/lib.rs`
  - object-safe `Tool` 增加 `build_invocation`、`exec_approval_requirement`、`execute_invocation` 默认方法。
- Create: `crates/tools/src/invocation.rs`
  - `ToolInvocation`、`ToolApprovalInvocation`、`ToolExecution` 和 `TypedToolRuntime`。
- Modify: `crates/tools/src/builtin/shell.rs`
  - 增加 `ShellRequest`、`ShellApprovalKey`、shell invocation adapter。
- Modify: `crates/tools/src/builtin/fs/legacy/apply_patch/mod.rs`
  - 增加 `ApplyPatchApprovalKey` 和 invocation adapter。
- Modify: `crates/tools/src/mcp.rs`
  - 增加 MCP approval key 和 invocation adapter。
- Modify: `crates/tui/src/ui/approval.rs`
  - 基于 `available_decisions` 渲染和 key mapping。
- Modify: `crates/tui/src/app.rs`
  - approval response 映射到新 `ReviewDecision`。
- Modify: `crates/protocol/src/acp_conv.rs`
  - ACP option kind 到新 decision 的兼容映射。

---

### Task 1: 新增 参考实现风格 approval protocol 类型

**Files:**
- Create: `crates/protocol/src/approvals.rs`
- Modify: `crates/protocol/src/lib.rs`
- Modify: `crates/protocol/src/permission.rs`
- Test: `crates/protocol/src/approvals.rs`

- [ ] **Step 1: 写 approval decision 兼容测试**

在 `crates/protocol/src/approvals.rs` 末尾加入测试模块。先写测试，文件此时不存在，运行会失败。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission;

    /// Verifies legacy one-shot approval decisions map to enhanced decisions.
    #[test]
    fn legacy_allow_once_maps_to_approved() {
        let decision = ReviewDecision::from(permission::PermissionOptionKind::AllowOnce);

        assert_eq!(decision, ReviewDecision::Approved);
    }

    /// Verifies legacy "always" decisions become session-scoped approvals.
    #[test]
    fn legacy_allow_always_maps_to_approved_for_session() {
        let decision = ReviewDecision::from(permission::PermissionOptionKind::AllowAlways);

        assert_eq!(decision, ReviewDecision::ApprovedForSession);
    }

    /// Verifies execpolicy amendments serialize as command-token arrays.
    #[test]
    fn exec_policy_amendment_serializes_as_command_array() {
        let amendment = ExecPolicyAmendment::new(vec![
            "cargo".to_string(),
            "test".to_string(),
        ]);

        let encoded = serde_json::to_string(&amendment).expect("serialize amendment");

        assert_eq!(encoded, r#"["cargo","test"]"#);
    }
}
```

- [ ] **Step 2: 运行失败测试**

Run: `rtk cargo test -p protocol legacy_allow_once_maps_to_approved`

Expected: 编译失败，错误包含 `file not found for module approvals` 或 `cannot find type ReviewDecision`。

- [ ] **Step 3: 新增 `crates/protocol/src/approvals.rs`**

创建完整类型定义。所有新增 public 类型和函数需要英文 doc comment。

```rust
//! enhanced approval protocol types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::permission;

/// Proposed execpolicy change that allows commands starting with this prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecPolicyAmendment {
    /// Command tokens that form the allow prefix.
    pub command: Vec<String>,
}

impl ExecPolicyAmendment {
    /// Create a new execpolicy amendment from command prefix tokens.
    #[must_use]
    pub fn new(command: Vec<String>) -> Self {
        Self { command }
    }

    /// Return the command prefix tokens.
    #[must_use]
    pub fn command(&self) -> &[String] {
        &self.command
    }
}

impl From<Vec<String>> for ExecPolicyAmendment {
    /// Convert command prefix tokens into an amendment.
    fn from(command: Vec<String>) -> Self {
        Self { command }
    }
}

/// Network protocol attached to an approval prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkApprovalProtocol {
    /// Plain HTTP traffic.
    Http,
    /// HTTPS traffic or CONNECT requests.
    #[serde(alias = "https_connect", alias = "http-connect")]
    Https,
    /// SOCKS5 TCP traffic.
    Socks5Tcp,
    /// SOCKS5 UDP traffic.
    Socks5Udp,
}

/// Runtime network request context shown in approval prompts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkApprovalContext {
    /// Host that triggered the approval prompt.
    pub host: String,
    /// Protocol used by the blocked request.
    pub protocol: NetworkApprovalProtocol,
}

/// Persisted network policy action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicyRuleAction {
    /// Allow matching future network requests.
    Allow,
    /// Deny matching future network requests.
    Deny,
}

/// Proposed network policy change for a host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPolicyAmendment {
    /// Host covered by the amendment.
    pub host: String,
    /// Action persisted for matching future requests.
    pub action: NetworkPolicyRuleAction,
}

/// Optional additional permissions requested by a tool invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdditionalPermissionProfile {
    /// Whether network access is requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<bool>,
    /// File-system paths requested for read access.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_paths: Vec<PathBuf>,
    /// File-system paths requested for write access.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_paths: Vec<PathBuf>,
}

/// Parsed command summary used by approval UIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedCommand {
    /// Parsed command tokens.
    pub command: Vec<String>,
}

/// User decision in response to an approval request.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// User approved this request once.
    Approved,
    /// User approved and wants to persist an execpolicy prefix.
    ApprovedExecpolicyAmendment {
        /// Proposed execpolicy amendment selected by the user.
        proposed_execpolicy_amendment: ExecPolicyAmendment,
    },
    /// User approved matching requests for this session.
    ApprovedForSession,
    /// User selected a network policy amendment.
    NetworkPolicyAmendment {
        /// Proposed network policy amendment selected by the user.
        network_policy_amendment: NetworkPolicyAmendment,
    },
    /// User denied this request but the turn may continue.
    #[default]
    Denied,
    /// Approval timed out before a decision was received.
    TimedOut,
    /// User aborted the current turn.
    Abort,
}

impl ReviewDecision {
    /// Return a stable non-sensitive label for logs and metrics.
    #[must_use]
    pub fn to_opaque_string(&self) -> &'static str {
        match self {
            ReviewDecision::Approved => "approved",
            ReviewDecision::ApprovedExecpolicyAmendment { .. } => {
                "approved_with_amendment"
            }
            ReviewDecision::ApprovedForSession => "approved_for_session",
            ReviewDecision::NetworkPolicyAmendment {
                network_policy_amendment,
            } => match network_policy_amendment.action {
                NetworkPolicyRuleAction::Allow => "approved_with_network_policy_allow",
                NetworkPolicyRuleAction::Deny => "denied_with_network_policy_deny",
            },
            ReviewDecision::Denied => "denied",
            ReviewDecision::TimedOut => "timed_out",
            ReviewDecision::Abort => "abort",
        }
    }
}

impl From<permission::ReviewDecision> for ReviewDecision {
    /// Convert legacy approval decisions into enhanced decisions.
    fn from(value: permission::ReviewDecision) -> Self {
        match value {
            permission::PermissionOptionKind::AllowOnce => ReviewDecision::Approved,
            permission::PermissionOptionKind::AllowAlways => ReviewDecision::ApprovedForSession,
            permission::PermissionOptionKind::RejectOnce
            | permission::PermissionOptionKind::RejectAlways => ReviewDecision::Denied,
            permission::ReviewDecision::Abort => ReviewDecision::Abort,
        }
    }
}

/// Shell command approval request delivered to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecApprovalRequestEvent {
    /// Tool call id that owns this approval.
    pub call_id: String,
    /// Specific approval id for nested approvals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    /// Turn id that owns this approval.
    pub turn_id: String,
    /// Unix timestamp in milliseconds when approval started.
    pub started_at_ms: i64,
    /// Command tokens to execute.
    pub command: Vec<String>,
    /// Working directory for the command.
    pub cwd: PathBuf,
    /// Optional approval reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Optional blocked network request context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_approval_context: Option<NetworkApprovalContext>,
    /// Optional command-prefix amendment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
    /// Optional network policy amendments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_network_policy_amendments: Option<Vec<NetworkPolicyAmendment>>,
    /// Optional extra permissions requested by the tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_permissions: Option<AdditionalPermissionProfile>,
    /// Ordered decisions that clients may show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_decisions: Option<Vec<ReviewDecision>>,
    /// Parsed command summary for display.
    pub parsed_cmd: Vec<ParsedCommand>,
}
```

- [ ] **Step 4: 导出 `approvals` 模块**

在 `crates/protocol/src/lib.rs` 增加：

```rust
pub mod approvals;
pub use approvals::*;
```

保持现有 `permission` 导出不删除，以便 ACP adapter 和旧调用点继续编译。

- [ ] **Step 5: 运行协议测试**

Run: `rtk cargo test -p protocol approvals`

Expected: `approvals` 模块测试通过。

- [ ] **Step 6: Checkpoint**

不提交。记录变更范围：`protocol` 新增 参考实现风格 approval 类型，旧 `permission::ReviewDecision` 仍保留。

---

### Task 2: 配置层引入 `AskForApproval`

**Files:**
- Modify: `crates/protocol/src/config.rs`
- Modify: `crates/config/src/config.rs`
- Test: `crates/config/tests/loading.rs`

- [ ] **Step 1: 写旧配置映射测试**

在 `crates/config/tests/loading.rs` 增加：

```rust
/// Legacy request_approval config resolves to enhanced OnRequest policy.
#[test]
fn legacy_request_approval_effective_policy_is_on_request() {
    let toml = r#"
active_model = "test/model"
approval = "request_approval"
"#;

    let config: config::AppConfig = toml::from_str(toml).expect("parse config");

    assert_eq!(
        config.effective_approval_policy(),
        protocol::AskForApproval::OnRequest
    );
}

/// Legacy yolo config preserves current no-prompt compatibility mode.
#[test]
fn legacy_yolo_effective_policy_is_never() {
    let toml = r#"
active_model = "test/model"
approval = "yolo"
"#;

    let config: config::AppConfig = toml::from_str(toml).expect("parse config");

    assert_eq!(
        config.effective_approval_policy(),
        protocol::AskForApproval::Never
    );
}
```

- [ ] **Step 2: 运行失败测试**

Run: `rtk cargo test -p config legacy_request_approval_effective_policy_is_on_request`

Expected: 编译失败，错误包含 `no method named effective_approval_policy` 或 `cannot find type AskForApproval`。

- [ ] **Step 3: 在 `protocol::config` 新增策略类型**

在 `crates/protocol/src/config.rs` 中加入：

```rust
/// enhanced approval policy for tool execution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AskForApproval {
    /// Ask for commands unless they are trusted by policy.
    #[serde(rename = "untrusted")]
    UnlessTrusted,
    /// Run sandboxed first and ask only after sandbox failure.
    OnFailure,
    /// Let tools request approval when needed.
    #[default]
    OnRequest,
    /// Fine-grained prompt enablement for approval categories.
    Granular(GranularApprovalConfig),
    /// Never ask the user; approval-required actions are denied or run by existing yolo compatibility.
    Never,
}

/// Fine-grained approval prompt enablement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GranularApprovalConfig {
    /// Whether sandbox/escalation approval prompts are allowed.
    pub sandbox_approval: bool,
    /// Whether execpolicy rule prompts are allowed.
    pub rules: bool,
    /// Whether skill script approval prompts are allowed.
    #[serde(default)]
    pub skill_approval: bool,
    /// Whether request_permissions prompts are allowed.
    #[serde(default)]
    pub request_permissions: bool,
    /// Whether MCP elicitation prompts are allowed.
    pub mcp_elicitations: bool,
}

impl From<ApprovalMode> for AskForApproval {
    /// Convert legacy approval modes into enhanced approval policy.
    fn from(value: ApprovalMode) -> Self {
        match value {
            ApprovalMode::RequestApproval => AskForApproval::OnRequest,
            ApprovalMode::Yolo => AskForApproval::Never,
        }
    }
}
```

更新 `ToolContext`：

```rust
/// Current tool-approval policy for the session.
pub approval_policy: AskForApproval,
```

本阶段同时保留旧字段并新增字段：

```rust
/// Current legacy approval mode for compatibility.
pub approval_mode: ApprovalMode,
/// Current enhanced approval policy for the session.
pub approval_policy: AskForApproval,
```

- [ ] **Step 4: 在 `AppConfig` 增加 `approval_policy` 字段**

在 `crates/config/src/config.rs` 的 `AppConfig` 中加入可选字段。使用 `Option` 是为了区分“用户没有配置新策略”和“用户显式配置新策略”，旧 `approval` 字段才能继续作为兼容来源。

```rust
/// enhanced tool approval policy.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub approval_policy: Option<AskForApproval>,
```

在 `impl Default for AppConfig` 中设为：

```rust
approval_policy: None,
```

在 `impl AppConfig` 中新增方法：

```rust
/// Return the enhanced approval policy after applying legacy compatibility.
#[must_use]
pub fn effective_approval_policy(&self) -> AskForApproval {
    self.approval_policy
        .unwrap_or_else(|| AskForApproval::from(self.approval))
}
```

- [ ] **Step 5: 修复 `ToolContext` 构造点**

在 `crates/kernel/src/turn.rs` 构造 `ToolContext` 的位置加入：

```rust
.approval_policy(ctx.approval.policy())
```

同时保留：

```rust
.approval_mode(ctx.approval.mode())
```

- [ ] **Step 6: 运行配置测试**

Run: `rtk cargo test -p config legacy_request_approval_effective_policy_is_on_request legacy_yolo_effective_policy_is_never`

Expected: 两个测试通过。

- [ ] **Step 7: Checkpoint**

不提交。确认旧配置仍可解析，新策略字段可通过 `effective_approval_policy()` 供后续 kernel 使用。

---

### Task 3: 实现 session `ApprovalStore`

**Files:**
- Modify: `crates/kernel/src/approval.rs`
- Create: `crates/kernel/src/approval/store.rs`
- Modify: `crates/kernel/src/session.rs`
- Modify: `crates/kernel/src/turn.rs`
- Test: `crates/kernel/src/approval/store.rs`

- [ ] **Step 1: 写 `ApprovalStore` 测试**

创建 `crates/kernel/src/approval/store.rs`，先写测试和类型使用：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ReviewDecision;
    use serde::Serialize;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[derive(Serialize)]
    struct TestKey {
        command: Vec<String>,
    }

    /// Verifies cached approvals require every key to be approved for session.
    #[tokio::test]
    async fn cached_approval_requires_all_keys() {
        let store = Arc::new(Mutex::new(ApprovalStore::default()));
        store.lock().await.put(
            TestKey {
                command: vec!["cargo".to_string()],
            },
            ReviewDecision::ApprovedForSession,
        );

        let decision = with_cached_approval(
            &store,
            vec![
                TestKey {
                    command: vec!["cargo".to_string()],
                },
                TestKey {
                    command: vec!["test".to_string()],
                },
            ],
            || async { ReviewDecision::Approved },
        )
        .await;

        assert_eq!(decision, ReviewDecision::Approved);
    }

    /// Verifies ApprovedForSession writes every key into the cache.
    #[tokio::test]
    async fn approved_for_session_caches_all_keys() {
        let store = Arc::new(Mutex::new(ApprovalStore::default()));
        let keys = vec![
            TestKey {
                command: vec!["cargo".to_string()],
            },
            TestKey {
                command: vec!["test".to_string()],
            },
        ];

        let decision = with_cached_approval(&store, keys, || async {
            ReviewDecision::ApprovedForSession
        })
        .await;

        assert_eq!(decision, ReviewDecision::ApprovedForSession);
        assert!(matches!(
            store.lock().await.get(&TestKey {
                command: vec!["cargo".to_string()]
            }),
            Some(ReviewDecision::ApprovedForSession)
        ));
    }
}
```

- [ ] **Step 2: 运行失败测试**

Run: `rtk cargo test -p kernel cached_approval_requires_all_keys`

Expected: 编译失败，`ApprovalStore` / `with_cached_approval` 未定义。

- [ ] **Step 3: 将 `approval.rs` 改为模块入口**

如果 `crates/kernel/src/approval.rs` 不能同时作为目录模块，改为：

1. Create: `crates/kernel/src/approval/mod.rs`
2. Move existing `ApprovalPolicy` 内容到 `mod.rs`
3. Delete old file via patch when module目录稳定

目标内容：

```rust
//! Approval policy and session-scoped approval cache.

mod store;

pub use protocol::{ApprovalMode, AskForApproval};
pub use store::{ApprovalStore, with_cached_approval};

use std::sync::Mutex;

/// Thread-safe approval policy for a session.
pub struct ApprovalPolicy {
    mode: Mutex<ApprovalMode>,
    policy: Mutex<AskForApproval>,
}

impl ApprovalPolicy {
    /// Create a new policy from a legacy approval mode.
    pub fn new(mode: ApprovalMode) -> Self {
        Self {
            mode: Mutex::new(mode),
            policy: Mutex::new(mode.into()),
        }
    }

    /// Return the current legacy approval mode.
    pub fn mode(&self) -> ApprovalMode {
        *self
            .mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Return the current enhanced approval policy.
    pub fn policy(&self) -> AskForApproval {
        *self
            .policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Change the approval mode and synchronized enhanced policy.
    pub fn set_mode(&self, mode: ApprovalMode) {
        *self
            .mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = mode;
        *self
            .policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = mode.into();
    }
}
```

- [ ] **Step 4: 实现 store**

在 `crates/kernel/src/approval/store.rs` 写入：

```rust
//! Session-scoped approval cache.

use std::collections::HashMap;
use std::sync::Arc;

use protocol::ReviewDecision;
use serde::Serialize;
use tokio::sync::Mutex;

/// Session-scoped approval cache keyed by serialized approval keys.
#[derive(Clone, Default, Debug)]
pub struct ApprovalStore {
    /// Serialized key to cached decision.
    map: HashMap<String, ReviewDecision>,
}

impl ApprovalStore {
    /// Load a cached decision for a serializable approval key.
    pub fn get<K>(&self, key: &K) -> Option<ReviewDecision>
    where
        K: Serialize,
    {
        let serialized = serde_json::to_string(key).ok()?;
        self.map.get(&serialized).cloned()
    }

    /// Store a cached decision for a serializable approval key.
    pub fn put<K>(&mut self, key: K, value: ReviewDecision)
    where
        K: Serialize,
    {
        if let Ok(serialized) = serde_json::to_string(&key) {
            self.map.insert(serialized, value);
        }
    }
}

/// Return a cached session approval or fetch and cache a new decision.
pub async fn with_cached_approval<K, F, Fut>(
    store: &Arc<Mutex<ApprovalStore>>,
    keys: Vec<K>,
    fetch: F,
) -> ReviewDecision
where
    K: Serialize,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ReviewDecision>,
{
    if keys.is_empty() {
        return fetch().await;
    }

    let already_approved = {
        let store = store.lock().await;
        keys.iter()
            .all(|key| matches!(store.get(key), Some(ReviewDecision::ApprovedForSession)))
    };

    if already_approved {
        return ReviewDecision::ApprovedForSession;
    }

    let decision = fetch().await;

    if matches!(decision, ReviewDecision::ApprovedForSession) {
        let mut store = store.lock().await;
        for key in keys {
            store.put(key, ReviewDecision::ApprovedForSession);
        }
    }

    decision
}
```

- [ ] **Step 5: 将 store 挂到 session runtime**

在 `crates/kernel/src/session.rs` 的 session runtime/thread handle 中新增字段：

```rust
pub approval_store: Arc<tokio::sync::Mutex<crate::approval::ApprovalStore>>,
```

在 `new_session_runtime` 或现有 session 构造处创建：

```rust
let approval_store = Arc::new(tokio::sync::Mutex::new(
    crate::approval::ApprovalStore::default(),
));
```

把它传入 `Session` 和 `Thread` builder。

- [ ] **Step 6: `TurnContext` 增加 store**

在 `crates/kernel/src/turn.rs` 的 `TurnContext` 中加入：

```rust
/// Session-scoped approval cache.
#[builder(default)]
pub approval_store: Arc<tokio::sync::Mutex<crate::approval::ApprovalStore>>,
```

构造 `TurnContext` 时传入：

```rust
.approval_store(Arc::clone(&rt.approval_store))
```

- [ ] **Step 7: 运行 store 测试**

Run: `rtk cargo test -p kernel approval::store`

Expected: `ApprovalStore` 两个测试通过。

- [ ] **Step 8: Checkpoint**

不提交。此时 session 已有缓存能力，但 `dispatch_tool` 还未接入。

---

### Task 4: 增加 object-safe `ToolInvocation` 和 typed runtime adapter

**Files:**
- Create: `crates/tools/src/invocation.rs`
- Modify: `crates/tools/src/lib.rs`
- Test: `crates/tools/src/invocation.rs`

- [ ] **Step 1: 写 invocation 基础测试**

创建 `crates/tools/src/invocation.rs` 并先加入测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AgentPath, ApprovalMode, AskForApproval, SessionId, ToolContext};
    use std::path::PathBuf;

    /// Verifies generic invocations preserve raw arguments for fallback tools.
    #[test]
    fn generic_invocation_preserves_raw_arguments() {
        let invocation = ToolInvocation::generic(
            "call-1",
            "shell",
            serde_json::json!({ "command": "pwd" }),
            &ToolContext::builder()
                .session_id(SessionId::from("s1"))
                .cwd(PathBuf::from("/tmp"))
                .agent_path(AgentPath::root())
                .approval_mode(ApprovalMode::RequestApproval)
                .approval_policy(AskForApproval::OnRequest)
                .build(),
        );

        assert_eq!(invocation.call_id, "call-1");
        assert_eq!(invocation.tool_name, "shell");
        assert_eq!(invocation.raw_arguments["command"], "pwd");
    }
}
```

- [ ] **Step 2: 运行失败测试**

Run: `rtk cargo test -p tools generic_invocation_preserves_raw_arguments`

Expected: 编译失败，`ToolInvocation` 不存在。

- [ ] **Step 3: 实现 `invocation.rs`**

```rust
//! Tool invocation envelope and typed runtime adapter traits.

use async_trait::async_trait;
use serde::Serialize;
use std::path::PathBuf;

use protocol::{AskForApproval, ToolContext, ToolStreamItem, TurnId};

/// Unified fact record for a model-requested tool call.
#[derive(Debug, Clone)]
pub struct ToolInvocation {
    /// Tool call id from the provider.
    pub call_id: String,
    /// Optional nested approval id.
    pub approval_id: Option<String>,
    /// Turn id that owns this invocation.
    pub turn_id: Option<TurnId>,
    /// Tool name selected by the model.
    pub tool_name: String,
    /// Raw JSON arguments emitted by the provider.
    pub raw_arguments: serde_json::Value,
    /// Working directory for this invocation.
    pub cwd: PathBuf,
    /// Tool-specific approval metadata.
    pub approval: ToolApprovalInvocation,
}

impl ToolInvocation {
    /// Build a generic invocation for tools that have not provided typed metadata yet.
    #[must_use]
    pub fn generic(
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        raw_arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            approval_id: None,
            turn_id: None,
            tool_name: tool_name.into(),
            raw_arguments,
            cwd: ctx.cwd.clone(),
            approval: ToolApprovalInvocation::Generic(GenericApprovalInvocation),
        }
    }
}

/// Tool-specific approval metadata attached to a tool invocation.
#[derive(Debug, Clone)]
pub enum ToolApprovalInvocation {
    /// Shell command metadata.
    Shell(ShellApprovalInvocation),
    /// Apply-patch metadata.
    ApplyPatch(ApplyPatchApprovalInvocation),
    /// MCP tool metadata.
    Mcp(McpApprovalInvocation),
    /// Fallback metadata for tools not yet migrated.
    Generic(GenericApprovalInvocation),
}

/// Shell approval metadata.
#[derive(Debug, Clone, Serialize)]
pub struct ShellApprovalInvocation {
    /// Canonical command tokens.
    pub command: Vec<String>,
}

/// Apply-patch approval metadata.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyPatchApprovalInvocation {
    /// Paths touched by the patch.
    pub paths: Vec<PathBuf>,
}

/// MCP approval metadata.
#[derive(Debug, Clone, Serialize)]
pub struct McpApprovalInvocation {
    /// MCP server name.
    pub server: String,
    /// MCP tool name.
    pub tool: String,
    /// Stable hash of arguments.
    pub arguments_hash: String,
}

/// Fallback approval metadata.
#[derive(Debug, Clone, Serialize)]
pub struct GenericApprovalInvocation;

/// Tool execution output stream type.
pub type ToolExecution = std::pin::Pin<
    Box<dyn futures::stream::Stream<Item = ToolStreamItem> + Send>,
>;

/// Error returned while constructing a tool invocation.
#[derive(Debug, thiserror::Error)]
pub enum ToolInvocationError {
    /// Tool arguments could not be parsed.
    #[error("invalid tool arguments: {0}")]
    InvalidArguments(String),
}

/// enhanced typed runtime adapter used inside concrete tools.
#[async_trait]
pub trait TypedToolRuntime: Send + Sync {
    /// Strong request type for this runtime.
    type Request: Send + Sync;
    /// Serializable approval key used for session approval cache.
    type ApprovalKey: Serialize + Send + Sync;

    /// Parse raw JSON arguments into a strong request.
    fn parse_request(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<Self::Request, ToolInvocationError>;

    /// Return approval keys for a parsed request.
    fn approval_keys(&self, request: &Self::Request) -> Vec<Self::ApprovalKey>;

    /// Return the approval requirement for a parsed request.
    fn exec_approval_requirement(
        &self,
        request: &Self::Request,
        policy: AskForApproval,
    ) -> crate::ExecApprovalRequirement;
}
```

- [ ] **Step 4: 在 `tools::lib` 导出 invocation**

在 `crates/tools/src/lib.rs` 加：

```rust
pub mod invocation;
pub use invocation::{
    ApplyPatchApprovalInvocation, GenericApprovalInvocation, McpApprovalInvocation,
    ShellApprovalInvocation, ToolApprovalInvocation, ToolExecution, ToolInvocation,
    ToolInvocationError, TypedToolRuntime,
};
```

- [ ] **Step 5: 新增 object-safe `Tool` 默认方法**

在 `Tool` trait 中加入默认方法，保留旧方法避免一次性迁移所有工具：

```rust
/// Build a unified invocation envelope for this tool call.
fn build_invocation(
    &self,
    call_id: &str,
    arguments: serde_json::Value,
    ctx: &ToolContext,
) -> Result<ToolInvocation, ToolInvocationError> {
    Ok(ToolInvocation::generic(call_id, self.name(), arguments, ctx))
}

/// Return the approval requirement for a built invocation.
fn exec_approval_requirement(
    &self,
    invocation: &ToolInvocation,
    ctx: &ToolContext,
) -> ExecApprovalRequirement {
    if self.needs_approval(&invocation.raw_arguments, ctx) {
        ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        }
    } else {
        ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
            proposed_execpolicy_amendment: None,
        }
    }
}

/// Execute a previously built invocation.
async fn execute_invocation(
    &self,
    invocation: ToolInvocation,
    ctx: &ToolContext,
) -> Result<ToolExecution, String> {
    self.execute_streaming(invocation.raw_arguments, ctx).await
}
```

在 `tools` crate 定义 `ExecApprovalRequirement`，因为它描述 tool runtime 的执行前决策，不属于跨进程协议事件：

```rust
/// Approval requirement returned before tool execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecApprovalRequirement {
    /// No prompt is required.
    Skip {
        /// Whether sandbox should be bypassed on first attempt.
        bypass_sandbox: bool,
        /// Optional command-prefix amendment for later prompts.
        proposed_execpolicy_amendment: Option<protocol::ExecPolicyAmendment>,
    },
    /// User approval is required.
    NeedsApproval {
        /// Human-readable approval reason.
        reason: Option<String>,
        /// Optional command-prefix amendment.
        proposed_execpolicy_amendment: Option<protocol::ExecPolicyAmendment>,
    },
    /// Execution is forbidden by policy.
    Forbidden {
        /// Model-facing rejection reason.
        reason: String,
    },
}
```

- [ ] **Step 6: 运行 tools 测试**

Run: `rtk cargo test -p tools generic_invocation_preserves_raw_arguments`

Expected: 测试通过。

- [ ] **Step 7: Checkpoint**

不提交。所有 tools 仍通过默认 adapter 编译，shell、apply_patch、MCP 在本计划的后续任务中迁移 typed metadata。

---

### Task 5: shell typed invocation 和 approval key

**Files:**
- Modify: `crates/tools/src/builtin/shell.rs`
- Test: `crates/tools/src/builtin/shell.rs`

- [ ] **Step 1: 写 shell invocation 测试**

在 shell tests 模块中增加：

```rust
/// Verifies shell invocations expose command tokens for approval.
#[tokio::test]
async fn shell_build_invocation_exposes_command_tokens() {
    let tool = ShellCommand::default();
    let ctx = ToolContext::builder()
        .session_id(protocol::SessionId::from("s1"))
        .cwd(std::path::PathBuf::from("/repo"))
        .agent_path(protocol::AgentPath::root())
        .approval_mode(protocol::ApprovalMode::RequestApproval)
        .approval_policy(protocol::AskForApproval::OnRequest)
        .build();

    let invocation = tool
        .build_invocation(
            "call-1",
            serde_json::json!({ "command": "cargo test -p tools" }),
            &ctx,
        )
        .expect("shell invocation");

    let tools::ToolApprovalInvocation::Shell(shell) = invocation.approval else {
        panic!("expected shell approval metadata");
    };
    assert_eq!(shell.command, vec!["cargo", "test", "-p", "tools"]);
}
```

- [ ] **Step 2: 运行失败测试**

Run: `rtk cargo test -p tools shell_build_invocation_exposes_command_tokens`

Expected: 测试失败，当前 `ShellCommand` 还返回 generic invocation。

- [ ] **Step 3: 增加 shell request/key 类型**

在 `crates/tools/src/builtin/shell.rs` 增加：

```rust
/// Parsed shell request used by approval and execution.
#[derive(Debug, Clone)]
struct ShellRequest {
    /// User command string passed to the shell.
    command: String,
    /// Canonical command tokens used by approval.
    command_tokens: Vec<String>,
    /// Working directory for the command.
    cwd: PathBuf,
}

/// Session approval key for shell command requests.
#[derive(Debug, Clone, serde::Serialize)]
struct ShellApprovalKey {
    /// Canonical command tokens.
    command: Vec<String>,
    /// Working directory for this command.
    cwd: PathBuf,
}
```

- [ ] **Step 4: 增加 shell command tokenization**

第一版使用 `shlex::split`。如果 workspace 没有 `shlex`，在 `crates/tools/Cargo.toml` 增加 workspace dependency；如果不想新增依赖，使用简单 whitespace split 并在注释中声明第一阶段只支持简单命令。推荐使用已在 参考实现 中使用的 `shlex`。

```rust
/// Split a simple shell command into approval tokens.
fn shell_command_tokens(command: &str) -> Vec<String> {
    shlex::split(command).unwrap_or_else(|| {
        command
            .split_whitespace()
            .map(ToString::to_string)
            .collect()
    })
}
```

- [ ] **Step 5: 覆盖 `build_invocation`**

在 `impl Tool for ShellCommand` 中加入：

```rust
fn build_invocation(
    &self,
    call_id: &str,
    arguments: serde_json::Value,
    ctx: &crate::ToolContext,
) -> Result<crate::ToolInvocation, crate::ToolInvocationError> {
    let args: ShellArgs = serde_json::from_value(arguments.clone())
        .map_err(|error| crate::ToolInvocationError::InvalidArguments(error.to_string()))?;
    let command = args
        .command
        .clone()
        .or(args.cmd.clone())
        .ok_or_else(|| crate::ToolInvocationError::InvalidArguments(
            "missing command".to_string(),
        ))?;
    let cwd = args
        .cwd
        .clone()
        .or(args.workdir.clone())
        .unwrap_or_else(|| ctx.cwd.clone());
    let command_tokens = shell_command_tokens(&command);

    Ok(crate::ToolInvocation {
        call_id: call_id.to_string(),
        approval_id: None,
        turn_id: None,
        tool_name: self.name().to_string(),
        raw_arguments: arguments,
        cwd,
        approval: crate::ToolApprovalInvocation::Shell(
            crate::ShellApprovalInvocation {
                command: command_tokens,
            },
        ),
    })
}
```

- [ ] **Step 6: 覆盖 approval requirement**

```rust
fn exec_approval_requirement(
    &self,
    invocation: &crate::ToolInvocation,
    _ctx: &crate::ToolContext,
) -> crate::ExecApprovalRequirement {
    let proposed_execpolicy_amendment = match &invocation.approval {
        crate::ToolApprovalInvocation::Shell(shell) if !shell.command.is_empty() => {
            Some(protocol::ExecPolicyAmendment::new(shell.command.clone()))
        }
        _ => None,
    };

    crate::ExecApprovalRequirement::NeedsApproval {
        reason: None,
        proposed_execpolicy_amendment,
    }
}
```

Task 9 会将该临时 requirement 来源替换为 execpolicy evaluator；本阶段只保证 shell 能产生 typed metadata。

- [ ] **Step 7: 运行 shell 测试**

Run: `rtk cargo test -p tools shell_build_invocation_exposes_command_tokens`

Expected: 测试通过。

- [ ] **Step 8: Checkpoint**

不提交。shell 已能构造 typed invocation 和 amendment 候选。

---

### Task 6: apply_patch approval key

**Files:**
- Modify: `crates/tools/src/builtin/fs/legacy/apply_patch/mod.rs`
- Test: `crates/tools/src/builtin/fs/legacy/apply_patch/mod.rs`

- [ ] **Step 1: 写 apply_patch invocation 测试**

在 apply_patch tests 模块中增加：

```rust
/// Verifies apply_patch invocations expose touched paths for session approval.
#[tokio::test]
async fn apply_patch_build_invocation_exposes_paths() {
    let tool = ApplyPatch::new();
    let ctx = protocol::ToolContext::builder()
        .session_id(protocol::SessionId::from("s1"))
        .cwd(std::path::PathBuf::from("/repo"))
        .agent_path(protocol::AgentPath::root())
        .approval_mode(protocol::ApprovalMode::RequestApproval)
        .approval_policy(protocol::AskForApproval::OnRequest)
        .build();

    let invocation = tool
        .build_invocation(
            "call-1",
            serde_json::json!({
                "patchText": "*** Begin Patch\n*** Add File: src/a.rs\n+fn main() {}\n*** End Patch"
            }),
            &ctx,
        )
        .expect("patch invocation");

    let tools::ToolApprovalInvocation::ApplyPatch(patch) = invocation.approval else {
        panic!("expected apply_patch approval metadata");
    };
    assert_eq!(patch.paths, vec![std::path::PathBuf::from("src/a.rs")]);
}
```

- [ ] **Step 2: 运行失败测试**

Run: `rtk cargo test -p tools apply_patch_build_invocation_exposes_paths`

Expected: 失败，当前 apply_patch 还没有 typed invocation。

- [ ] **Step 3: 增加路径提取方法**

在 apply_patch module 中新增关联函数，避免 helper 泛滥：

```rust
impl ApplyPatch {
    /// Extract touched file paths from a patch text for approval keys.
    fn approval_paths(patch_text: &str) -> Vec<PathBuf> {
        patch_text
            .lines()
            .filter_map(|line| {
                line.strip_prefix("*** Add File: ")
                    .or_else(|| line.strip_prefix("*** Update File: "))
                    .or_else(|| line.strip_prefix("*** Delete File: "))
                    .or_else(|| line.strip_prefix("*** Move to: "))
            })
            .map(PathBuf::from)
            .collect()
    }
}
```

- [ ] **Step 4: 覆盖 `build_invocation`**

在 `impl Tool for ApplyPatch` 中加入：

```rust
fn build_invocation(
    &self,
    call_id: &str,
    arguments: serde_json::Value,
    ctx: &crate::ToolContext,
) -> Result<crate::ToolInvocation, crate::ToolInvocationError> {
    let patch_text = arguments
        .get("patchText")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| crate::ToolInvocationError::InvalidArguments(
            "missing patchText".to_string(),
        ))?;
    let paths = Self::approval_paths(patch_text);

    Ok(crate::ToolInvocation {
        call_id: call_id.to_string(),
        approval_id: None,
        turn_id: None,
        tool_name: self.name().to_string(),
        raw_arguments: arguments,
        cwd: ctx.cwd.clone(),
        approval: crate::ToolApprovalInvocation::ApplyPatch(
            crate::ApplyPatchApprovalInvocation { paths },
        ),
    })
}
```

- [ ] **Step 5: approval requirement 保持需要审批**

```rust
fn exec_approval_requirement(
    &self,
    _invocation: &crate::ToolInvocation,
    _ctx: &crate::ToolContext,
) -> crate::ExecApprovalRequirement {
    crate::ExecApprovalRequirement::NeedsApproval {
        reason: None,
        proposed_execpolicy_amendment: None,
    }
}
```

- [ ] **Step 6: 运行 apply_patch 测试**

Run: `rtk cargo test -p tools apply_patch_build_invocation_exposes_paths`

Expected: 测试通过。

- [ ] **Step 7: Checkpoint**

不提交。apply_patch 已能提供路径级 approval metadata。

---

### Task 7: kernel dispatch 接入 invocation 和 session cache

**Files:**
- Modify: `crates/kernel/src/turn.rs`
- Modify: `crates/kernel/src/session.rs`
- Test: `crates/kernel/src/turn.rs`

- [ ] **Step 1: 写 dispatch session cache 测试**

在 `turn.rs` tests 中增加一个 fake tool。测试结构参考现有 `Tool` fake tests，核心断言是第二次相同 key 不发送 approval event。

```rust
/// Verifies ApprovedForSession skips the second identical shell approval.
#[tokio::test]
async fn dispatch_tool_uses_session_approval_cache() {
    let approval_store = Arc::new(tokio::sync::Mutex::new(
        crate::approval::ApprovalStore::default(),
    ));
    approval_store.lock().await.put(
        ShellApprovalCacheTestKey {
            command: vec!["pwd".to_string()],
            cwd: PathBuf::from("/repo"),
        },
        ReviewDecision::ApprovedForSession,
    );

    let decision = crate::approval::with_cached_approval(
        &approval_store,
        vec![ShellApprovalCacheTestKey {
            command: vec!["pwd".to_string()],
            cwd: PathBuf::from("/repo"),
        }],
        || async { ReviewDecision::Denied },
    )
    .await;

    assert_eq!(decision, ReviewDecision::ApprovedForSession);
}

#[derive(serde::Serialize)]
struct ShellApprovalCacheTestKey {
    command: Vec<String>,
    cwd: PathBuf,
}
```

本测试先验证核心 cache helper，不直接构造完整 LLM turn，避免测试过大。

- [ ] **Step 2: 运行测试**

Run: `rtk cargo test -p kernel dispatch_tool_uses_session_approval_cache`

Expected: 通过。如果不通过，先修正 Task 3 的 store 接入。

- [ ] **Step 3: 在 dispatch 中构造 invocation**

替换 `dispatch_tool` 中：

```rust
let needs_approval = tool.needs_approval(arguments, tool_ctx);
```

为：

```rust
let invocation = tool
    .build_invocation(call_id, arguments.clone(), tool_ctx)
    .map_err(|error| KernelError::Internal(anyhow::anyhow!(error)))?;
let approval_requirement = tool.exec_approval_requirement(&invocation, tool_ctx);
```

- [ ] **Step 4: 按 requirement 分支**

在 `dispatch_tool` 中新增 match：

```rust
let approval_decision = if ctx.approval.mode() == protocol::ApprovalMode::Yolo {
    ReviewDecision::Approved
} else {
    match approval_requirement {
    tools::ExecApprovalRequirement::Skip { .. } => ReviewDecision::Approved,
    tools::ExecApprovalRequirement::Forbidden { reason } => {
        let _ = tx_event.send(Event::tool_call_update(
            ctx.session_id.clone(),
            call_id,
            Some(reason.clone()),
            Some(ToolCallStatus::Failed),
        ));
        return Ok((reason, false));
    }
    tools::ExecApprovalRequirement::NeedsApproval {
        reason,
        proposed_execpolicy_amendment,
    } if ctx.approval.policy() != protocol::AskForApproval::Never => {
        request_tool_approval(
            ctx,
            tx_event,
            call_id,
            tool_name,
            invocation.clone(),
            reason,
            proposed_execpolicy_amendment,
        )
        .await?
    }
    tools::ExecApprovalRequirement::NeedsApproval { .. } => ReviewDecision::Denied,
    }
};
```

- [ ] **Step 5: 将 decision 判断改为 参考实现风格**

```rust
match approval_decision {
    ReviewDecision::Abort => return Err(KernelError::Cancelled),
    ReviewDecision::Denied | ReviewDecision::TimedOut => {
        let msg = "rejected by user".to_string();
        let _ = tx_event.send(Event::tool_call_update(
            ctx.session_id.clone(),
            call_id,
            Some(msg.clone()),
            Some(ToolCallStatus::Failed),
        ));
        return Ok((msg, false));
    }
    ReviewDecision::Approved
    | ReviewDecision::ApprovedForSession
    | ReviewDecision::ApprovedExecpolicyAmendment { .. }
    | ReviewDecision::NetworkPolicyAmendment { .. } => {}
}
```

- [ ] **Step 6: 执行 invocation**

替换：

```rust
tool.execute_streaming(arguments.clone(), tool_ctx).await
```

为：

```rust
tool.execute_invocation(invocation, tool_ctx).await
```

- [ ] **Step 7: 运行 kernel 测试**

Run: `rtk cargo test -p kernel dispatch_tool_uses_session_approval_cache`

Expected: 测试通过，编译错误全部修复。

- [ ] **Step 8: Checkpoint**

不提交。kernel 已经从 bool approval 迁移到 invocation + requirement 的主路径。

---

### Task 8: 新 approval event 和 TUI available_decisions

**Files:**
- Modify: `crates/protocol/src/event.rs`
- Modify: `crates/tui/src/ui/approval.rs`
- Modify: `crates/tui/src/app.rs`
- Test: `crates/tui/src/ui/approval.rs`

- [ ] **Step 1: 写 TUI decision mapping 测试**

更新 `approval_keys_map_to_decisions`，目标映射为 参考实现风格：

```rust
/// Verifies approval keys map to enhanced decisions.
#[test]
fn approval_keys_map_to_review_decisions() {
    let allow = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    let session = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
    let reject = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
    let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);

    assert_eq!(decision_for_key(allow), Some(ApprovalDecision::Approved));
    assert_eq!(
        decision_for_key(session),
        Some(ApprovalDecision::ApprovedForSession)
    );
    assert_eq!(decision_for_key(reject), Some(ApprovalDecision::Denied));
    assert_eq!(decision_for_key(escape), Some(ApprovalDecision::Abort));
}
```

- [ ] **Step 2: 运行失败测试**

Run: `rtk cargo test -p tui approval_keys_map_to_review_decisions`

Expected: 失败，当前 UI 只有 allow/reject once。

- [ ] **Step 3: 更新 `ApprovalDecision`**

在 `crates/tui/src/ui/approval.rs` 中改为：

```rust
/// User decision selected from the approval overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Approve this request once.
    Approved,
    /// Approve matching requests for this session.
    ApprovedForSession,
    /// Approve and persist a command-prefix amendment.
    ApprovedExecpolicyAmendment(protocol::ExecPolicyAmendment),
    /// Deny this request.
    Denied,
    /// Abort the current turn.
    Abort,
}
```

如果 ACP `PermissionOptionId` 仍需要映射，保留方法：

```rust
/// Returns the ACP permission option id used by the local ACP server.
pub fn option_id(&self) -> PermissionOptionId {
    match self {
        ApprovalDecision::Approved => PermissionOptionId::new("allow_once"),
        ApprovalDecision::ApprovedForSession
        | ApprovalDecision::ApprovedExecpolicyAmendment(_) => {
            PermissionOptionId::new("allow_always")
        }
        ApprovalDecision::Denied | ApprovalDecision::Abort => {
            PermissionOptionId::new("reject_once")
        }
    }
}
```

- [ ] **Step 4: 更新 key mapping**

```rust
pub fn decision_for_key(key: KeyEvent) -> Option<ApprovalDecision> {
    if key.kind != KeyEventKind::Press {
        return None;
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char('a' | 'y'), KeyModifiers::NONE) => {
            Some(ApprovalDecision::Approved)
        }
        (KeyCode::Char('s'), KeyModifiers::NONE) => {
            Some(ApprovalDecision::ApprovedForSession)
        }
        (KeyCode::Char('r' | 'n'), KeyModifiers::NONE) => {
            Some(ApprovalDecision::Denied)
        }
        (KeyCode::Esc, _) => Some(ApprovalDecision::Abort),
        _ => None,
    }
}
```

- [ ] **Step 5: 转换到 protocol decision**

在 `app.rs` 的 `handle_approval_key` 前新增转换：

```rust
impl From<ApprovalDecision> for protocol::ReviewDecision {
    /// Convert a TUI approval decision into the protocol decision.
    fn from(value: ApprovalDecision) -> Self {
        match value {
            ApprovalDecision::Approved => protocol::ReviewDecision::Approved,
            ApprovalDecision::ApprovedForSession => {
                protocol::ReviewDecision::ApprovedForSession
            }
            ApprovalDecision::ApprovedExecpolicyAmendment(amendment) => {
                protocol::ReviewDecision::ApprovedExecpolicyAmendment {
                    proposed_execpolicy_amendment: amendment,
                }
            }
            ApprovalDecision::Denied => protocol::ReviewDecision::Denied,
            ApprovalDecision::Abort => protocol::ReviewDecision::Abort,
        }
    }
}
```

- [ ] **Step 6: 让 overlay 使用 `available_decisions`**

给 `PendingApproval` 增加 decisions 字段：

```rust
/// Stores the currently displayed approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    /// Local request id used to look up the one-shot ACP responder.
    request_id: u64,
    /// Short heading rendered by the approval overlay.
    title: String,
    /// Detailed text rendered by the approval overlay.
    body: String,
    /// Decisions the overlay may present.
    available_decisions: Vec<ApprovalDecision>,
}
```

为当前 ACP fallback 设置默认 decisions：

```rust
let available_decisions = vec![
    ApprovalDecision::Approved,
    ApprovalDecision::ApprovedForSession,
    ApprovalDecision::Denied,
    ApprovalDecision::Abort,
];
```

internal `ExecApprovalRequestEvent` 进入 TUI 时，必须从 event 的 `available_decisions` 转换，不允许 TUI 自行添加不存在的 option。

- [ ] **Step 7: 更新 overlay 文案**

将 footer 从：

```rust
"[a] allow once   [r] reject"
```

改为：

```rust
"[a] approve   [s] session   [r] reject   [esc] abort"
```

- [ ] **Step 8: 运行 TUI 测试**

Run: `rtk cargo test -p tui approval_keys_map_to_review_decisions`

Expected: 测试通过。

- [ ] **Step 9: Checkpoint**

不提交。TUI 已使用 参考实现风格决策，并且 overlay state 已有 `available_decisions` 边界。

---

### Task 9: execpolicy parser/writer 和 shell amendment

**Files:**
- Create: `crates/kernel/src/exec_policy.rs`
- Modify: `crates/kernel/src/lib.rs`
- Modify: `crates/kernel/src/turn.rs`
- Test: `crates/kernel/src/exec_policy.rs`

- [ ] **Step 1: 写 writer 测试**

创建 `crates/kernel/src/exec_policy.rs`，先写测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ExecPolicyAmendment;
    use tempfile::tempdir;

    /// Verifies prefix amendments create a default rules file.
    #[tokio::test]
    async fn append_amendment_creates_default_rules_file() {
        let dir = tempdir().expect("tempdir");
        let manager = ExecPolicyManager::new(dir.path().to_path_buf());
        let amendment = ExecPolicyAmendment::new(vec![
            "cargo".to_string(),
            "test".to_string(),
        ]);

        manager
            .append_amendment_and_update(&amendment)
            .await
            .expect("append amendment");

        let contents = std::fs::read_to_string(
            dir.path().join("rules").join("default.rules"),
        )
        .expect("rules file");
        assert_eq!(
            contents,
            "prefix_rule(pattern=[\"cargo\", \"test\"], decision=\"allow\")\n"
        );
    }

    /// Verifies duplicate amendments are not written twice.
    #[tokio::test]
    async fn append_amendment_dedupes_existing_rule() {
        let dir = tempdir().expect("tempdir");
        let manager = ExecPolicyManager::new(dir.path().to_path_buf());
        let amendment = ExecPolicyAmendment::new(vec!["cargo".to_string()]);

        manager.append_amendment_and_update(&amendment).await.expect("first append");
        manager.append_amendment_and_update(&amendment).await.expect("second append");

        let contents = std::fs::read_to_string(
            dir.path().join("rules").join("default.rules"),
        )
        .expect("rules file");
        assert_eq!(
            contents.lines().filter(|line| line.contains("prefix_rule")).count(),
            1
        );
    }
}
```

- [ ] **Step 2: 运行失败测试**

Run: `rtk cargo test -p kernel append_amendment_creates_default_rules_file`

Expected: 编译失败，`ExecPolicyManager` 不存在。

- [ ] **Step 3: 实现 manager 和 writer**

```rust
//! Minimal enhanced execpolicy support.

use std::path::PathBuf;

use protocol::ExecPolicyAmendment;
use tokio::sync::Semaphore;

/// Manages project-level execpolicy rules.
pub struct ExecPolicyManager {
    claw_home: PathBuf,
    update_lock: Semaphore,
}

impl ExecPolicyManager {
    /// Create an execpolicy manager rooted at the given claw home.
    #[must_use]
    pub fn new(claw_home: PathBuf) -> Self {
        Self {
            claw_home,
            update_lock: Semaphore::new(1),
        }
    }

    /// Append an allow-prefix amendment and refresh in-memory policy.
    pub async fn append_amendment_and_update(
        &self,
        amendment: &ExecPolicyAmendment,
    ) -> anyhow::Result<()> {
        let _guard = self.update_lock.acquire().await?;
        if amendment.command.is_empty() {
            anyhow::bail!("prefix rule requires at least one token");
        }

        let policy_path = self.claw_home.join("rules").join("default.rules");
        let line = allow_prefix_rule_line(&amendment.command)?;
        tokio::task::spawn_blocking(move || append_unique_line(policy_path, line))
            .await??;

        Ok(())
    }
}

/// Format a prefix_rule line with JSON-escaped command tokens.
fn allow_prefix_rule_line(command: &[String]) -> anyhow::Result<String> {
    let tokens = command
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!(
        "prefix_rule(pattern=[{}], decision=\"allow\")",
        tokens.join(", ")
    ))
}

/// Append a unique line to a rules file, creating parent directories.
fn append_unique_line(path: PathBuf, line: String) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };

    if contents.lines().any(|existing| existing == line) {
        return Ok(());
    }

    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(&line);
    contents.push('\n');
    std::fs::write(path, contents)?;
    Ok(())
}
```

- [ ] **Step 4: 导出 module**

在 `crates/kernel/src/lib.rs` 加：

```rust
pub mod exec_policy;
```

- [ ] **Step 5: 在 approval response 时处理 amendment**

在 `request_tool_approval` 返回 decision 后的处理位置加入：

```rust
if let ReviewDecision::ApprovedExecpolicyAmendment {
    proposed_execpolicy_amendment,
} = &decision
{
    if let Err(error) = ctx
        .exec_policy
        .append_amendment_and_update(proposed_execpolicy_amendment)
        .await
    {
        tracing::warn!("failed to persist execpolicy amendment: {error}");
    }
}
```

为此 `TurnContext` 需要新增：

```rust
/// Project-level execpolicy manager.
pub exec_policy: Arc<crate::exec_policy::ExecPolicyManager>,
```

session 构造时使用当前项目级 policy root：`cwd.join(".clawcode")`。该逻辑必须集中在一个函数中，函数名为 `default_exec_policy_home()`，并带英文 doc comment。

- [ ] **Step 6: 运行 execpolicy 测试**

Run: `rtk cargo test -p kernel exec_policy`

Expected: execpolicy writer 测试通过。

- [ ] **Step 7: Checkpoint**

不提交。持久 prefix rule writer 已可用，dispatch 接入后可处理 amendment decision。

---

### Task 10: ACP compatibility

**Files:**
- Modify: `crates/protocol/src/acp_conv.rs`
- Modify: `crates/tui/src/acp/client.rs`
- Test: `crates/protocol/src/acp_conv.rs`

- [ ] **Step 1: 写 legacy ACP kind 映射测试**

在 `acp_conv.rs` tests 中加入：

```rust
/// Verifies ACP allow-always maps to session approval.
#[test]
fn acp_allow_always_maps_to_approved_for_session() {
    let legacy = crate::permission::PermissionOptionKind::AllowAlways;
    let decision = crate::ReviewDecision::from(legacy);

    assert_eq!(decision, crate::ReviewDecision::ApprovedForSession);
}
```

- [ ] **Step 2: 运行测试**

Run: `rtk cargo test -p protocol acp_allow_always_maps_to_approved_for_session`

Expected: 测试通过。如果失败，修复 Task 1 的 `From` impl。

- [ ] **Step 3: 更新 TUI ACP client decision 映射**

`crates/tui/src/acp/client.rs` 仍然需要给 ACP responder 发送 option id。保留 `ApprovalDecision::option_id()`，但 kernel 内部的 approval response 使用 `protocol::ReviewDecision`。

在 resolve path 中明确：

```rust
pub fn resolve_permission(
    &self,
    request_id: u64,
    decision: ApprovalDecision,
) -> anyhow::Result<()> {
    self.permissions
        .respond_selected(request_id, decision.option_id())
}
```

该路径不直接写 kernel decision；ACP server adapter 收到 option id 后转换为新 `ReviewDecision`。

- [ ] **Step 4: 运行 ACP 相关测试**

Run: `rtk cargo test -p tui approval_from_acp_request_extracts_id_title_and_body`

Expected: 测试通过。

- [ ] **Step 5: Checkpoint**

不提交。ACP 兼容层仍支持旧 option kind，同时内部 decision 已迁移。

---

### Task 11: 全量验证和文档同步

**Files:**
- Modify: `docs/superpowers/specs/2026-06-01-fine-grained-permissions-design.md` if implementation exposes deliberate differences.
- Verify: workspace crates touched by this plan.

- [ ] **Step 1: 运行格式化**

Run: `rtk cargo fmt --all`

Expected: command exits 0。

- [ ] **Step 2: 运行 protocol tests**

Run: `rtk cargo test -p protocol`

Expected: protocol tests pass。

- [ ] **Step 3: 运行 config tests**

Run: `rtk cargo test -p config`

Expected: config tests pass。

- [ ] **Step 4: 运行 tools tests**

Run: `rtk cargo test -p tools`

Expected: tools tests pass。

- [ ] **Step 5: 运行 kernel tests**

Run: `rtk cargo test -p kernel`

Expected: kernel tests pass。

- [ ] **Step 6: 运行 TUI tests**

Run: `rtk cargo test -p tui`

Expected: tui tests pass。

- [ ] **Step 7: 运行 clippy**

Run: `rtk cargo clippy --all-targets --all-features --locked -- -D warnings`

Expected: clippy exits 0。

- [ ] **Step 8: 检查 git diff**

Run: `rtk git diff --stat`

Expected: diff 只包含本计划涉及的 protocol/config/kernel/tools/tui/docs 文件；`claw.toml` 如果仍是用户已有修改，不纳入本次变更。

- [ ] **Step 9: 提交前检查**

如果用户明确授权 commit，先运行项目相关 pre-commit hooks，并读取 `.gitmessage`：

Run: `rtk pre-commit run --all-files`

Expected: hooks pass。

Run: `rtk sed -n '1,120p' .gitmessage`

Expected: 输出提交模板，commit message 必须按模板填写。

---

## 计划自检

### Spec 覆盖

- `AskForApproval` / `GranularApprovalConfig`：Task 2。
- enhanced `ReviewDecision`：Task 1、Task 8、Task 10。
- Session `ApprovalStore`：Task 3、Task 7。
- `ToolInvocation` 和 object-safe adapter：Task 4、Task 5、Task 6。
- `ExecApprovalRequirement`：Task 4、Task 7。
- execpolicy amendment：Task 9。
- network policy amendment 协议形状：Task 1、Task 9。
- TUI `available_decisions` 边界：Task 8。
- ACP 兼容：Task 10。

### 风险控制

- 旧 `permission::ReviewDecision` 保留到兼容层稳定后再删除。
- `Tool::needs_approval()` 保留默认 fallback，避免一次性迁移所有工具。
- execpolicy 第一版只写项目级 `.clawcode/rules/default.rules`。
- 文件写入类工具不做跨 session 持久 allow。
- 所有新增或修改 Rust 代码必须包含英文注释，新增函数必须有函数级英文 doc comment。
