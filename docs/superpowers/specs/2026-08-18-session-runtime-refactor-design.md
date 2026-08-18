# Session 运行时状态拆分设计

## 1. 背景

当前 `crates/kernel/src/runtime.rs` 中的 `SessionRuntime` 同时保存持久化 transcript、Prompt/Skill 资源、模型选择、扩展运行时、MCP 生命周期、队列、Run 协调和事件投影状态，共 26 个字段。多个子模块直接读取这些字段，使状态边界、锁顺序和构造依赖难以辨认。

本次重构将 `SessionRuntime` 重命名为 `Session`，并将其改造成 Session 级门面。重构以收敛共享不变量为目标，不通过简单嵌套隐藏字段数量。

## 2. 目标

1. 将顶层类型 `SessionRuntime` 重命名为 `Session`。
2. 将顶层字段缩减到约 8 个职责清晰的组件。
3. 将必须一起变化的状态放入同一锁或同一枚举，避免半初始化和撕裂快照。
4. 通过组件方法访问状态，不在调用方继续展开内部锁和容器。
5. 保持 Session 创建、恢复、分叉、关闭、Run、Slash Command、队列、扩展、MCP 和事件顺序的现有外部行为。

## 3. 非目标

1. 不修改 Kernel 的公开协议和 ACP 接口。
2. 不改变持久化 JSONL 格式、lane 语义或恢复规则。
3. 不改变扩展 Hook 顺序、MCP 能力或 Tool 覆盖顺序。
4. 不引入新的 crate 或第三方依赖。
5. 不把所有 Session 状态放入一个全局大锁。

## 4. 方案选择

### 4.1 采用：领域组件加行为方法

`Session` 保留 Session 级编排所需的组件引用，各组件拥有自己的锁、不变量和快照方法。调用方表达领域操作，例如追加消息、安装活跃 Run、读取模型快照，而不是直接锁字段。

该方案能同时降低字段数量、减少跨模块耦合，并修复多个锁表达单一状态的问题。

### 4.2 不采用：仅将字段机械包装成嵌套结构

机械包装会把 `session.active_run_id.lock()` 变成 `session.execution.active_run_id.lock()`，不会改善所有权和不变量，反而增加调用链长度。

### 4.3 不采用：单个 `Mutex<SessionInner>`

单个大锁会让 Store I/O、快照读取、事件发送和运行协调互相阻塞，并扩大异步路径中的死锁风险。

## 5. 顶层结构

目标结构如下：

```rust
#[derive(typed_builder::TypedBuilder)]
struct Session {
    cwd: PathBuf,
    transcript: Arc<SessionTranscript>,
    resources: OnceLock<SessionResources>,
    model: SessionModelState,
    tools: Arc<SessionToolState>,
    extensions: SessionExtensions,
    mcp: SessionMcpRuntime,
    execution: Arc<SessionExecution>,
}
```

所有超过 3 个字段的新增结构使用 `typed-builder`，并通过 builder 构造。所有新增函数提供英文函数级注释，非平凡逻辑提供英文原因说明。

## 6. 组件设计

### 6.1 SessionTranscript

`SessionTranscript` 负责持久化 Store、固定 lane 和当前分支的模型上下文 history。这里的 history 不是 Store 中所有 Message entry 的简单镜像：失败 Assistant 可以继续保存在 Store 中，却在后续模型上下文或 Compaction 中被过滤；Compaction 和 Tree navigation 还会整体替换 history。

`store` 与 `history` 放入同一个 `Mutex<SessionTranscriptState>`。所有同时改变 Store 和 history 的操作必须通过一个组件方法持有一次锁完成，调用方不得先调用 Store 方法、再单独修改 history。

它提供以下行为：

- `append_context_message`：追加 Message entry，并在 Store 操作成功后更新 history。
- `append_store_only_entry`：追加不直接改变模型上下文的 typed entry。
- `append_record`：追加只参与恢复或审计的 record，不改变 history。
- `commit_compaction`：在同一临界区追加 Compaction entry，并用 summary 与 retained tail 替换 history。
- `commit_navigation`：在同一临界区移动 lane、可选追加 branch summary，并从新分支重建 history。
- 读取 Session ID、路径、标题和 Tree 所需数据时返回 owned snapshot，不向调用方暴露锁 guard 或 Store 引用。
- 返回 history 防御性快照。
- 同步 Store。

