# 参考实现风格细粒度权限审批设计

## 背景

当前 clawcode 的权限审批链路是一次性的：`Tool::needs_approval()` 返回布尔值，kernel 发出 approval request，TUI 或 ACP 回传 `ReviewDecision`，等待中的 `oneshot` 被唤醒后立即继续或拒绝。本模型能表达“这次是否执行”，但不能表达 参考实现已经具备的几个关键能力：

1. 审批策略不是细粒度的，当前只有 `request_approval` 和 `yolo`。
2. 协议里虽然有 `AllowAlways` / `RejectAlways`，但运行时没有 session cache，也没有磁盘规则。
3. 工具不能提供结构化 approval key，所以系统不知道两次请求是否安全等价。
4. shell、patch、MCP、network、additional permissions 被压成同一类确认框。
5. 没有 参考实现的 `execpolicy` amendment 模型，用户无法选择“以后允许同类命令前缀”。

本设计目标是尽量贴合 参考实现，而不是为 clawcode 重新发明一套权限系统。实现上可以分阶段落地，但命名、协议形状、运行时职责应尽量向 参考实现 靠拢，避免后续再迁移一次。

## 目标

1. 用 参考实现风格的 `AskForApproval` 替代当前薄弱的 `ApprovalMode` 语义。
2. 引入 参考实现风格的 `ReviewDecision`：`Approved`、`ApprovedForSession`、`ApprovedExecpolicyAmendment`、`NetworkPolicyAmendment`、`Denied`、`TimedOut`、`Abort`。
3. 引入 session 级 `ApprovalStore`，用结构化 approval key 支持 `ApprovedForSession`。
4. 引入 `ExecApprovalRequirement`，让工具或 policy 明确返回 `Skip`、`NeedsApproval`、`Forbidden`。
5. 引入 参考实现风格的 `ExecPolicyAmendment`，将可持久化的 shell command prefix 写入规则文件并刷新内存 policy。
6. 引入 network policy amendment 的协议形状，即使第一阶段只预留或部分落地。
7. 让 TUI 根据 `available_decisions` 展示选项，而不是硬编码 allow/reject。
8. 保持旧 `approval = "request_approval"` / `"yolo"` 配置兼容。

## 非目标

1. 第一版不实现 自动审查器 自动审查器，但保留 `TimedOut` 和未来 reviewer 接入点。
2. 第一版不完整移植 sandbox enforcement；权限审批只决定是否执行、是否跳过审批、是否写入规则。
3. 第一版不要求一次性支持所有 参考实现 `PermissionProfile` 能力，但类型设计必须能承载后续文件系统、网络、additional permissions。
4. 第一版不迁移历史 session 数据。历史事件缺少新字段时走兼容路径。
5. 第一版不提供文件写入类工具的跨 session 持久 allow。参考实现的持久化重点是 execpolicy/network/MCP policy，不是直接把任意文件写入持久放行。

## 参考实现对齐点

### 审批策略对齐

参考实现的策略类型是 `AskForApproval`：

```rust
pub enum AskForApproval {
    UnlessTrusted,
    OnFailure,
    OnRequest,
    Granular(GranularApprovalConfig),
    Never,
}

pub struct GranularApprovalConfig {
    pub sandbox_approval: bool,
    pub rules: bool,
    pub skill_approval: bool,
    pub request_permissions: bool,
    pub mcp_elicitations: bool,
}
```

clawcode 应采用同一语义，而不是使用 `Allow/Ask/Deny` per-category 枚举。`GranularApprovalConfig` 中的布尔值表示该类 prompt 是否允许展示；如果为 `false`，运行时应自动拒绝该类请求，而不是展示确认框。

旧配置映射：

| 旧配置 | 新语义 |
|---|---|
| `approval = "request_approval"` | `AskForApproval::OnRequest` |
| `approval = "yolo"` | `AskForApproval::Never`，并保持当前无提示执行语义 |

后续可新增：

```toml
approval_policy = "on-request"

[approval_policy.granular]
sandbox_approval = true
rules = true
skill_approval = true
request_permissions = true
mcp_elicitations = true
```

