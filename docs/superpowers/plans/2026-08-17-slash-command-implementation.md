# Slash Command 统一机制实施计划

> **执行要求：** REQUIRED SUB-SKILL：使用 `superpowers:executing-plans` 在当前会话逐项执行。项目规则禁止 SubAgent 和 worktree，因此不使用 `subagent-driven-development`。

**目标：** 将 Builtin、Extension、Skill 和 Prompt Template 统一接入 Kernel Slash Command 路由，通过 ACP v2 发布和执行，并在 Store 与 WebUI 中无损持久化、回放和展示。

**架构：** `protocol` 保存唯一公共命令、输入和消息类型；Kernel 使用类型化命令目录与执行上下文保持 Pi 顺序；ACP 只映射标准 Available Commands、Prompt、Message、Session Info 和 State Update；WebUI 只消费 ACP 快照和命令 metadata。`provider` 与 `config` crate 不修改，Kernel 的 provider 消息投影只增加新协议消息的模型上下文映射。

**技术栈：** Rust 2024、Tokio、Serde、typed-builder、ACP v2 Rust SDK、JSONL Store、TypeScript 6、React 19、Zustand、Vite。

## 全局约束

- spec：`docs/superpowers/specs/2026-08-17-slash-command-design.md`。
- 所有新增 Rust 函数必须有英文函数级注释，非平凡逻辑必须有英文注释。
- 不创建大量短 helper；单个自定义类型参数的逻辑优先放入该类型的 `impl` 或标准 Trait 实现。
- 超过 3 个字段的 Rust struct 使用 `typed-builder`，`Option` 字段使用 `#[builder(default)]`。
- `Arc<T>` 字段使用 `Arc::clone(&value)`，不使用含义不清的 `.clone()`。
- 测试只放在 crate 的 `tests/` 集成测试目录；生产正文不增加测试支撑代码。
- 后端严格执行 RED-GREEN-REFACTOR；每项生产行为必须先运行对应失败测试。
- 前端不新增单元测试，但必须运行 TypeScript、ESLint、生产构建和真实 WebUI E2E。
- 所有命令使用 `rtk` 前缀。
- 日志只增加命令开始、结束、拒绝和失败等有意义边界，使用 `tracing::info!()` 等全路径宏和格式化文本，不记录完整 Prompt 或凭据。
- 项目名称、ACP namespace 与扩展类型均使用 `ProductIdentity` 公共常量。
- 不修改 `crates/provider` 和 `crates/config`。
- 未经用户再次明确授权，不创建 commit。

---

### 任务 1：建立唯一的 Slash Command 协议模型

**文件：**

- 新建：`crates/protocol/src/slash_command.rs`
- 新建：`crates/protocol/tests/slash_command.rs`
- 修改：`crates/protocol/src/lib.rs`
- 修改：`crates/protocol/src/prompt/mod.rs`
- 删除：`crates/protocol/src/prompt/command.rs`
- 修改：`crates/protocol/src/session.rs`
- 修改：`crates/protocol/src/message.rs`
- 修改：`crates/protocol/src/event.rs`

**接口：**

- 产出：`SlashCommandDefinition`、`SlashCommandSource`、`SlashCommandAliasKind`、`SlashCommandInvocation`、`SlashCommandExpansion`、`SlashCommandMessage`、`SlashCommandOutput`、`SlashCommandStatus`、`SlashCommandOutcome`、`SlashCommandError`。
- 产出：`RunInput::{Text, Composite}`，由 `RunRequest.input` 使用。
- 产出：`MessageContent::{ExpandedUser, SlashCommand}`。
- 产出：`AgentEventPayload::{SlashCommandStart, SlashCommandEnd}`。
- 替代：删除 `AvailableAgentCommand` 与 `AvailableAgentCommandKind`，所有调用方改用统一类型。

- [ ] **步骤 1：增加失败的协议解析与序列化测试**