上述方法保持现有 entry kind、payload、父子关系和写入顺序。导航失败采用分阶段补偿：branch summary 尚未追加时恢复旧 lane 与 label；summary 已追加后因 label、history 重建或 `sync` 失败时，不伪造无法实现的删除，而是保留 summary 所在分支为活动分支，并按 Store 当前 lane 对齐 history。若所有逻辑变更均已完成，只有首次 `sync` 失败，且 history 对齐和重试 `sync` 都成功，则该导航视为已提交并返回成功，使调用方继续发布 Tree 事件；其他后 summary 失败仍返回原始错误。这样既符合 append-only Store 的能力，也不会在 Tree 中留下不可达的孤儿 summary 或漏发已提交导航的事件。

`KernelExtensionDiagnostics` 改为持有 `Arc<SessionTranscript>`，不再分别持有 `store`、`history` 和 `lane`。

### 6.2 SessionResources

`SessionResources` 同时保存不可变 `PromptSession` 和可选 `SkillCatalog`。`Session` 只保留一个 `OnceLock<SessionResources>`，资源发现全部成功后一次安装，避免只初始化 Skills 或 Prompt 的状态。

资源仍在 `project_trust`、`session_start` 和 `resources_discover` Hook 完成后创建，不改变现有启动顺序。

### 6.3 SessionModelState

`SessionModelState` 保存只读 `ModelCatalog` 和一个 `RwLock<ModelSelection>`。`ModelSelection` 同时保存当前 Model handle 与 Thinking Level，使扩展快照一次取得一致状态。

模型和 Thinking Level 的持久化顺序保持不变：先写入 Store，再发布新的内存选择，最后发送扩展事件。更新方法只原地修改目标字段，不使用调用前克隆的旧 `ModelSelection` 覆盖整个结构，避免并发的 Model 与 Thinking 更新互相丢失。

### 6.4 SessionExtensions

`SessionExtensions` 保存 `Arc<ExtensionRuntime>`、动态 Command Registry、Flag 快照和 Session 事件总线。组件提供解析/注册 Command、读取 Flags、发布/订阅事件等行为。Provider Hook 等需要延长生命周期的调用方继续通过 `Arc::clone` 获取 Extension Runtime，不借用 `SessionExtensions` 跨越 `.await`。

高频 Extension Hook 调用通过明确的 runtime 访问方法完成，不为每个 Hook 创建无意义的转发函数。

### 6.5 SessionMcpRuntime

MCP 使用枚举表示合法状态：

```rust
enum SessionMcpRuntime {
    Disabled,
    Enabled(SessionMcpState),
}
```

`SessionMcpState` 保存 `McpSession`、projection task 和 pending elicitations。Disabled 状态不分配这些容器，也无法出现没有 MCP Session 却存在 projection 的组合。

MCP Host 继续通过 `Weak<Session>` 回调，避免所有权环。

Disabled 状态保持现有公开行为：

- `mcp_status` 返回默认 `McpSessionSnapshot`。
- `subscribe_mcp` 返回 `None`。
- `mcp_elicitations` 返回携带当前 SessionId 的空快照。
- reconnect、authorization continuation、Prompt、Resource 和 completion 请求返回与当前实现一致的 `McpError::ServerNotFound`。
- shutdown 和 projection stop 为成功的 no-op。

Enabled 状态的正常关闭顺序固定为：先取消当前 Session operation，使 Turn-scoped elicitation 结束；再等待 operation gate；执行 Extension shutdown Hook；调用 `McpSession::shutdown()`；等待 projection task；同步 transcript；最后 invalidate Extension Runtime 并移除 Session。

