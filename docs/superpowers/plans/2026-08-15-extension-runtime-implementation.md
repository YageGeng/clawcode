# Extension 运行时实施计划

> **执行要求：** 使用 `superpowers:executing-plans` 在当前会话逐任务实施。禁止使用 SubAgent 和 worktree。每个任务采用 RED→GREEN→REFACTOR，并使用复选框跟踪。

**目标：** 将当前无载荷的统一 Extension Pipeline 重构为 Pi 兼容的类型化、Session 隔离、可持久化且可通过 ACP 2.0 回放的非 UI Extension 运行时。

**架构：** `protocol` 定义所有公共事件与结果；`extension` 使用 Point Marker、泛型 Handler、Registrar 和每 Session Runtime 执行组合策略；`kernel` 实现 Host capability 并在真实生命周期边界调用 Runtime；`provider` 只提供中立请求 Hook；`store` 和 `acp` 分别负责持久化与协议映射。Handler Registry 每 Session 冻结，Tools 与 Commands 使用版本化动态快照。

**技术栈：** Rust 2024、Tokio、async-trait、typed-builder、serde、现有 Provider Factory、ACP Rust SDK 2.x、React/Vite WebUI。

## 全局约束

- 保留 `provider` 和 `config` 的职责，不实现配置热更新。
- 不实现 UI Extension、Client FS callback 或 Client Terminal callback。
- 不实现 WASM、动态库、脚本、进程外加载或 reload。
- 不实现运行期 Provider 注册或注销；只允许构建期静态 Provider 声明。
- 每条消息必须带 TurnId 和字符串毫秒时间戳；流式消息保存创建、首内容和末内容时间。
- 所有公共类型只定义在 `protocol`，其他 crate 不重复定义结构体。
- 新函数必须有英文函数级注释；非平凡逻辑必须有英文原因注释。
- 单参数且参数为项目自定义类型的 helper 优先实现为类型的关联方法或标准 Trait。
- 测试只放在 crate 的 `tests/` 目录；正文不增加测试支撑代码。
- Cargo 依赖先放 Workspace 相对路径依赖，再按根 Cargo.toml 语义分组；版本统一声明在根 `[workspace.dependencies]`。
- 不创建 commit，除非用户再次明确授权。
- 所有 shell 命令使用 `rtk` 前缀。

---

## Task 1：建立 Extension 公共协议类型

**文件：**

- 删除：`crates/protocol/src/extension.rs`
- 创建：`crates/protocol/src/extension/mod.rs`
- 创建：`crates/protocol/src/extension/context.rs`
- 创建：`crates/protocol/src/extension/result.rs`
- 创建：`crates/protocol/src/extension/registration.rs`
- 创建：`crates/protocol/src/extension/events/mod.rs`
- 创建：`crates/protocol/src/extension/events/startup.rs`
- 创建：`crates/protocol/src/extension/events/session.rs`
- 创建：`crates/protocol/src/extension/events/agent.rs`
- 创建：`crates/protocol/src/extension/events/provider.rs`
- 创建：`crates/protocol/src/extension/events/model.rs`
- 创建：`crates/protocol/src/extension/events/tool.rs`
- 修改：`crates/protocol/src/id.rs`
- 修改：`crates/protocol/src/message.rs`
- 修改：`crates/protocol/src/event.rs`
- 修改：`crates/protocol/src/lib.rs`
- 测试：`crates/protocol/tests/extensions.rs`

**接口：**

- 产出 `ExtensionId`、`ExtensionDescriptor`、`ExtensionInvocation`、`ExtensionMessageDraft`、`ExtensionEntryData` 和 `StaticExtensionRegistration`。
- 每个事件结构体只包含该 Point 的业务载荷。
- 产出 `InputResult`、`ContextResult`、`BeforeAgentStartResult`、`HeaderPatch`、`ToolCallResult`、`ToolResultPatch`、Session cancel/custom summary 结果。
- `AgentEventPayload` 增加 `ExtensionHandlerFailed` 和必要的 Extension 诊断载荷。
- `ExtensionMessage` 改为 `extension_id/custom_type/blocks/display/include_in_context/details`，不再使用无法验证的 `discriminator/payload/meta` 三元组。

- [ ] **Step 1：先写协议序列化失败测试**

在 `crates/protocol/tests/extensions.rs` 写入真实 round-trip 测试，至少覆盖：