### 审批决策对齐

clawcode 应将内部决策迁移为 参考实现风格：

```rust
pub enum ReviewDecision {
    Approved,
    ApprovedExecpolicyAmendment {
        proposed_execpolicy_amendment: ExecPolicyAmendment,
    },
    ApprovedForSession,
    NetworkPolicyAmendment {
        network_policy_amendment: NetworkPolicyAmendment,
    },
    Denied,
    TimedOut,
    Abort,
}
```

语义：

- `Approved`：仅本次允许。
- `ApprovedForSession`：本次允许，并把本次 approval key 写入 session `ApprovalStore`。
- `ApprovedExecpolicyAmendment`：本次允许，并将命令前缀 append 到 execpolicy 规则文件，刷新内存 policy。
- `NetworkPolicyAmendment`：写入网络 allow/deny 规则，并按 action 决定本次允许或拒绝。
- `Denied`：本次拒绝，turn 继续。
- `TimedOut`：自动审查或外部审批超时，第一版可作为拒绝处理。
- `Abort`：中止当前 turn，让用户重新指示。

兼容映射：

| 旧决策 | 新决策 |
|---|---|
| `AllowOnce` | `Approved` |
| `AllowAlways` | `ApprovedForSession` |
| `RejectOnce` | `Denied` |
| `RejectAlways` | 第一版映射为 `Denied`；不设计通用 deny-for-session |
| `Abort` | `Abort` |

不引入通用 `RejectForSession`。参考实现没有这个通用决策；持久 deny 应通过具体 policy amendment 表达，例如 network deny rule 或 MCP deny rule。

### 请求事件对齐

参考实现的 exec approval request 是专门的事件，不是泛化的 `PermissionRequest`。clawcode 应新增或迁移到类似结构：

```rust
pub struct ExecApprovalRequestEvent {
    pub call_id: String,
    pub approval_id: Option<String>,
    pub turn_id: String,
    pub started_at_ms: i64,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub reason: Option<String>,
    pub network_approval_context: Option<NetworkApprovalContext>,
    pub proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
    pub proposed_network_policy_amendments: Option<Vec<NetworkPolicyAmendment>>,
    pub additional_permissions: Option<AdditionalPermissionProfile>,
    pub available_decisions: Option<Vec<ReviewDecision>>,
    pub parsed_cmd: Vec<ParsedCommand>,
}
```

关键点：

1. `approval_id` 独立于 `call_id`，为未来 execve 子命令拦截预留。
2. `available_decisions` 由 core/kernel 生成，TUI 只展示，不自行创造 persisted rule。
3. `proposed_execpolicy_amendment` 是一个 prefix amendment，不是把整个 shell command 原文持久化。
4. `additional_permissions` 单独出现时，默认只给 `Approved` / `Abort`，不展示 execpolicy option。

默认决策集合应贴合 参考实现：

| 请求类型 | 默认可选决策 |
|---|---|
| 普通 shell prompt，无 amendment | `Approved`, `Abort` |
| 普通 shell prompt，有 prefix amendment | `Approved`, `ApprovedExecpolicyAmendment`, `Abort` |
| network prompt | `Approved`, `ApprovedForSession`, `NetworkPolicyAmendment(allow)`, `Abort` |
| additional permissions prompt | `Approved`, `Abort` |

## 核心运行时架构

```text
Provider tool call
  │
  ├─ tool_name + JSON arguments
  │
  ├─ ToolRegistry -> Arc<dyn Tool>
  │
  ├─ Tool::build_invocation(...)
  │    ├─ ToolInvocation
  │    └─ tool-specific typed request
  │
  ├─ exec_approval_requirement(req)
  │    ├─ Skip { bypass_sandbox, proposed_execpolicy_amendment }
  │    ├─ NeedsApproval { reason, proposed_execpolicy_amendment }
  │    └─ Forbidden { reason }
  │
  ├─ Orchestrator / dispatch_tool
  │    ├─ Skip      → 直接执行
  │    ├─ Forbidden → 返回 rejected tool result
  │    └─ NeedsApproval
  │         ├─ with_cached_approval(keys)
  │         ├─ request_command_approval(...)
  │         └─ reject_if_not_approved(decision)
  │
  └─ 执行工具
```