启动失败回滚保持当前差异：先 invalidate Extension Runtime，再调用 `McpSession::shutdown()`，随后等待 projection task；仅新建或分叉失败时删除持久化 Session。Resume 的 `session_before_switch` 在注册前取消时也必须执行相同的内存资源关闭，但不得删除已有持久化 Session。任何路径都不得只丢弃 projection `JoinHandle` 而不等待任务结束。

### 6.6 SessionExecution

`SessionExecution` 保存生命周期、串行 operation gate、当前 Run、idle 通知、取消 Token、待处理队列和事件序号。

当前 Run 使用单锁状态：

```rust
struct ActiveRunContext {
    run_id: RunId,
    turn_id: TurnId,
    sink: Arc<dyn EventSink>,
}
```

`ActiveRunLease` 原子安装和清除整个 `ActiveRunContext`。取消 Token 保留独立状态，因为普通 Agent Run 和直接 Slash Command 都使用取消能力，但只有普通 Agent Run 对外暴露 active Run 关联。

`SessionExecution` 不持有 `Session` 或扩展运行时。诊断器可以安全持有 `Arc<SessionExecution>`，不会形成引用环。

运行协调必须保留以下协议：

1. 新构建的 runtime 以 `Starting` 状态注册，供启动 Hook 构造 Session 上下文；所有按 SessionId 进入的普通公开操作在资源安装和启动 checkpoint 完成前返回 `SessionStarting`。
2. 启动 checkpoint 成功后，runtime 才从 `Starting` 原子切换到 `Active`；任一启动步骤失败时直接回滚注册，不允许经历可公开操作的 `Active` 窗口。
3. `acquire_operation` 在等待 gate 前检查一次 lifecycle，获取 gate 后再次检查；第二次检查失败时立即释放 guard 并返回 `SessionClosing`。
4. `try_acquire_operation` 同样在尝试获取 gate 前后检查 lifecycle；gate 已占用时返回 busy，不进入等待队列。
5. Close 先取消当前 Token，再等待 gate；持有 gate 后立即将 lifecycle 从 Active 改为 Closing，然后才调用可能重入 Host API 的异步 shutdown Hook。
6. 普通 Agent Run 和直接 Slash Command 在开始时都安装新的 CancellationToken；`cancel_session` 只取消当前快照，下一次 operation 不继承已取消 Token。
7. `ActiveRunLease` 只在当前 `run_id` 仍与 lease 一致时清除上下文。清除在一个锁内完成；释放锁后才调用 `idle_notify.notify_waiters()`。
8. `wait_for_idle` 必须先注册 `notified()` future，再检查 active Run，避免检查与等待之间丢失通知。这里的 idle 延续现有定义：只表示没有普通 Agent Run，不把直接 Slash Command 计入 active Run。
9. Extension Slash Command 只用 non-blocking gate acquisition 检查 busy；调用 Extension handler 前必须释放 gate，使 handler 可以安全调用 `send_user_message`、`compact` 等 run-gated Host API。
10. live Resume 的 `session_before_switch` Hook 不持有 operation gate，以允许 Extension Host 重入；Hook 返回后必须获取 gate 并再次检查 lifecycle。若并发 Close 已完成，Resume 返回 `SessionClosing`，不得同步或成功返回已移除 runtime 的路径。
11. Create、未注册 Resume、Fork 和 Delete 对同一 SessionId 使用同一个互斥预留集合。Delete 必须在 Close 前取得预留并持有到持久化 Store 删除完成，避免 runtime 缺席窗口中与构造、注册或 Store 打开竞争。
12. Active Run 的 `run_id`、诊断 `turn_id` 和 fallback event sink 必须通过同一个 `ActiveRunContext` 快照读取，不允许分别读取后拼接。
13. Session event sequence 保持单调递增，普通事件、MCP Host 事件和 Extension diagnostics 共享同一个原子计数器。

### 6.7 Tool 执行模块

Tool 的 Session 状态、批处理执行和增量更新继续属于同一领域，但按职责放入 `crates/kernel/src/runtime/tools/`：

- `state.rs`：`SessionToolState` 与不可变快照。
- `batch.rs`：`ToolBatch`、`ToolBatchResult` 和批处理执行。
- `update.rs`：批次内 latest-value 更新通道。