```rust
#[test]
fn extension_invocation_serializes_precision_safe_time() {
    let value = serde_json::to_value(ExtensionInvocation::builder()
        .extension_id(ExtensionId::try_from("audit").expect("extension id"))
        .session_id(SessionId::try_from("session-1").expect("session id"))
        .timestamp_ms(TimestampMs::from(u64::MAX))
        .cwd(PathBuf::from("/workspace"))
        .build())
        .expect("serialize invocation");

    assert_eq!(value["timestamp_ms"], serde_json::json!(u64::MAX.to_string()));
}

#[test]
fn tool_call_result_cannot_represent_unrelated_directives() {
    let result = ToolCallResult::Block(ToolBlock::builder()
        .reason("denied".to_string())
        .terminate(true)
        .build());

    assert_eq!(serde_json::to_value(result).expect("serialize")["type"], "block");
}
```

- [ ] **Step 2：运行测试并确认 RED**

运行：`rtk cargo test -p protocol --test extensions`

预期：编译失败，提示 `ExtensionInvocation`、`ToolCallResult` 和新模块不存在。

- [ ] **Step 3：实现最小公共类型**

实现 `ExtensionId` 的 `TryFrom<String>`、`TryFrom<&str>`、`Display` 和 serde；所有超过 3 个字段的结构体使用 `typed-builder`。删除统一 `ExtensionDirective` 和 `ExtensionEffects`，按 Spec §7 列出的全部非 UI Point 定义 Event/Result。

- [ ] **Step 4：迁移 ExtensionMessage 和错误事件**

让 Kernel 生成身份之前使用 `ExtensionMessageDraft`，持久化后使用完整 `ExtensionMessage`。新增：

```rust
ExtensionHandlerFailed {
    extension_id: ExtensionId,
    point: String,
    message: String,
}
```

错误载荷不保存 stack、Header、Provider payload 或配置正文。

- [ ] **Step 5：运行协议测试和格式检查**

运行：

```bash
rtk cargo test -p protocol --test extensions
rtk cargo test -p protocol --all-targets
rtk cargo fmt --all -- --check
```

预期：全部通过，旧 `ExtensionEvent/Directive/Effects` 的引用仍可在后续 crate 编译阶段暴露为待迁移错误。

## Task 2：实现类型化 Point、Handler 和 Registrar

**文件：**

- 删除：`crates/extension/src/contract.rs`
- 删除：`crates/extension/src/pipeline.rs`
- 创建：`crates/extension/src/point.rs`
- 创建：`crates/extension/src/handler.rs`
- 创建：`crates/extension/src/registrar.rs`
- 创建：`crates/extension/src/registry.rs`
- 创建：`crates/extension/src/factory.rs`
- 创建：`crates/extension/src/error.rs`
- 修改：`crates/extension/src/lib.rs`
- 删除：`crates/extension/tests/pipeline.rs`
- 创建：`crates/extension/tests/registration.rs`

**接口：**

```rust
pub trait ExtensionPoint: Send + Sync + 'static {
    type Event: Send + Sync;
    type Output: Send;
    const NAME: &'static str;
}

#[async_trait]
pub trait ExtensionHandler<P: ExtensionPoint>: Send + Sync {
    async fn handle(
        &self,
        event: &P::Event,
        context: &ExtensionContext,
    ) -> Result<P::Output, ExtensionError>;
}

pub trait ExtensionModule: Send + Sync {
    fn descriptor(&self) -> ExtensionDescriptor;
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError>;
}

pub trait ExtensionFactory: Send + Sync {
    fn static_registration(
        &self,
    ) -> Result<StaticExtensionRegistration, ExtensionError>;

    fn create_modules(
        &self,
    ) -> Result<Vec<Arc<dyn ExtensionModule>>, ExtensionError>;
}
```

- [ ] **Step 1：写 Registrar RED 测试**

测试注册两个 `ContextPoint` Handler 和一个 `TurnStartPoint` Handler，冻结 Registry 后断言 Point 数量和来源顺序；测试重复 ExtensionId、重复 Flag 和同 Extension 重复 Command 返回明确错误。

- [ ] **Step 2：运行 RED**

运行：`rtk cargo test -p extension --test registration`

预期：编译失败，提示 `ExtensionPoint`、`ExtensionRegistrar` 和 Marker 类型不存在。

- [ ] **Step 3：实现 Point Marker 和无 Any 注册存储**

`point.rs` 为每个 Point 实现 `ExtensionPoint`。`registrar.rs` 提供：

```rust
pub fn on<P, H>(&mut self, handler: H) -> Result<(), ExtensionError>
where
    P: RegisterPoint,
    H: ExtensionHandler<P> + 'static;
```

`RegisterPoint` 把 `Arc<dyn ExtensionHandler<P>>` 写入 startup/session/agent/provider/model/tool/input 的强类型注册枚举。生产实现不得使用 `Any`、`TypeId` 或 JSON Event。

- [ ] **Step 4：实现每 Session 新 Module 契约**

`StaticExtensionFactory` 保存 Module Factory 闭包或可重复创建的 Module 定义，`create_modules()` 每次返回新的 Module/Handler 实例。测试使用带实例序号的 Module 证明两个调用不会返回同一个状态对象。