clawcode 当前的 `dispatch_tool` 直接调用 `tool.needs_approval()`。目标设计应把它拆成 参考实现 式三层：

1. `Approvable`：工具提供 approval keys 和发起 approval 的方式。
2. `ExecApprovalRequirement`：policy 判断结果。
3. `ApprovalStore`：session 级缓存 `ApprovedForSession`。

参考实现中 core 内部大量使用泛型 `ToolRuntime<Rq, Out>` / `Approvable<Req>`。clawcode 不应直接把现有 `Tool` trait 改成泛型，因为当前 `ToolRegistry` 需要存放异构工具：

```rust
HashMap<String, Arc<dyn Tool>>
```

如果把 `Tool` 改成 `Tool<Req, Out>`，registry、MCP dynamic tools、provider dispatch 都会被迫泛型化，改动面过大。clawcode 应采用 **object-safe Tool + typed runtime adapter**：

1. 对外保留 `Arc<dyn Tool>`，让 registry 和 provider dispatch 继续稳定。
2. 每个具体工具内部定义强类型 request、approval key、runtime。
3. `Tool::build_invocation()` 负责把 JSON arguments 解析成该工具的 typed request，并封装到统一 `ToolInvocation`。
4. approval engine 只处理统一的 invocation metadata、approval requirement 和 decision，不直接依赖具体 request 类型。

## ToolInvocation 设计

`ToolInvocation` 是“模型请求调用工具”的统一 envelope。它记录事实，不直接表达审批结果。

```rust
pub struct ToolInvocation {
    pub call_id: String,
    pub approval_id: Option<String>,
    pub turn_id: TurnId,
    pub tool_name: String,
    pub raw_arguments: serde_json::Value,
    pub cwd: PathBuf,
    pub approval: ToolApprovalInvocation,
}

pub enum ToolApprovalInvocation {
    Shell(ShellApprovalInvocation),
    ApplyPatch(ApplyPatchApprovalInvocation),
    Mcp(McpApprovalInvocation),
    Generic(GenericApprovalInvocation),
}
```

职责边界：

- `ToolInvocation` 保存 tool call 的统一上下文：call id、turn id、cwd、原始 arguments 和审批元数据。
- `ToolApprovalInvocation` 保存可用于审批的工具特定结构，例如 shell command tokens、patch paths、MCP server/tool。
- `ExecApprovalRequirement` 是基于 invocation 和 policy 得出的审批判断。
- `ExecApprovalRequestEvent` 是给 UI 的展示事件。
- `ReviewDecision` 是用户或自动策略返回的结果。

建议的 object-safe `Tool` 扩展：

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;

    fn build_invocation(
        &self,
        call_id: &str,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolInvocation, ToolInvocationError>;

    fn exec_approval_requirement(
        &self,
        invocation: &ToolInvocation,
        policy: AskForApproval,
    ) -> ExecApprovalRequirement;

    async fn execute_invocation(
        &self,
        invocation: ToolInvocation,
        ctx: &ToolContext,
    ) -> Result<ToolExecution, String>;
}
```

工具内部可以保留 参考实现风格的泛型 runtime：

```rust
pub trait TypedToolRuntime {
    type Request;
    type ApprovalKey: serde::Serialize;