```rust
#[test]
fn slash_invocation_preserves_ascii_space_arguments() {
    let invocation = SlashCommandInvocation::try_from("/compact  keep details")
        .expect("Slash Command");
    assert_eq!(invocation.name, "compact");
    assert_eq!(invocation.arguments, " keep details");
    assert_eq!(invocation.original, "/compact  keep details");
}

#[test]
fn composite_input_is_not_a_slash_candidate() {
    let input = RunInput::Composite("/compact\n\n[spec](file:///spec)".to_string());
    assert!(input.slash_command_text().is_none());
}
```

同时覆盖无 `/`、前导空格、空命令名、Tab/换行不是分隔符，以及命令消息序列化后 `TurnId` 和三个时间字段仍为字符串。

- [ ] **步骤 2：运行协议测试并确认 RED**

```sh
rtk cargo test -p protocol --test slash_command
```

预期：因 `SlashCommandInvocation` 和 `RunInput` 尚不存在而编译失败。

- [ ] **步骤 3：实现公共类型与解析 Trait**

```rust
pub enum RunInput {
    Text(String),
    Composite(String),
}

impl RunInput {
    /// Returns command-eligible text only for a text-only ACP Prompt.
    #[must_use]
    pub fn slash_command_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Composite(_) => None,
        }
    }

    /// Returns the complete model-facing text projection.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Text(text) | Self::Composite(text) => text,
        }
    }
}

pub struct SlashCommandInvocation {
    pub original: String,
    pub name: String,
    pub arguments: String,
}

impl TryFrom<&str> for SlashCommandInvocation {
    type Error = SlashCommandParseError;

    /// Parses a leading slash and the first U+0020 delimiter without tokenizing arguments.
    fn try_from(input: &str) -> Result<Self, Self::Error> {
        let command = input.strip_prefix('/').ok_or(SlashCommandParseError::NotCommand)?;
        let (name, arguments) = command
            .split_once(' ')
            .map_or((command, ""), |(name, arguments)| (name, arguments));
        if name.is_empty() {
            return Err(SlashCommandParseError::EmptyName);
        }
        Ok(Self { original: input.to_string(), name: name.to_string(), arguments: arguments.to_string() })
    }
}
```

`SlashCommandDefinition` 包含 `name`、`description`、`argument_hint`、`source`、`qualified_name`、`alias_kind`，使用 typed-builder。`SlashCommandExpansion` 包含 invocation、source 和 model blocks。`SlashCommandMessage` 是 `Invocation`/`Output` 枚举，避免输入与输出重复结构体。

- [ ] **步骤 4：扩展消息和事件类型**

`MessageContent::ExpandedUser` 的 `role()` 返回 `Role::User`，`text_content()` 返回模型展开内容；`MessageContent::SlashCommand` 的 `role()` 返回 `None`，不进入模型上下文。`SlashCommandStart` 携带 `RunId` 和 invocation，`SlashCommandEnd` 携带 `RunId` 和 `SlashCommandStatus`。

- [ ] **步骤 5：运行协议测试并确认 GREEN**

```sh
rtk cargo test -p protocol --test slash_command
rtk cargo test -p protocol
```

- [ ] **步骤 6：检查本任务差异，不提交**

```sh
rtk cargo fmt --all -- --check
rtk git diff --check
rtk git status --short
```

---

### 任务 2：统一命令目录、Builtin 注册和 Extension 限定名

**文件：**

- 修改：`crates/extension/src/dynamic.rs`
- 修改：`crates/extension/src/registrar.rs`
- 修改：`crates/extension/src/lib.rs`
- 修改：`crates/extension/tests/dynamic.rs`
- 修改：`crates/kernel/src/runtime/command.rs`
- 新建：`crates/kernel/src/runtime/command/builtin.rs`
- 新建：`crates/kernel/tests/slash_commands.rs`
- 修改：`crates/kernel/tests/session_capabilities.rs`

**接口：**

- 消费：任务 1 的 `SlashCommandDefinition`、`SlashCommandInvocation` 和 `SlashCommandSource`。
- 产出：`BuiltinSlashCommand::{Compact, Name, Session}`。
- 产出：Kernel 私有 `ResolvedSlashCommand` 和 Session 命令目录解析。
- 保留：`Kernel::available_commands(&SessionId) -> Result<Vec<SlashCommandDefinition>, KernelError>`。

- [ ] **步骤 1：增加限定名和完整快照失败测试**