- [ ] **Step 5：运行 Extension 注册测试**

运行：

```bash
rtk cargo test -p extension --test registration
rtk cargo check -p extension --all-targets
```

预期：通过且无 warning。

## Task 3：实现类型化组合策略和错误隔离

**文件：**

- 创建：`crates/extension/src/runtime/mod.rs`
- 创建：`crates/extension/src/runtime/startup.rs`
- 创建：`crates/extension/src/runtime/session.rs`
- 创建：`crates/extension/src/runtime/agent.rs`
- 创建：`crates/extension/src/runtime/provider.rs`
- 创建：`crates/extension/src/runtime/model.rs`
- 创建：`crates/extension/src/runtime/tool.rs`
- 创建：`crates/extension/src/runtime/input.rs`
- 创建：`crates/extension/tests/runtime.rs`

**接口：**

- `ExtensionRuntime` 对外提供语义方法，例如 `emit_context`、`emit_input`、`emit_tool_call`，不提供接受任意 Event 的统一 `dispatch`。
- 普通错误写入 `ExtensionDiagnosticSink` 后继续。
- `emit_tool_call` 把 Handler 错误转换为 `ToolCallResult::Block`。
- 链式事件每个后续 Handler 接收当前值；观察事件返回 `()`。

- [ ] **Step 1：写组合规则 RED 测试**

覆盖：Context A→B 链式替换、Input transform→handled 短路、Session cancel 短路、普通观察错误后续 Handler 继续、ToolCall 错误阻止工具。

关键断言：

```rust
assert_eq!(context_result.messages, second_handler_output);
assert_eq!(called.load(Ordering::SeqCst), 2);
assert!(matches!(tool_result, ToolCallResult::Block(_)));
assert_eq!(diagnostics.len(), 1);
```

- [ ] **Step 2：运行 RED**

运行：`rtk cargo test -p extension --test runtime`

预期：编译失败或行为断言失败，因为 Runtime 组合器尚不存在。

- [ ] **Step 3：实现领域 Runtime**

每个领域文件只实现本领域事件的组合策略。复杂链式状态使用有名称的领域类型，例如 `ContextChain`、`ToolResultChain`，不拆分成大量单参数 free helper。

- [ ] **Step 4：实现并发安全验证**

测试两个 Tool update Handler 通过 barrier 同时进入，证明 Runtime 不持有全局 Extension Mutex；同一个 Point 内仍按注册顺序串行。

- [ ] **Step 5：运行 Extension 全部测试**

运行：`rtk cargo test -p extension --all-targets`

预期：通过。

## Task 4：增加 Tool 预检和版本化动态快照

**文件：**

- 修改：`crates/tools/src/contract.rs`
- 修改：`crates/tools/src/lib.rs`
- 修改：`crates/tools/src/builtin/bash.rs`
- 修改：`crates/tools/src/builtin/read.rs`
- 修改：`crates/tools/src/builtin/write.rs`
- 修改：`crates/tools/src/builtin/edit.rs`
- 创建：`crates/tools/tests/registry.rs`
- 修改：`crates/tools/tests/bash.rs`
- 修改：`crates/tools/tests/filesystem.rs`

**接口：**

```rust
pub trait AgentTool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        let definition = self.definition();
        if definition.name == call.name {
            Ok(())
        } else {
            Err(ToolError::NotFound(call.name.clone()))
        }
    }
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError>;
}
```

`ToolRegistry` 增加 `tool`、`validate`、`upsert` 和 `remove`；`DynamicToolRegistry` 使用 `RwLock<Arc<ToolRegistry>>`，读取时克隆 Arc 后立即释放锁。

- [ ] **Step 1：写预检与快照 RED 测试**

测试无效 Bash/Read 参数在 `validate()` 阶段失败；测试旧快照执行旧 Tool，新快照执行覆盖后的 Tool；测试移除 Tool 同时使新快照不可见。

- [ ] **Step 2：运行 RED**

运行：`rtk cargo test -p tools --test registry`

预期：编译失败，`validate` 和动态 Registry 尚不存在。

- [ ] **Step 3：用类型转换复用参数校验**

为 `BashArguments`、`ReadArguments`、`WriteArguments` 和 `EditArguments` 实现 `TryFrom<&ToolCall>` 或领域校验方法。`validate` 与 `execute` 使用同一转换，不复制 JSON 解析规则，也不增加只调用一次的 parse helper。Trait 的默认 `validate` 只校验 Tool 名称，保证第三方 Tool 源码兼容；四个 Built-in Tool 必须覆盖它并执行完整参数校验。

- [ ] **Step 4：实现版本化快照**