    fn parse_request(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<Self::Request, String>;

    fn approval_keys(&self, req: &Self::Request) -> Vec<Self::ApprovalKey>;

    fn exec_approval_requirement(
        &self,
        req: &Self::Request,
        policy: AskForApproval,
    ) -> ExecApprovalRequirement;
}
```

映射关系：

```text
ToolRegistry
  └─ Arc<dyn Tool>
      ├─ ShellCommand
      │   └─ ShellRuntime: TypedToolRuntime<Request = ShellRequest>
      ├─ ApplyPatch
      │   └─ ApplyPatchRuntime: TypedToolRuntime<Request = ApplyPatchRequest>
      └─ McpTool
          └─ McpRuntime: TypedToolRuntime<Request = McpRequest>
```

这样可以保留当前 registry 架构，同时让每个工具获得 参考实现 式强类型 request/key 设计。

## 核心类型

### ExecApprovalRequirement

```rust
pub enum ExecApprovalRequirement {
    Skip {
        bypass_sandbox: bool,
        proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
    },
    NeedsApproval {
        reason: Option<String>,
        proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
    },
    Forbidden {
        reason: String,
    },
}
```

说明：

- `Skip` 表示当前 policy 已允许，不需要 prompt。
- `NeedsApproval` 表示可以向用户询问。
- `Forbidden` 表示 policy 不允许询问或明确禁止，应直接拒绝。
- `proposed_execpolicy_amendment` 即使命中 `Skip` 也可能存在，用于 sandbox 失败后提示无 sandbox 重试时复用。

### ApprovalStore

参考实现的 session cache 是泛型 key 序列化后的 `HashMap<String, ReviewDecision>`。clawcode 可直接采用：

```rust
pub struct ApprovalStore {
    map: HashMap<String, ReviewDecision>,
}

impl ApprovalStore {
    pub fn get<K: Serialize>(&self, key: &K) -> Option<ReviewDecision>;
    pub fn put<K: Serialize>(&mut self, key: K, value: ReviewDecision);
}
```

配套 helper：

```rust
pub async fn with_cached_approval<K, F, Fut>(
    services: &SessionServices,
    tool_name: &str,
    keys: Vec<K>,
    fetch: F,
) -> ReviewDecision
where
    K: Serialize,
    F: FnOnce() -> Fut,
    Fut: Future<Output = ReviewDecision>;
```

行为保持 参考实现 一致：

1. key 为空时不查缓存，直接执行 `fetch()`。
2. 所有 key 都命中 `ApprovedForSession` 时跳过 prompt。
3. 用户返回 `ApprovedForSession` 时，将本次所有 key 写入 store。
4. store 只缓存 approval，不缓存通用 deny。

### ApprovalKey

不设计全局万能 key；每个 runtime 定义自己的 key 类型。统一要求是 `Serialize`、稳定、足够窄。

shell key：

```rust
pub struct ShellApprovalKey {
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub sandbox_permissions: SandboxPermissions,
    pub additional_permissions: Option<AdditionalPermissionProfile>,
}
```

apply_patch key：

```rust
pub struct ApplyPatchApprovalKey {
    pub environment_id: Option<String>,
    pub path: PathBuf,
}
```

MCP key：

```rust
pub struct McpToolApprovalKey {
    pub server: String,
    pub tool: String,
    pub arguments_hash: String,
}
```

这些 key 的职责只服务 `ApprovedForSession`。跨 session 持久化必须通过 policy amendment 或 MCP policy rule 表达。

## execpolicy 设计

为了贴合 参考实现，持久化规则不应使用 `permissions.toml` 自创 schema。第一版应引入 参考实现风格 `execpolicy` 文件，至少支持：

```text
prefix_rule(pattern=["cargo", "test"], decision="allow")
network_rule(host="api.github.com", protocol="https", decision="allow")
network_rule(host="example.com", protocol="https", decision="deny", justification="user blocked")
```

推荐路径：

```text
<project_cwd>/.clawcode/rules/default.rules
```

如果项目后续已有 config layer，再扩展为 user/project/managed 多层规则。第一版先实现项目级 `.clawcode/rules/default.rules`，避免把某个项目的命令信任扩散到全局。

### ExecPolicyAmendment

```rust
pub struct ExecPolicyAmendment {
    pub command: Vec<String>,
}
```

写入规则：

1. 空 prefix 必须拒绝。
2. prefix token 用 JSON string 序列化，避免手写转义错误。
3. 写文件时创建 `rules/` 目录。
4. 使用文件锁或进程内 update lock，避免并发审批重复写。
5. 如果已有完全相同行，直接返回成功。
6. 写盘成功后刷新内存 policy。
7. 如果内存中已有 allow rule，也仍要保证磁盘存在该规则。

### amendment 派生

shell approval request 不应总是提供 amendment。规则：

1. 如果已有 execpolicy prompt rule 命中，不提供自动 allow amendment，因为 allow prefix 可能无法跳过显式 prompt rule。
2. 如果命中 heuristic prompt，可取第一个触发 prompt 的 parsed command 作为 amendment。
3. 如果命中 heuristic allow，但 sandbox 失败后需要无 sandbox 重试，可提供 allow amendment。
4. 禁止为过宽 prefix 提供 amendment，例如空 prefix、`bash`、`bash -lc`、`sh -c`、`python -c`、`git` 等。
5. 多行命令或复杂 shell 无法稳定解析时，不提供 amendment，只允许 `Approved` / `Abort`。

## network policy 对齐

协议类型：

```rust
pub enum NetworkApprovalProtocol {
    Http,
    Https,
    Socks5Tcp,
    Socks5Udp,
}

pub struct NetworkApprovalContext {
    pub host: String,
    pub protocol: NetworkApprovalProtocol,
}

pub enum NetworkPolicyRuleAction {
    Allow,
    Deny,
}

pub struct NetworkPolicyAmendment {
    pub host: String,
    pub action: NetworkPolicyRuleAction,
}
```

行为：

1. network prompt 默认展示 `Approved`、`ApprovedForSession`、`NetworkPolicyAmendment(allow)`、`Abort`。
2. 如果 UI 后续支持 deny-list，也可展示 `NetworkPolicyAmendment(deny)`。
3. 写入 network rule 前必须 normalize host。
4. amendment host 必须与本次 `NetworkApprovalContext.host` normalize 后一致。
5. runtime network proxy 已启动时，应同步更新 runtime allowlist/denylist；没有 proxy 时只更新 policy 文件和内存 policy。

## MCP 对齐

参考实现 对 MCP approval 有两类行为：

1. `ApprovedForSession` 使用 session `ApprovalStore` 记住 tool approval key。
2. MCP elicitation 中可以通过 meta 表达 tool approval 和 persist scope。

clawcode 第一版应实现：

- MCP 默认需要 approval。
- MCP key 包含 server、tool、arguments hash。
- `ApprovedForSession` 写入 session `ApprovalStore`。
- 持久 MCP allow/deny 可作为第二阶段，落到 MCP server policy，而不是塞进 shell execpolicy。

第一版不需要完全复刻 MCP elicitation form，但内部决策语义要与 `ApprovedForSession` 对齐，避免未来 adapter 重写。

## request_permissions / additional_permissions

参考实现区分两种权限：

- `PermissionProfile`：当前 turn/session 的基础权限。
- `AdditionalPermissionProfile`：某个 tool call 请求的额外权限。

clawcode 第一版可以先定义最小结构：

```rust
pub struct PermissionProfile {
    pub file_system: FileSystemPermissionProfile,
    pub network: NetworkPermissionProfile,
}

pub struct AdditionalPermissionProfile {
    pub file_system: Option<FileSystemPermissionProfile>,
    pub network: Option<NetworkPermissionProfile>,
}
```

行为：

1. tool call 如果携带 additional permissions，approval request 必须展示权限摘要。
2. additional permissions prompt 第一版只允许 `Approved` / `Abort`，不展示 execpolicy amendment。
3. `request_permissions` 工具后续可返回 turn 或 session scope grant；本设计先预留，不作为第一阶段必做。

## TUI 对齐

TUI 不应硬编码 `[a] allow once [r] reject`。它应接收 `available_decisions`，按 参考实现 文案风格展示：

普通 shell：

```text
1. Yes, proceed
2. Yes, and don't ask again for commands that start with `cargo test`
3. No, and tell Clawcode what to do differently
```

network：

```text
1. Yes, just this once
2. Yes, and allow this host for this conversation
3. Yes, and allow this host in the future
4. No, and tell Clawcode what to do differently
```

additional permissions：

```text
1. Yes, grant these permissions for this turn
2. No, continue without permissions
```

映射规则：

- UI option 必须来自 `available_decisions`。
- `ApprovedExecpolicyAmendment` 的 label 使用 amendment prefix 渲染。
- 如果渲染 prefix 含换行，隐藏该 persisted option。
- UI 回传结构化 `ReviewDecision`，不回传自造 option id。

## ACP 兼容

当前 ACP permission option kind 可映射到新决策：

| ACP option kind | clawcode 内部决策 |
|---|---|
| `AllowOnce` | `Approved` |
| `AllowAlways` | `ApprovedForSession` |
| `RejectOnce` | `Denied` |
| `RejectAlways` | `Denied` |

ACP adapter 后续可以通过 `_meta` 携带 参考实现风格 persist 信息：

- `approval_kind`
- `persist`
- `tool_params`
- `tool_params_display`

内部 kernel 不依赖 ACP `_meta`；它只处理结构化 `ReviewDecision`。

## 迁移步骤

### 阶段 1：协议命名和 session cache 对齐 参考实现

1. 在 `protocol` 中新增 参考实现风格 `AskForApproval`、`GranularApprovalConfig`、`ReviewDecision`、`ExecPolicyAmendment`。
2. 保留旧 `ApprovalMode`，但 config loader 将旧字段映射到 `AskForApproval`。
3. 新增 `ApprovalStore`，挂到 session runtime 或 session services。
4. shell runtime 定义 `ShellApprovalKey`，apply_patch runtime 定义 `ApplyPatchApprovalKey`。
5. TUI 展示 `Approved`、`ApprovedForSession`、`Denied`、`Abort`。

验收：

- `AllowAlways` 兼容输入会映射为 `ApprovedForSession`。
- 同一 shell key 在本 session 第二次执行不再弹窗。
- apply_patch 对已批准路径再次修改不再弹窗。
- 旧 `request_approval` 和 `yolo` 配置仍能启动。

### 阶段 2：ExecApprovalRequirement 和 execpolicy

1. 将 `Tool::needs_approval()` 迁移为 `exec_approval_requirement()` 或 tool runtime adapter。
2. 新增 execpolicy parser 和 evaluator，支持 `prefix_rule`。
3. shell command 评估 execpolicy，返回 `Skip`、`NeedsApproval`、`Forbidden`。
4. 实现 `append_amendment_and_update()`：写入 `.clawcode/rules/default.rules` 并刷新内存 policy。
5. TUI 展示 `ApprovedExecpolicyAmendment` 选项。

验收：

- 用户选择 prefix amendment 后，规则文件出现 `prefix_rule(pattern=[...], decision="allow")`。
- 新 session 加载规则后，同 prefix 命令不再弹窗。
- 重复 amendment 不重复写文件。
- 空 prefix 和过宽 prefix 不展示持久化选项。

### 阶段 3：network 和 MCP 对齐

1. 新增 `NetworkApprovalContext`、`NetworkPolicyAmendment`、network rule writer。
2. network prompt 支持 `ApprovedForSession` 和 allow host future。
3. MCP tool approval 接入 `ApprovalStore`。
4. MCP 持久 allow/deny 规则单独设计到 MCP policy，不与 shell execpolicy 混用。

验收：

- network host allow future 会写入 `network_rule(...)`。
- host normalize 不一致时拒绝写入。
- MCP tool `ApprovedForSession` 后同 key 不再弹窗。

## 模块结构

建议新增或修改：

```text
crates/protocol/src/approvals.rs          # 新增 参考实现风格 approval/event 类型
crates/protocol/src/config.rs             # 增加 AskForApproval / GranularApprovalConfig
crates/kernel/src/approval/mod.rs         # ApprovalStore / cached approval helper
crates/kernel/src/exec_policy.rs          # execpolicy evaluator + amendment writer
crates/kernel/src/turn.rs                 # dispatch_tool 接入 requirement/orchestrator
crates/tools/src/lib.rs                   # Tool approval API 迁移
crates/tools/src/builtin/shell.rs         # ShellApprovalKey / amendment 派生
crates/tools/src/builtin/fs/...           # ApplyPatchApprovalKey
crates/tui/src/ui/approval.rs             # available_decisions 渲染和 key mapping
crates/config/src/config.rs               # 旧 approval 字段兼容和新策略加载
```

如果后续引入更完整的 tool runtime/orchestrator，可再拆出：

```text
crates/kernel/src/tools/orchestrator.rs
crates/kernel/src/tools/sandboxing.rs
```

## 错误处理

1. approval channel 丢失时返回明确 internal error。
2. UI 回传不在 `available_decisions` 中的 decision 时拒绝该 decision，第一版可按 `Denied` 处理。
3. approval key 序列化失败时不写 session cache，本次按用户决策执行。
4. execpolicy 文件解析失败时启动不失败，但禁用 persisted policy 并发 warning。
5. amendment 写盘失败时，本次 `ApprovedExecpolicyAmendment` 仍允许执行，但必须发 warning，且不能记录“已保存”上下文。
6. network amendment host 校验失败时不写规则，本次按 `Denied` 或普通 `Approved` 取决于 action；第一版推荐拒绝该 amendment 并发 warning。

## 安全边界

1. `ApprovedForSession` 只写入 session `ApprovalStore`，不写磁盘。
2. `ApprovedExecpolicyAmendment` 只能写入非空、不过宽、可解释的 command prefix。
3. additional permissions prompt 不展示 execpolicy amendment。
4. network deny/allow 必须经过 host normalize 和一致性校验。
5. deny 规则优先于 allow 规则。
6. MCP 持久规则不能默认 wildcard arguments，除非 UI 明确展示 wildcard 语义。
7. 旧 `yolo` 兼容不应暗中写入持久规则。

## 测试计划

### 单元测试

1. `ApprovalMode` 到 `AskForApproval` 的兼容映射。
2. `GranularApprovalConfig` 禁用某类 prompt 时返回 `Forbidden`。
3. `ApprovalStore` all-keys-approved 才跳过 prompt。
4. `ApprovedForSession` 写入所有 approval keys。
5. shell key 包含 command、cwd、sandbox permissions、additional permissions。
6. amendment 派生拒绝空 prefix、过宽 wrapper、多行命令。
7. execpolicy writer 创建目录、追加规则、去重、刷新内存 policy。
8. `default_available_decisions()` 在普通 shell、network、additional permissions 三类请求上符合 参考实现 行为。

### 集成测试

1. shell 第一次请求审批，选择 `ApprovedForSession` 后第二次不弹窗。
2. shell 选择 `ApprovedExecpolicyAmendment` 后，新 session 读取规则并跳过审批。
3. apply_patch 多路径 approval key 支持子集复用。
4. execpolicy parse 失败时 session 仍可启动并发 warning。
5. MCP tool `ApprovedForSession` 后同 server/tool/arguments hash 不再弹窗。

### TUI 测试

1. `available_decisions` 驱动 overlay 选项。
2. 有 amendment 时展示 “don't ask again for commands that start with ...”。
3. prefix 含换行时隐藏 amendment option。
4. network prompt 展示 once/session/future host。
5. additional permissions prompt 不展示 prefix 持久化选项。

## 成功标准

1. clawcode 内部审批类型和语义与 参考实现主干保持同构，后续迁移成本低。
2. `ApprovedForSession` 具备真实 session cache 行为。
3. shell command 支持 参考实现风格 `ApprovedExecpolicyAmendment`，并写入 `prefix_rule(...)`。
4. TUI 由 `available_decisions` 驱动，不再硬编码两个按钮。
5. 旧配置和旧 ACP option 可以无破坏映射到新语义。
6. 第一版不为了“更细”而过度放权：所有持久化都必须通过明确 amendment 或 policy rule。