```rust
#[test]
fn extension_qualified_names_use_colon() {
    assert_eq!(registered_command("review").qualified_name(), "sample:review");
}

#[tokio::test]
async fn command_snapshot_reserves_builtins_and_orders_sources() {
    let commands = fixture().kernel.available_commands(&fixture().session_id)
        .expect("commands");
    assert_eq!(
        commands.iter().map(|command| command.name.as_str()).collect::<Vec<_>>(),
        vec!["compact", "name", "session", "first:inspect", "inspect", "skill:review", "review"]
    );
}
```

再覆盖两个 Extension 同名时只发布限定名、Builtin 遮蔽同名 Extension 短别名、Skill `skill:` 名称高于 Template。

- [ ] **步骤 2：运行目标测试并确认 RED**

```sh
rtk cargo test -p extension --test dynamic
rtk cargo test -p kernel --test slash_commands
```

预期：限定名仍为 `extension/name`，快照缺少 Builtin 和来源 metadata。

- [ ] **步骤 3：让 Extension Registry 使用协议 Invocation 与冒号限定名**

删除 `extension::ExtensionCommandInvocation`，统一使用 `protocol::SlashCommandInvocation`。`RegisteredCommand::qualified_name()`、registrar duplicate key、upsert/remove key全部改为 `extension-id:command`。不在 Extension crate 再定义命令解析结构体。

- [ ] **步骤 4：实现类型化 Builtin 与目录解析**

```rust
pub(super) enum BuiltinSlashCommand {
    Compact,
    Name,
    Session,
}

impl BuiltinSlashCommand {
    /// Returns definitions for every server-owned command in lexical order.
    pub(super) fn definitions() -> [SlashCommandDefinition; 3] {
        [
            Self::Compact.definition(
                "Manually compact the session context",
                Some("[custom instructions]"),
            ),
            Self::Name.definition("Set or show the session display name", Some("[name]")),
            Self::Session.definition("Show session information and statistics", None),
        ]
    }

    /// Resolves one reserved built-in name without allocating aliases.
    pub(super) fn resolve(name: &str) -> Option<Self> {
        match name {
            "compact" => Some(Self::Compact),
            "name" => Some(Self::Name),
            "session" => Some(Self::Session),
            _ => None,
        }
    }
}

pub(super) enum ResolvedSlashCommand {
    Builtin(BuiltinSlashCommand),
    Extension(extension::RegisteredCommand),
    Skill(protocol::SkillInfo),
    PromptTemplate(protocol::PromptTemplateInfo),
}
```

`Kernel::available_commands` 使用一个 `BTreeSet` 统一保留名称，按 Builtin、Extension、Skill、Template 分段构建并排序。Extension 歧义短名称仍被保留为不可达名称，避免发布低优先级同名命令。

- [ ] **步骤 5：运行 Extension 与 Kernel 命令目录测试并确认 GREEN**

```sh
rtk cargo test -p extension --test dynamic
rtk cargo test -p kernel --test slash_commands
rtk cargo test -p kernel --test session_capabilities available_commands
```

- [ ] **步骤 6：运行格式与差异检查，不提交**

```sh
rtk cargo fmt --all -- --check
rtk git diff --check
```

---

### 任务 3：统一执行上下文、Pi 输入顺序和消息持久化

**文件：**

- 修改：`crates/kernel/src/runtime/command.rs`
- 新建：`crates/kernel/src/runtime/command/execution.rs`
- 修改：`crates/kernel/src/runtime/input.rs`
- 修改：`crates/kernel/src/runtime/run.rs`
- 修改：`crates/kernel/src/runtime/queue.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/tree.rs`
- 修改：`crates/kernel/src/runtime/compaction.rs`
- 修改：`crates/kernel/src/provider.rs`
- 修改：`crates/kernel/tests/slash_commands.rs`
- 修改：`crates/kernel/tests/lifecycle_order.rs`
- 修改：`crates/kernel/tests/session_capabilities.rs`

**接口：**