写锁内克隆当前 BTreeMap 并提交新 `Arc<ToolRegistry>`；执行路径只持有快照 Arc。覆盖策略由调用者显式选择，基础 `register` 仍保留重复错误语义。

- [ ] **Step 5：运行 Tools 测试**

运行：`rtk cargo test -p tools --all-targets`

预期：全部通过。

## Task 5：实现 Extension Context、Host capability 和动态 Commands

**文件：**

- 创建：`crates/extension/src/context.rs`
- 创建：`crates/extension/src/host.rs`
- 创建：`crates/extension/src/command.rs`
- 创建：`crates/extension/src/dynamic.rs`
- 修改：`crates/extension/src/runtime/mod.rs`
- 创建：`crates/extension/tests/context.rs`
- 创建：`crates/extension/tests/dynamic.rs`

**接口：**

```rust
#[async_trait]
pub trait ExtensionHost: Send + Sync {
    fn snapshot(&self, invocation: &ExtensionInvocation)
        -> Result<ExtensionSnapshot, ExtensionHostError>;
    async fn send_extension_message(
        &self,
        invocation: &ExtensionInvocation,
        draft: ExtensionMessageDraft,
    ) -> Result<AgentMessage, ExtensionHostError>;
    async fn send_user_message(
        &self,
        invocation: &ExtensionInvocation,
        request: ExtensionUserMessage,
    ) -> Result<(), ExtensionHostError>;
    async fn append_entry(
        &self,
        invocation: &ExtensionInvocation,
        entry: ExtensionEntryData,
    ) -> Result<EntryId, ExtensionHostError>;
    async fn exec(
        &self,
        invocation: &ExtensionInvocation,
        request: ExtensionExecRequest,
    ) -> Result<ExtensionExecResult, ExtensionHostError>;
}
```

同一 Trait 明确定义 `is_idle`、`has_pending_messages`、`abort`、`shutdown`、`compact`、`system_prompt`、`send_extension_message`、`send_user_message`、`append_entry`、`set_session_name`、`set_label`、`exec`、`active_tools`、`all_tools`、`set_active_tools`、`register_tool`、`unregister_tool`、`commands`、`flags`、`register_command`、`unregister_command`、`set_model`、`thinking_level`、`set_thinking_level` 和 `publish_event`。不建立每个方法一个 capability trait，也不提供 Provider 注册方法。

- [ ] **Step 1：写 Context 失效与动态注册 RED 测试**

测试 Context 通过 Runtime generation 检查失效；测试动态 Tool/Command 只影响本 Runtime；测试 unknown Active Tool 返回错误；测试 Command 快照在分发期间不变化。

- [ ] **Step 2：运行 RED**

运行：

```bash
rtk cargo test -p extension --test context
rtk cargo test -p extension --test dynamic
```

预期：缺少 Host 和 Dynamic Registry API。

- [ ] **Step 3：实现 Context 与 generation guard**

`ExtensionContext` 使用 `typed-builder`，保存 Invocation、Snapshot、Host Arc 和 Runtime generation。所有 Action 调用先检查 generation；Session shutdown 后返回 `ExtensionHostError::StaleRuntime`。

- [ ] **Step 4：实现 Command Context**

`ExtensionCommandContext` 在普通 Context 基础上增加 `wait_for_idle`、`create_session`、`switch_session`、`fork_session` 和 `navigate_tree`。这些方法不出现在普通 Event Context。

- [ ] **Step 5：运行 Extension 全量验证**

运行：`rtk cargo test -p extension --all-targets`

预期：通过。

## Task 6：持久化 Extension Entry 和完整 Extension Message

**文件：**

- 修改：`crates/store/src/model.rs`
- 修改：`crates/store/src/session.rs`
- 修改：`crates/store/src/state.rs`
- 修改：`crates/store/src/factory.rs`
- 修改：`crates/store/src/lib.rs`
- 修改：`crates/store/tests/jsonl.rs`
- 创建：`crates/store/tests/extensions.rs`

**接口：**

- `SessionStore::append_extension_entry` 接收协议层 `ExtensionEntryData` 并返回完整 `SessionEntry`。
- Extension Entry 使用现有 `EntryKind::Custom`，payload 固定包含 `extensionId/customType/data`。
- Extension Message 仍通过 Message Entry 持久化，保留完整身份、三段时间和 context behavior。

- [ ] **Step 1：写 Store RED 测试**

覆盖 append→reopen→replay、Fork 指定 leaf、Tree 导航和无 `index.jsonl`。断言：

```rust
assert_eq!(entry.kind, EntryKind::Custom);
assert_eq!(entry.payload["extensionId"], "audit");
assert!(!session_root.join("index.jsonl").exists());
```

- [ ] **Step 2：运行 RED**