`tools/mod.rs` 只向 `runtime` 重导出编排所需类型。由于模块名与外部 `tools` crate 同名，runtime 子树对外部 crate 使用 `::tools::...` 绝对路径，避免名称解析随模块层级变化。

持久化的 `activeTools` 表示用户选择偏好，不是恢复时对当前 Tool 目录的强约束。动态 Extension/MCP Tool 暂时不可用时，Session 仍可恢复；该名称不进入当前 `active_registry`，但也不写入 disabled preference，待 Tool 重连后自动恢复激活。Host 修改选择时必须使用同一个不可变 Tool 快照完成验证和偏好应用：验证后目录中消失的 Tool 保留选择，期间新出现且不在原快照内的 Tool 保留既有偏好，不能被这次选择误禁用。

### 6.8 Queue 跨组件持久化协议

Queue 的内存容器属于 `SessionExecution`，Queue records 属于 `SessionTranscript`。跨组件操作由 `Session` 门面编排；任何步骤都不得同时持有 queue 锁和 transcript 锁。

固定顺序如下：

- 入队：先从 `ActiveRunContext` 取得 RunId；完成输入展开；若展开产生诊断，则重新读取同一 Run 的当前 `ActiveRunContext`，使用最新 TurnId 与对应 sink 发布诊断；随后追加 `QueueEnqueued` record、同步 Store，最后将 item 插入内存 queue。该顺序允许入队与活跃 Run 的 Turn 推进及最终 checkpoint 竞争，并保证诊断关联正确且返回成功的消息可在 Resume 后恢复。
- 显式删除：先取得待删除 ID 的 owned snapshot；追加 `QueueCancelled(Removed)` record；同步 Store；最后从内存 queue 删除。
- 清空：先取得所有 ID 的 owned snapshot；逐个追加 `QueueCancelled(Cleared)` record；同步 Store；最后从内存 queue 删除这些 ID。
- Run 消费：先把预留 EntryId 对应的消息写入 transcript，再追加 `QueueCancelled(Consumed)` record，之后才从内存 queue 删除；由现有 Run settlement checkpoint 负责最终同步。
- 读取下一项和公开 pending snapshot 只短暂持有 queue 锁并返回 owned value。

不把 queue 与 transcript 合并进同一个锁，因为外部入队允许与持有 operation gate 的 Run 并发；durable-first 记录是两者之间的恢复协议。

## 7. 构造与启动顺序

`build_session_runtime` 重命名为 `build_session`，按以下顺序构建：

1. 创建 Extension Registry、MCP Session 和 Tool partitions。
2. 从 Store 恢复模型、Thinking、Tools、history、queue 和初始事件序号。
3. 创建 `SessionTranscript`、`SessionExecution`、`SessionModelState` 和扩展诊断器。
4. 创建 `SessionExtensions` 与顶层 `Session`。
5. 将完整 `Session` 以 Weak 引用附加到 MCP Host。
6. 启动 MCP projection。
7. 以 `Starting` 生命周期注册 Session，执行 Extension 启动和资源发现。
8. 一次安装 `SessionResources`，完成 Store checkpoint 后切换为 `Active`。

创建、恢复和分叉继续共享同一构造路径。

MCP Session 创建后仍有 settings 恢复、Command Registry、Tool state、Extension Runtime 和 projection 启动等 fallible 步骤。`build_session` 必须把这些步骤放入统一的异步错误出口：任何后续步骤失败时，先 invalidate 已创建的 Extension Runtime，再调用 `McpSession::shutdown()`；若 projection 已启动则继续等待其 task，最后才返回原始构造错误。不能依赖尚未构造完成的 `Session` 或 `Drop` 执行异步清理。

## 8. 错误处理与并发约束