- 产出：Kernel 私有 `SlashCommandExecution`，字段为 Session、RunId、TurnId、TraceId、Event Sink 和时间戳，使用 typed-builder。
- 产出：`SlashCommandExecution::execute_direct`、`persist_invocation`、`persist_output` 类型方法。
- 消费：`RunInput::slash_command_text()` 与任务 2 的目录解析。
- 保证：Provider 读取 `ExpandedUser.model_blocks`，ACP 后续读取原始 invocation。

- [ ] **步骤 1：增加执行顺序、Turn 关联和双投影失败测试**

```rust
#[tokio::test]
async fn handled_extension_command_persists_one_correlated_command_turn() {
    let result = fixture.kernel.run(
        RunRequest { session_id: fixture.session_id.clone(), input: RunInput::Text("/sample:handled alpha".to_string()) },
        fixture.sink.clone(),
    ).await.expect("command");
    assert!(result.turns.is_empty());
    let invocation = result.messages.iter().find(|message| {
        matches!(message.content, MessageContent::SlashCommand { .. })
    }).expect("persisted command invocation");
    let started = fixture.events().into_iter().find(|event| {
        matches!(event.payload, AgentEventPayload::SlashCommandStart { .. })
    }).expect("command start");
    assert_eq!(invocation.identity.turn_id, started.metadata.turn_id);
    assert!(fixture.model.requests().is_empty());
}

#[tokio::test]
async fn skill_replay_keeps_original_while_provider_receives_expansion() {
    fixture.run("/skill:review src/lib.rs").await;
    assert_eq!(fixture.transcript_user_text(), "/skill:review src/lib.rs");
    assert!(fixture.provider_user_text().contains("<skill name=\"review\""));
}
```

测试同时断言事件、命令输入和输出具有同一 TurnId，三个时间字段有效；input Hook 仍位于 Extension Command 后、Skill/Template 前；展开结果不递归执行。

- [ ] **步骤 2：运行目标测试并确认 RED**

```sh
rtk cargo test -p kernel --test slash_commands handled_extension_command
rtk cargo test -p kernel --test slash_commands skill_replay
rtk cargo test -p kernel --test lifecycle_order prompt_order_matches_pi
```

- [ ] **步骤 3：在 Run 入口预分配命令上下文**

`run_with_source` 保留 User Bash 第一优先级，然后获取 Session 并一次性生成 RunId、首 TurnId 和开始时间。Builtin/Extension 直接路由使用该上下文；`NotFound` 继续使用相同标识进入现有 run gate 和 Agent 流程。Extension Context 必须接收 `Some(&run_id)` 与 `Some(&turn_id)`。

识别到直接命令时按顺序执行：`SlashCommandStart`、持久化 Invocation、执行 handler、持久化可选 Output、`SlashCommandEnd`。失败也必须完成 Output 与结束状态，不将已识别命令送入 Agent。

- [ ] **步骤 4：实现展开用户消息的显示/模型双投影**

Skill/Template 返回 `SlashCommandExpansion`。`run.rs` 持久化 `MessageContent::ExpandedUser`；`provider.rs` 将其 `model_blocks` 映射为 User 内容；ACP 后续只展示 `invocation.original`。`session.rs` 默认标题使用原始文本，`tree.rs` 将 ExpandedUser 视为 User，`compaction.rs` 使用 model blocks 估算和序列化上下文。

- [ ] **步骤 5：统一 Queue 行为**

`queue_message` 使用同一目录判断 Builtin 和 Extension 直接命令并返回 `Busy`/不可排队错误；Skill 和 Template 可以进入 Queue，但持久化 `ExpandedUser`，保留原始输入和已冻结展开内容。不得在消费 Queue 时重新读取 Skill 或 Template。

具体 Kernel 错误为 `KernelError::SlashCommandCannotQueue { name: String, source: SlashCommandSource }`；ACP FollowUp 将其映射为可读 Busy 响应，不把命令作为普通队列消息保存。

- [ ] **步骤 6：运行相关 Kernel 测试并确认 GREEN**

```sh
rtk cargo test -p kernel --test slash_commands
rtk cargo test -p kernel --test lifecycle_order
rtk cargo test -p kernel --test session_capabilities
rtk cargo test -p kernel --test compaction
```

- [ ] **步骤 7：检查单文件职责**