运行：`rtk cargo test -p store --test extensions`

预期：缺少 typed append API 或 payload 断言失败。

- [ ] **Step 3：实现 typed Store 边界**

JSON 形状只在 Store 的 Entry 转换边界形成。Extension crate 不定义 Store Entry 副本。Fork 和 replay 继续使用现有 parent/sequence 算法。

- [ ] **Step 4：运行 Store 全部测试**

运行：`rtk cargo test -p store --all-targets`

预期：通过。

## Task 7：把 Extension Runtime 移入 Session 并接入 Session 生命周期

**文件：**

- 修改：`crates/kernel/src/runtime.rs`
- 创建：`crates/kernel/src/runtime/extension.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/compaction.rs`
- 修改：`crates/kernel/src/runtime/settlement.rs`
- 修改：`crates/kernel/src/prompt.rs`
- 修改：`crates/skill/src/lib.rs`
- 修改：`crates/skill/tests/discovery.rs`
- 修改：`crates/kernel/tests/session_capabilities.rs`
- 修改：`crates/kernel/tests/compaction.rs`
- 创建：`crates/kernel/tests/extensions_session.rs`

**接口：**

- `Kernel` 保存 `Arc<dyn ExtensionFactory>` 和构建期静态声明，不保存全局 Pipeline。
- `SessionRuntime` 保存 `ExtensionRuntime`、动态 Tool/Command Registry 和 Active Tools。
- `SessionRuntime` 保存 `resources_discover` 累积出的 Skill/Prompt 路径，以及基于这些路径生成的不可变 `SkillCatalog` 和 `PromptTemplateCatalog`。
- `SessionExtensionHost` 实现 `ExtensionHost`，所有读取先建立快照再进入 `await`。

- [ ] **Step 1：写 Session 隔离 RED 测试**

创建两个 Session，Factory 为每个 Session 生成带独立计数器的 Module。触发 start/rename/fork/tree/shutdown，断言事件载荷、顺序和计数不串线；before cancel 时 Store 不变化。

- [ ] **Step 2：运行 RED**

运行：`rtk cargo test -p kernel --test extensions_session`

预期：旧全局 Pipeline 导致 API 缺失或隔离断言失败。

- [ ] **Step 3：调整 KernelFactory 构建顺序**

先读取 Extension 静态声明，再创建 Model Catalog 和 Built-in Tool 基线；创建/恢复 Session 时调用 `create_modules()`、Registrar freeze 和 `ExtensionRuntime::new()`。`project_trust` 明确允许后执行 `resources_discover`，将累积路径规范化为绝对路径，再通过 `SkillFactory::create_with_roots` 和 `PromptTemplateCatalog::discover` 建立该 Session 的资源快照；路径读取全部发生在服务器端。

- [ ] **Step 4：接入 Session 与 Compaction Point**

确保所有 before 事件在 Store 变更前；shutdown 在 Runtime invalidate 前完成；自定义 Compaction/Tree Summary 经过现有 `CompactionResult` 校验后才持久化。

- [ ] **Step 5：运行 Session 与 Compaction 测试**

运行：

```bash
rtk cargo test -p kernel --test extensions_session
rtk cargo test -p kernel --test session_capabilities
rtk cargo test -p kernel --test compaction
```

预期：通过。

## Task 8：接入 Input、Agent、Turn、Message 和 Context 链

**文件：**

- 修改：`crates/kernel/src/runtime/run.rs`
- 修改：`crates/kernel/src/runtime/attempt.rs`
- 修改：`crates/kernel/src/runtime/settlement.rs`
- 修改：`crates/kernel/src/provider.rs`
- 修改：`crates/kernel/src/prompt.rs`
- 修改：`crates/kernel/tests/turn.rs`
- 创建：`crates/kernel/tests/extensions_agent.rs`

**接口：**

- `input` 在 Skill/Prompt 展开前运行；Transform 后的文本先解析唯一匹配的 Prompt Template，再处理显式 Skill 调用。
- `before_agent_start` 返回 System Prompt 替换和 Message Draft 累积。
- `context` 只修改本次 ModelRequest。
- `message_end` 在持久化前替换最终消息且保持 role/identity。
- `ProviderMessage::try_from_agent` 根据 Extension Message 的 `include_in_context` 做明确投影。

- [ ] **Step 1：写 Agent 链 RED 测试**

覆盖 Input transform/handled、Extension Prompt 路径模板展开、Extension Skill 路径显式调用、System Prompt A→B、Context 过滤不修改 Store、MessageEnd 替换持久化、Extension Message 自动分配 ID/TurnId/三段时间。

- [ ] **Step 2：运行 RED**

运行：`rtk cargo test -p kernel --test extensions_agent`

预期：旧无载荷 dispatch 无法产生预期转换。