1. 所有 std 锁 poisoning 继续映射为 `KernelError::Poisoned` 或对应边界错误。
2. `ActiveRunContext` 安装失败时不允许留下部分 Run 状态。
3. transcript 修改在单一锁内完成；若 Store 操作失败，不更新派生 history。
4. 不持有 std 锁跨越 `.await`。
5. Extension 和 MCP 回调在 await 前获取独立快照或 Arc handle。
6. `Arc<T>` 字段复制统一使用 `Arc::clone(&value)`。
7. 跨组件调用前必须释放当前组件的锁；组件公开方法返回 owned snapshot，不返回可跨组件传播的内部 guard。
8. Queue 与 transcript 的交互严格遵循第 6.8 节，不通过扩大锁范围伪造跨组件事务。
9. Active Run 清除与 idle 通知严格遵循“先清除并释放锁，后通知”的顺序。

## 9. 测试策略

本次为 Rust 后端重构，遵循 TDD：

1. 先增加组件级测试，定义资源原子安装、Run 上下文原子安装/释放、模型一致快照和 MCP 状态行为。
2. 每个测试先在目标组件不存在或行为缺失时产生预期 RED，再实现最小代码达到 GREEN。
3. 复用 Kernel 集成测试验证 Session 创建、恢复、分叉、关闭、Run、队列、Extension、MCP、Prompt 和 Tree 行为。
4. 最终运行格式化、Kernel 全量测试、workspace check 和 Clippy。

必须显式覆盖以下高风险回归：

- operation 等待 gate 期间进入 Closing 后，获取 gate 的 operation 仍被拒绝。
- 已注册但仍在启动 Hook 中的 Session 拒绝 Close 等公开操作；释放 Hook 后创建继续完成。
- live Resume 在 `session_before_switch` 等待期间被 Close 后，必须在 Hook 返回后观察到 `SessionClosing`。
- 未注册 Resume 持有 SessionId 预留时，Delete 必须失败且不得删除其持久化 Store。
- shutdown Hook 重入 run-gated Host API 立即返回 `SessionClosing`，不发生死锁。
- Extension Slash Command 调用 run-gated Host API 不因保留 gate 而死锁。
- idle waiter 只在完整 `ActiveRunContext` 清除后返回，并能读取到一致的空状态。
- Queue 入队与 Run 最终 checkpoint 竞争后，关闭并 Resume 只恢复一次消息。
- Queue 的入队、删除、清空和消费保持现有 durable-first 顺序。
- Compaction 和 Tree navigation 后的内存 history 与关闭、Resume 后重建的 history 一致。
- branch summary 后首次 `sync` 失败但重试成功时，navigation 返回已提交结果并允许发布 Tree 事件。
- root navigation 首次 `sync` 失败时恢复原 lane leaf 与模型 history，并返回原始持久化错误。
- root navigation 成功事件必须携带清空前捕获的 `old_leaf_id`，不能从清空后的 Session 快照反推。
- 动态 Tool 暂时离线时 Resume 仍成功，重连后恢复用户选择；选择验证与发布之间目录变化不得产生持久化成功、内存发布失败的分裂状态。
- Queue 的 Skill 展开跨越 Turn 推进时，诊断必须关联展开完成后的同一 Run 当前 Turn，而不是入队开始时的旧 Turn。
- Store-only entry 和 record 不进入模型 history。
- MCP Disabled 的所有公开方法保持上述返回语义。
- MCP Session 创建后的任意构造失败、projection 启动失败、资源初始化失败、注册前 Resume 取消和正常关闭都按规定顺序关闭 MCP，并在已启动 projection 时等待 task 结束。

测试代码只放在 crate 级 `tests/` 或标准 `#[cfg(test)] mod tests` 中，不向生产代码加入测试专用入口。

## 10. 迁移顺序

1. 引入 `Session` 名称并完成全量类型引用迁移。
2. 提取 `SessionResources` 与 `SessionModelState`。
3. 提取 `SessionExecution`，原子化 Active Run 状态。
4. 提取 `SessionTranscript`，迁移 Store/history/lane 行为。
5. 提取 `SessionMcpRuntime` 与 `SessionExtensions`。
6. 简化 `build_session`，删除旧字段和直接锁访问。
7. 执行全量回归验证。

每一步保持可编译、可测试；不创建兼容类型别名，最终代码中不保留 `SessionRuntime` 名称。