确认 `run.rs` 只保留流程编排，命令持久化和执行位于 `command/execution.rs`；不为每个命令创建只有数行的 free helper。运行：

```sh
rtk cargo fmt --all -- --check
rtk git diff --check
```

---

### 任务 4：实现 `/name` 与 `/session`

**文件：**

- 修改：`crates/kernel/src/runtime/command/builtin.rs`
- 新建：`crates/kernel/src/runtime/command/session_stats.rs`
- 修改：`crates/kernel/src/runtime/command.rs`
- 修改：`crates/kernel/tests/slash_commands.rs`
- 修改：`crates/kernel/tests/session_capabilities.rs`

**接口：**

- 产出：Kernel 私有 `SessionStatistics`，使用 typed-builder，并实现 `Display`。
- 产出：`BuiltinSlashCommand::execute(&SlashCommandExecution, &str) -> Result<SlashCommandOutcome, KernelError>`。
- 复用：现有 `Kernel::rename_session` 和 `SessionTitleChanged`。

- [ ] **步骤 1：增加 Builtin 行为失败测试**

```rust
#[tokio::test]
async fn name_command_sets_and_queries_the_persisted_title() {
    fixture.run("/name Slash E2E").await;
    assert_eq!(fixture.kernel.session_tree(&fixture.session_id).unwrap().name.as_deref(), Some("Slash E2E"));
    assert!(fixture.events().iter().any(|event| matches!(event.payload, AgentEventPayload::SessionTitleChanged { .. })));
    let query = fixture.run("/name").await;
    assert_command_output_contains(&query.messages, "Slash E2E");
}

#[tokio::test]
async fn session_command_reports_persisted_usage_without_calling_provider() {
    let before = fixture.model.request_count();
    let result = fixture.run("/session").await;
    assert_command_output_contains(&result.messages, "Session ID");
    assert_eq!(fixture.model.request_count(), before);
}
```

同时覆盖 `/name` 无现有名称时的 usage、`/session unexpected` 的 `InvalidArguments`，以及错误输出不进入 Provider 上下文。

- [ ] **步骤 2：运行 Builtin 测试并确认 RED**

```sh
rtk cargo test -p kernel --test slash_commands name_command
rtk cargo test -p kernel --test slash_commands session_command
```

- [ ] **步骤 3：实现 `/name`**

参数非空时将完整参数交给 `SessionTitle::try_from` 并调用 `rename_session`；参数为空时读取 Store 当前名称。成功输出 `Session name set: {title}` 或 `Session name: {title}`，无名称输出 `Usage: /name <name>`，状态为 Failed 但不返回 Kernel transport error。

- [ ] **步骤 4：实现类型化 Session 统计**

`SessionStatistics::from_transcript` 统计 User、ExpandedUser、Assistant、ToolResult 和 ToolCall，聚合 Assistant metadata 中的 token usage。Slash Command Invocation/Output、System、Bash 和 Extension 分别按 Pi 语义处理。费用在当前持久化 metadata 不可计算时不展示，也不写假零值。使用 `Display` 生成稳定 Markdown，而不是多个格式化 helper。

- [ ] **步骤 5：运行 Builtin 与 Session 测试并确认 GREEN**

```sh
rtk cargo test -p kernel --test slash_commands
rtk cargo test -p kernel --test session_capabilities
```

- [ ] **步骤 6：检查日志和差异**

只记录命令开始、成功/拒绝/失败及 SessionId、RunId、TurnId，不记录 `/name` 参数正文。运行 `rtk git diff --check`。

---

### 任务 5：让 `/compact` 复用命令 Turn 并支持自定义指令

**文件：**

- 修改：`crates/kernel/src/runtime/command/builtin.rs`
- 修改：`crates/kernel/src/runtime/compaction.rs`
- 修改：`crates/kernel/src/runtime/extension/host.rs`
- 修改：`crates/kernel/tests/compaction.rs`
- 修改：`crates/kernel/tests/slash_commands.rs`
- 修改：`crates/acp/src/extension.rs`
- 修改：`crates/acp/tests/extension.rs`

**接口：**