- [ ] **Step 3：按生命周期位置替换旧 dispatch**

删除 `Kernel::dispatch(ExtensionEvent, ...)`。`run.rs` 只调用有语义的方法，并把组合结果显式用于 Input、Prompt Template、Skill、System Prompt、ModelRequest、Assistant 和 ToolResult。Prompt Template 采用 Pi 的 Markdown 模板语义，名称来自文件名或 frontmatter，参数替换只在本次输入副本上发生。

- [ ] **Step 4：实现消息草稿落地**

`ExtensionMessageDraft` 转完整消息的逻辑实现为 Kernel 领域类型方法，不写单参数 free helper。所有非流式 Extension 消息使用同一 TimestampMs 填充三段时间。

- [ ] **Step 5：运行 Agent 测试**

运行：

```bash
rtk cargo test -p kernel --test extensions_agent
rtk cargo test -p kernel --test turn
```

预期：通过。

## Task 9：接入 Tool 预检、转换、并行事件和动态快照

**文件：**

- 修改：`crates/kernel/src/runtime/tool_batch.rs`
- 修改：`crates/kernel/src/runtime/run.rs`
- 修改：`crates/kernel/tests/turn.rs`
- 创建：`crates/kernel/tests/extensions_tools.rs`

**接口：**

- Turn 开始时捕获 `Arc<ToolRegistry>`，System Prompt、ModelRequest、预检和执行共用该版本。
- `tool_execution_start` 先按源码顺序发出。
- `tool_call` 顺序转换参数并调用快照 Registry 的 `validate`。
- Block、Handler Error、Invalid Arguments 形成错误 ToolResult，不调用 `execute`。
- `tool_result` 转换发生在 end 事件和持久化前。

- [ ] **Step 1：写 fail-safe RED 测试**

使用带原子执行计数的真实 Test Tool，分别让 Handler 主动 Block、返回非法参数和抛错；三种情况都断言执行计数为 0，且最终 ToolResult 为错误。

- [ ] **Step 2：写并行顺序 RED 测试**

两个 Tool 使用 barrier 证明执行并行；断言 start/tool_call 为源码顺序，update/end 可交错，最终消息仍为源码顺序。

- [ ] **Step 3：运行 RED**

运行：`rtk cargo test -p kernel --test extensions_tools`

预期：旧 ToolBatch 未应用 Extension 结果，断言失败。

- [ ] **Step 4：实现预检与结果链**

把预检状态建模为 `PreparedToolCall::Ready/Blocked`，不使用布尔值与可选错误组合非法状态。Tool execution 只接受 Ready。

- [ ] **Step 5：运行 Tool/Kernel 测试**

运行：

```bash
rtk cargo test -p kernel --test extensions_tools
rtk cargo test -p tools --all-targets
```

预期：通过。

## Task 10：把 Provider Hook 接到真实请求边界

**文件：**

- 创建：`crates/provider/src/completion/hooks.rs`
- 修改：`crates/provider/src/completion/mod.rs`
- 修改：`crates/provider/src/completion/request.rs`
- 修改：`crates/provider/src/factory/mod.rs`
- 修改：`crates/provider/src/http_client/mod.rs`
- 修改：`crates/provider/src/client/mod.rs`
- 修改：`crates/provider/src/providers/chatgpt/mod.rs`
- 修改：`crates/provider/src/providers/openai/completion/mod.rs`
- 修改：`crates/provider/src/providers/openai/responses_api/mod.rs`
- 修改：`crates/provider/src/providers/openai/responses_api/websocket.rs`
- 修改：`crates/kernel/src/model.rs`
- 修改：`crates/kernel/src/provider.rs`
- 创建：`crates/provider/tests/request_hooks.rs`
- 修改：`crates/kernel/tests/provider_runtime.rs`
- 创建：`crates/kernel/tests/extensions_provider.rs`

**接口：**

```rust
#[async_trait]
pub trait CompletionRequestHooks: Send + Sync {
    async fn before_payload(
        &self,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, CompletionError>;
    async fn before_headers(
        &self,
        headers: ProviderHeaders,
    ) -> Result<ProviderHeaders, CompletionError>;
    async fn after_response(
        &self,
        response: ProviderResponseMetadata,
    ) -> Result<(), CompletionError>;
}
```

`CompletionRequest` 的 Hook 字段使用 `Option<Arc<dyn CompletionRequestHooks>>`、`#[serde(skip)]`、`#[builder(default)]` 和手写 Debug，不进入 Provider JSON，也不要求反序列化构造默认 trait object。每个具体 Provider 在原生请求序列化后应用 payload Hook，在网络发送前应用 Header Hook，在消费响应体/流前应用 response Hook。

- [ ] **Step 1：写 Provider 边界 RED 测试**