- 产出：`ManualCompaction` 类型，包含 Session、可选自定义指令和可选预分配 operation identity，使用 typed-builder。
- 保留：ACP Compact 扩展入口仍可供独立 Session 管理 UI 使用，但内部建立与 Slash 路径等价的上下文。
- 保证：Slash `/compact` 的 Invocation、CompactionStart、Provider 请求、CompactionEnd 和 Output 使用同一 TurnId 与 trace。

- [ ] **步骤 1：增加自定义指令与关联失败测试**

```rust
#[tokio::test]
async fn compact_command_uses_custom_instruction_and_one_turn() {
    let result = fixture.run("/compact Preserve API decisions").await.expect("compact");
    assert!(fixture.compaction_prompt().contains("Preserve API decisions"));
    assert_single_turn_for_command_and_compaction(&fixture.events(), &result.messages);
}
```

现有手动 Compact 扩展测试增加断言：未传自定义指令时仍使用 `COMPACTION_INSTRUCTION`，失败时不写成功 compaction entry。

- [ ] **步骤 2：运行 Compact 测试并确认 RED**

```sh
rtk cargo test -p kernel --test slash_commands compact_command
rtk cargo test -p kernel --test compaction compact_streams
```

- [ ] **步骤 3：重构压缩执行身份**

`compact_session_with_reason` 不再无条件创建 RunId/TurnId。Slash Command 传入预分配 identity；阈值、溢出和旧 ACP 管理入口在调用边界创建 identity。`perform_compaction` 只消费已经完整的 `CompactionExecution`。自定义指令为空时回到默认常量，非空时原样传给 `SummaryGeneration.instruction`。

- [ ] **步骤 4：实现 `/compact` Builtin**

命令先通过 Session operation gate，执行现有 before/after compact Hook、Store 原子写入和流式事件。成功输出简短压缩结果；取消、Busy 和 Provider 失败转换为类型化命令错误，同时保留原始 Kernel 错误链日志。

- [ ] **步骤 5：运行 Kernel 与 ACP Compact 测试并确认 GREEN**

```sh
rtk cargo test -p kernel --test compaction
rtk cargo test -p kernel --test slash_commands
rtk cargo test -p acp --test extension compact
```

- [ ] **步骤 6：运行格式和 Clippy 局部门禁**

```sh
rtk cargo fmt --all -- --check
rtk cargo clippy -p kernel -p acp --all-targets -- -D warnings
```

---

### 任务 6：完成 ACP v2 命令发现、执行、状态和回放映射

**文件：**

- 修改：`crates/acp/src/server.rs`
- 修改：`crates/acp/src/mapping.rs`
- 修改：`crates/acp/tests/mapping.rs`
- 修改：`crates/acp/tests/extension.rs`
- 新建：`crates/acp/tests/slash_commands.rs`
- 修改：`crates/protocol/src/identity.rs`

**接口：**

- 消费：统一 `SlashCommandDefinition` 和两种新增 MessageContent。
- 产出：每条 ACP Available Command 的产品 metadata：`source`、`qualifiedName`、`aliasKind`。
- 产出：命令消息产品 metadata：`name`、`source`、`status`、`messageKind`。
- 保持：命令输入只使用标准 `session/prompt`，实时和 replay 使用同一 `AcpEventMapper`。

- [ ] **步骤 1：增加 ACP 映射与端到端协议失败测试**

```rust
#[test]
fn command_output_maps_to_agent_message_with_product_metadata() {
    let updates = AcpEventMapper::map(command_output_event()).expect("map");
    let value = serde_json::to_value(&updates[0]).expect("serialize");
    assert_eq!(value["sessionUpdate"], "agent_message");
    assert_eq!(value["_meta"][ProductIdentity::ACP_NAMESPACE]["slashCommand"]["messageKind"], "output");
}
```

协议连接测试通过 `session/prompt` 依次执行 `/session` 和 `/name ACP Session`，断言 Running/AgentMessage/SessionInfo/Idle 的顺序、TurnId 一致，以及 resume from start 得到相同命令消息顺序。Available Commands 快照断言四类来源与 metadata。

- [ ] **步骤 2：运行 ACP 测试并确认 RED**

```sh
rtk cargo test -p acp --test mapping command_output
rtk cargo test -p acp --test slash_commands
```

- [ ] **步骤 3：让 ACP Prompt 保留命令资格**

`PromptInput` 解析器记录输入是否仅由 Text block 组成：纯文本构造 `RunInput::Text`；包含 ResourceLink 或 Other 的受支持输入构造 `RunInput::Composite`。不支持的 Image/Audio/Embedded Resource 行为保持现状。ACP 层不解析命令名称。

- [ ] **步骤 4：映射 Available Commands 与命令消息**

Available Command 的标准字段继续承载 name、description、text hint，每条命令自身的 `_meta` 使用 `ProductIdentity::ACP_NAMESPACE`。ExpandedUser 映射为展示原始 invocation 的 `user_message`；Slash Invocation 映射为 `user_message`；Slash Output 映射为 `agent_message`。三者都在 metadata 中保留完整 message timing。

- [ ] **步骤 5：映射命令状态**

`SlashCommandStart` 产生 Running StateUpdate；`SlashCommandEnd` 产生 Idle StateUpdate，失败状态使用产品 namespace metadata 携带稳定错误类别。命令领域错误不返回异步 JSON-RPC error。未知 Slash 输入仍进入普通 Agent Run。

- [ ] **步骤 6：运行 ACP 全部测试并确认 GREEN**

```sh
rtk cargo test -p acp --test mapping
rtk cargo test -p acp --test extension
rtk cargo test -p acp --test slash_commands
rtk cargo test -p acp
```

- [ ] **步骤 7：检查 ACP 参数日志**

确认 `session/prompt` DEBUG 参数日志仍打印用户输入并执行递归脱敏，命令实现不新增第二份参数日志。运行 `rtk git diff --check`。

---

### 任务 7：升级 WebUI 通用命令 metadata 与命令卡片

**文件：**

- 修改：`web/src/domain/model.ts`
- 修改：`web/src/workspace/updateDecoder.ts`
- 修改：`web/src/workspace/state.ts`
- 修改：`web/src/features/composer/CommandPalette.tsx`
- 修改：`web/src/features/composer/Composer.tsx`
- 新建：`web/src/features/conversation/CommandMessageCard.tsx`
- 修改：`web/src/features/conversation/MessageView.tsx`
- 修改：`web/src/features/skills/SkillCard.tsx`
- 修改：`web/src/features/inspector/SessionTree.tsx`
- 修改：`web/src/workspace/controller.ts`
- 修改：`web/src/theme/workbench.css`
- 修改：`README.md`

**接口：**

- `AvailableCommandEntity` 增加 `source`、`qualifiedName`、`aliasKind` 可选字段，保持普通 ACP Agent 兼容。
- `MessageEntity` 增加可选 `slashCommand: SlashCommandMessageMeta`。
- `CommandMessageCard` 只消费解码后的 MessageEntity，不读取 ACP 原始对象。
- `WorkspaceController.send` 保持唯一 Slash Command 提交入口。

- [ ] **步骤 1：扩展前端领域类型和严格解码**

`decodeAvailableCommands` 从每个 command 的产品 `_meta` 解码来源和别名，字段错误只忽略扩展 metadata，不丢弃标准命令。完整 `agent_message`/`user_message` 从 product `slashCommand` 解码命令名、来源、状态和消息类型；未知值回退为普通消息。

- [ ] **步骤 2：升级 Command Palette**

Palette 展示来源 badge 和限定名称信息，但筛选、键盘导航和选择仍只依赖服务端快照。不得添加 Builtin 名称数组或按名称分支。Composer 继续把选择结果写为 `/name ` 并通过 `controller.send` 提交。

- [ ] **步骤 3：实现命令消息卡片**

`MessageView` 在 `message.slashCommand` 存在时委托 `CommandMessageCard`，卡片显示 `/command`、来源、成功/失败状态、Markdown 内容、TurnId 和三个时间字段。无 metadata 的 AgentMessage 继续走现有模型消息样式。

- [ ] **步骤 4：让现有 Skill 与 Compact 操作走 Slash Command**

`SkillCard` 调用：

```ts
await controller.send({
  text: `/skill:${skill.name}${input.trim().length === 0 ? "" : ` ${input.trim()}`}`,
  resources: []
});
```