使用 Wiremock 验证 payload Hook 修改最终 HTTP JSON、Header Hook 增删最终 Header、response Hook 收到真实 status/Header，且认证 Header 不进入事件数据。额外验证一次逻辑 Provider 请求发生传输重试时，payload/header Hook 只执行一次并复用准备后的不可变请求；response Hook 只对最终取得的响应执行一次。

- [ ] **Step 2：运行 RED**

运行：`rtk cargo test -p provider --test request_hooks`

预期：缺少 Hook 接口和请求透传。

- [ ] **Step 3：实现中立 Hook 透传**

Provider crate 只认识 Completion Hook，不依赖 extension。Hook 在 `client/mod.rs` 与 `http_client/mod.rs` 的共享请求准备阶段执行，因此 Anthropic、DeepSeek、Moonshot、MiniMax 和 XiaomiMimo 等 OpenAI-compatible 路径自动继承；ChatGPT、OpenAI Chat Completions、OpenAI Responses HTTP 与 Responses WebSocket 的直接发送点分别显式接入。用 `rg` 检查所有 `send_streaming`、SSE、`client.send` 和 WebSocket 握手发送点，确保没有绕过 Hook 的 Completion 路径。

- [ ] **Step 4：实现 Kernel Bridge**

`Model::stream` 增加请求级 Hook 参数。Kernel Bridge 把 Provider 类型转换为 Extension Point，普通 Handler 错误记录后保留当前值；Hook 自身的框架错误转换为 ModelError。

- [ ] **Step 5：运行 Provider 与 Kernel 测试**

运行：

```bash
rtk cargo test -p provider --test request_hooks
rtk cargo test -p kernel --test extensions_provider
rtk cargo test -p kernel --test provider_runtime
```

预期：通过。

## Task 11：实现 Session Model Catalog、Thinking 和静态 Provider 声明

**文件：**

- 修改：`crates/kernel/src/model.rs`
- 修改：`crates/kernel/src/provider.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/config/src/config.rs`
- 修改：`crates/provider/src/factory/mod.rs`
- 修改：`crates/app/src/lib.rs`
- 创建：`crates/kernel/tests/extensions_model.rs`
- 修改：`crates/config/tests/loading.rs`

**接口：**

```rust
pub trait ModelFactory: Send + Sync {
    fn create_catalog(
        &self,
        providers: &[ExtensionProviderRegistration],
    ) -> Result<ModelCatalog, ModelError>;
}
```

`ModelCatalog` 按 `provider_id/model_id` 保存已配置、已认证校验的 Model handle。Session 保存 Active Model key 和 Thinking Level；Turn 开始时捕获不可变快照。

- [ ] **Step 1：写 Model RED 测试**

测试两个 Session 选择不同 Model；Turn 中切换只影响下一 Turn；Thinking 被目标 Model 能力裁剪；事件顺序为 Thinking 变化后 ModelSelect；未知 Model 不改变 Session。

- [ ] **Step 2：写静态 Provider RED 测试**

Extension 静态声明在 LlmFactory 初始化前叠加到不可变 Config snapshot；重复 Provider、无效 Model 和认证错误阻止 App 构建；运行期 Context 不存在 register/unregister Provider 方法。

- [ ] **Step 3：运行 RED**

运行：`rtk cargo test -p kernel --test extensions_model`

预期：当前 Kernel 只有单一全局 Model，断言失败。

- [ ] **Step 4：实现 Catalog 与 Session 快照**

Provider/Config 只增加构建期 overlay 接口，不引入可变配置句柄或热更新。Model 选择记录 Pi v4 `ModelChange`，Thinking 记录 `ThinkingLevelChange`。

- [ ] **Step 5：运行 Model、Config 和 App 检查**

运行：

```bash
rtk cargo test -p kernel --test extensions_model
rtk cargo test -p config --all-targets
rtk cargo check -p app --all-targets
```

预期：通过。

## Task 12：实现 Host Actions、Extension Commands 和 User Bash

**文件：**

- 修改：`crates/kernel/src/runtime/extension.rs`
- 修改：`crates/kernel/src/runtime/queue.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/compaction.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/acp/src/extension.rs`
- 创建：`crates/kernel/tests/extensions_host.rs`
- 修改：`crates/acp/tests/extension.rs`

**接口：**

- Message/Entry/Queue/Session/Model/Tool/Exec Action 全部通过 `ExtensionHost` 实现。
- Command 使用限定名 `extension_id/name`；短名仅在唯一时解析。
- User Bash 使用服务器本地 Bash operation；不请求 ACP Client Terminal。

- [ ] **Step 1：写 Host Action RED 测试**

覆盖 send Extension Message、steer/follow-up User Message、append Entry、set Session name/label、abort、compact、set Active Tools、exec cancellation 和 stale Context。