`SessionTree` 的 Compact 按钮调用 `controller.send({ text: "/compact", resources: [] })`。删除不再使用的 `WorkspaceController.invokeSkill` 与 `compact`。侧边栏针对任意 Session 的 rename 仍是独立 Session 管理能力，不改写成 Slash Command。

- [ ] **步骤 5：更新 README**

记录四类来源、优先级、限定名、三个 Builtin、标准 `session/prompt` 执行方式，以及 WebUI 不在浏览器解析/展开命令。

- [ ] **步骤 6：运行前端门禁**

```sh
cd web
rtk npm run check
rtk npm run build
```

预期：TypeScript、ESLint 和生产构建全部成功且无 warning。前端按项目规则不新增单元测试。

---

### 任务 8：全量审查、门禁和真实 WebUI E2E

**文件：**

- 审查：本计划涉及的全部 Rust、TypeScript、CSS 和文档文件
- 不新增 fixture、mock ACP Server 或生产测试支撑代码

- [ ] **步骤 1：运行格式、编译和 Clippy**

```sh
rtk cargo fmt --all -- --check
rtk cargo check --workspace --all-targets
rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
```

- [ ] **步骤 2：运行全部 Rust 测试**

```sh
rtk cargo test --workspace
```

确认所有测试通过，ignored 数量与基线一致；任何新失败先写/保留最小回归测试再修复。

- [ ] **步骤 3：运行 WebUI 门禁与 pre-commit**

```sh
cd web
rtk npm run check
rtk npm run build
cd ..
rtk pre-commit run --all-files
```

- [ ] **步骤 4：启动正式后端**

使用仓库根目录固定 `claw.toml`、正式 app、配置中的真实 Provider 和构建后的 WebUI：

```sh
rtk cargo run -p app --bin clawcode -- serve --bind 127.0.0.1:3000 --web-root web/dist
```

不得启动 `webui_fixture`，不得替换 Provider，日志中不得输出认证信息。

- [ ] **步骤 5：通过真实浏览器验证命令发现和执行**

打开 `http://127.0.0.1:3000/`，创建真实 Session，并验证：

1. `/` Palette 同时包含 Builtin、已启用 Extension、Skill 和 Prompt Template；Builtin 为 `compact`、`name`、`session`。
2. Extension 限定名使用 `extension-id:command`，歧义短名称不出现。
3. `/session` 生成命令卡片且不产生模型 Assistant metadata。
4. `/name Slash E2E` 生成命令卡片，并由 `session_info_update` 更新侧边栏。
5. 从 Skill 页面调用 Skill 时，Composer/ACP 发送 `/skill:name`，服务端展开后真实 Provider 正常回答。

- [ ] **步骤 6：验证真实 `/compact` 与回放**

先发送一条真实 Prompt 并等待 Provider 完整回复，再发送 `/compact Preserve API decisions`。确认日志中的同一 trace 从 ACP 进入 Kernel 和 Provider，UI 依次显示 Running、压缩流、命令结果和 Idle，且 TurnId 一致。

刷新页面并 Resume from start，确认原始 Slash 输入、命令输出、Thinking/压缩事件和 Session 标题按 sequence 原顺序回放；命令卡片不变成普通模型消息。

- [ ] **步骤 7：最终代码质量审查**

检查：

- 公共类型只存在于 `protocol`，无跨 crate 重复结构体。
- `run.rs`、`mapping.rs`、`updateDecoder.ts` 未因本功能形成新的混合职责；可独立语义已下沉到命令模块或类型 `impl`。
- 无只有一个自定义类型参数的 free helper。
- 无硬编码项目名称、ACP namespace 或 Builtin 前端表。
- 无无意义逐 Token 日志、tracing wildcard import 或 structured event field 风格。
- Store 未增加 `index.jsonl`，Fork/Tree/Resume 继续读取同一 JSONL 消息模型。

- [ ] **步骤 8：记录验证结果，不提交**

```sh
rtk git diff --check
rtk git status --short
```

向用户报告实际测试数量、ignored 数、WebUI E2E 操作和任何剩余风险。等待用户明确要求后才进入 commit 流程。