- [ ] **Step 2：写 Command RED 测试**

两个 Extension 注册同短名 Command，断言限定名可调用、短名返回歧义错误；Session replacement 操作只允许 Command Context。

- [ ] **Step 3：运行 RED**

运行：

```bash
rtk cargo test -p kernel --test extensions_host
rtk cargo test -p acp --test extension
```

预期：Host Action 或限定 Command 解析缺失。

- [ ] **Step 4：实现 Host 和 ACP Command 分发**

所有 Action 在进入 `await` 前释放 Session/Store/Registry 锁。Exec 复用 Tools 的服务端进程语义和取消令牌，不增加 ACP terminal 方法。

- [ ] **Step 5：运行 Host/ACP 测试**

运行同 Step 3，预期全部通过。

## Task 13：完成 ACP 2.0 Extension 映射和批量回放

**文件：**

- 修改：`crates/protocol/src/acp.rs`
- 修改：`crates/acp/src/mapping.rs`
- 修改：`crates/acp/src/server.rs`
- 修改：`crates/acp/tests/mapping.rs`
- 修改：`crates/acp/tests/extension.rs`
- 修改：`crates/app/tests/web.rs`

**接口：**

- ACP 有原生消息时使用原生更新。
- Extension Message、Entry 可见投影、Handler Error 和来源诊断使用产品命名空间 Extension Update。
- 每个更新 metadata 包含 TurnId、字符串 TimestampMs 和 Sequence。
- 回放使用 ACP 2.0 批量消息方法，按 Store sequence 排序，只发送最终持久化状态。

- [ ] **Step 1：写 ACP RED 测试**

构造包含标准消息、Extension Message、Extension Entry 和 Handler Error 的 Session，断言批量回放顺序、命名空间、TurnId 和 `u64::MAX` 时间字符串不丢精度。

- [ ] **Step 2：运行 RED**

运行：

```bash
rtk cargo test -p acp --test mapping
rtk cargo test -p acp --test extension
```

预期：缺少新 Extension 投影或批量回放断言失败。

- [ ] **Step 3：实现 Mapping 和回放**

复用 `ProductIdentity::ACP_NAMESPACE`，不硬编码项目名称。回放不模拟 delta；只发送已持久化最终消息和诊断。

- [ ] **Step 4：运行 ACP/App 测试**

运行：

```bash
rtk cargo test -p acp --all-targets
rtk cargo test -p app --all-targets
```

预期：通过。

## Task 14：完整审查、验证和真实 WebUI E2E

**文件：**

- 修改：`README.md`
- 修改：`README_zh.md`
- 修改：`THIRD_PARTY_NOTICES.md`，仅当新增依赖时
- 修改：`web/src/` 中需要展示 Extension Message/Error 的现有领域组件

- [ ] **Step 1：静态结构审查**

运行：

```bash
rtk rg -n 'ExtensionEvent|ExtensionDirective|ExtensionEffects' crates --glob '*.rs'
rtk rg -n '#\[cfg\(test\)\]' crates --glob 'src/**/*.rs'
rtk find crates -path '*/src/*.rs' -type f -size +100k
```

预期：旧统一类型无生产引用；正文没有测试支撑；超过 1k 行的非 Provider Rust 正式文件均完成语义拆分判断和处理。

- [ ] **Step 2：运行完整 Rust 门禁**

运行：

```bash
rtk cargo fmt --all -- --check
rtk cargo check --workspace --all-targets
rtk cargo clippy --workspace --tests --examples -- -Dwarnings
rtk cargo test --workspace --all-targets
```

预期：全部成功且无 warning。

- [ ] **Step 3：运行 WebUI 门禁**

运行：

```bash
cd web
rtk npm run check
rtk npm run build
```

预期：类型检查、ESLint 和生产构建成功。

- [ ] **Step 4：启动正式后端**

从仓库根目录运行 `rtk cargo run -p app`，使用已经修复的 `claw.toml` 和真实 Provider。不得启动或恢复 fixture backend。

- [ ] **Step 5：执行真实浏览器 E2E**

使用 in-app Browser 打开正式 WebUI，验证：

1. 创建真实 Session 并发送消息。
2. Assistant text、thinking 和 Tool events 顺序正确。
3. Extension Message 与 Handler Error 使用通用非 UI 卡片显示。
4. 刷新后批量回放顺序、TurnId 和时间保持一致。
5. 动态 Tool/Command 在下一 Turn 生效。
6. 删除 Session 后后端和侧边栏均不再显示该 Session。

- [ ] **Step 6：运行 pre-commit hooks**

运行：`rtk pre-commit run --all-files`

预期：Rustfmt 和 Clippy hooks 全部通过。不要创建 commit。
