# ACP 5.0 升级、HTTP/2 与私有批量协议设计规格

日期：2026-11-02。状态：设计已获口头确认，待审阅本文。前置：[ACP 3.6 升级与 1.13 对齐设计](./2026-10-29-acp-1-13-alignment-design.md)、[WebUI 性能优化设计](./2026-08-22-webui-performance-optimization-design.md)（后者定义了 WebUI 按物理 frame 一次提交的消费方式，本文保持该行为）。

## 1. 目标与已确认决策

目标：把 `agent-client-protocol` 与 `agent-client-protocol-http` 从 3.6.0 升级到 5.0.0（schema 1.13.0 → 1.15.1），在 ACP v2 移除 JSON-RPC batch 后保留会话更新的批量提交能力，开启 HTTP/2，并用标准 `session/set_title` 替换私有重命名扩展。

| 决策 | 结论 |
|---|---|
| 升级时机 | 立即升级，版本约束写 `"5"`，后续 5.0.x 补丁直接跟进 |
| JSON-RPC batch | 服务端不再发送 JSON 数组（v2 规范禁止，SDK 5.0 也已移除）；批量改为 `_meta` 承载的私有协议（§4） |
| 批量协商 | 独立能力 `_meta.clawcode.sessionUpdateBatch: { "version": 1 }`，客户端在 `initialize` 请求中声明；服务端不在响应中回声明；未声明的客户端（Zed 等）只收到标准逐条通知 |
| HTTP/2 | 只开 h2c（axum `http2` feature），不内置 TLS；WebSocket 与 WebUI 继续使用 HTTP/1.1（§5） |
| set_title | 采用标准 `session/set_title`（`unstable_session_set_title`），删除 `_clawcode/session/rename`，并补上改名后缺失的标题事件（§6） |
| 载体元素格式 | 嵌套项使用完整的标准 `session/update` params，前端解码器直接复用 |
| 前端数组兼容 | 删除；收到数组按协议错误处理 |

## 2. 依赖与机械迁移

### 2.1 Cargo

根 `Cargo.toml`：

```toml
agent-client-protocol = "5"
agent-client-protocol-http = { version = "5", default-features = false }
axum = { version = "0.8", features = ["http2"] }
```

`crates/acp/Cargo.toml` 的 `agent-client-protocol` features 增加 `unstable_session_set_title`，其余保持 `stdio`、`unstable_end_turn_token_usage`、`unstable_protocol_v2`。更新 `Cargo.lock`。

### 2.2 API 改名

试编译（`cargo check -p acp --all-targets`）确认的变化：

| 3.6 | 5.0 | 位置 |
|---|---|---|
| `agent_client_protocol::schema::v2` | `agent_client_protocol::v2::schema` | `config.rs`、`content.rs`、`input.rs`、`mapping.rs`、`projection.rs`、`recovery.rs`、`terminal.rs`、`tool_call.rs`、`elicitation.rs`、`coalescing.rs`、`server.rs` 及测试 |
| `schema::ProtocolVersion` | 不变（版本无关项仍在 `schema::`） | `server.rs` |
| `V2ConnectionTo` | `v2::ConnectionTo` | `server.rs`、`extension.rs`、`tests/elicitation.rs`、`tests/v2_alignment.rs` |
| `Responder<T>` | `v2::Responder<T>`（`Responder<T, V2>`） | `server.rs` 全部请求处理器、`trace.rs`、`tests/elicitation.rs` |
| `JsonRpcMessage::parse_message(method, impl …)` | `parse_message(method, &serde_json::Value)` | `extension.rs` |
| `JsonRpcResponse::from_value` | `JsonRpcResponse::from_json` | `extension.rs` |
| `Channel { rx, tx }` 可穷尽解构 | `Channel` 改为 `#[non_exhaustive]`，解构需加 `..` | `batching.rs` |
| `TransportBatch`、`TransportBatchEntry`、`TransportFrame::Batch` | 删除；`TransportFrame` 只剩 `Single` 与 `Malformed` | `batching.rs`（§4.3 重写） |

`crates/app/src/logging/format.rs` 与 `crates/app/tests/logging.rs` 依赖 tracing target `agent_client_protocol::jsonrpc::incoming_actor`；5.0 中该模块文件仍在 `src/jsonrpc/incoming_actor.rs`，实施时以日志测试确认 target 未变。

## 3. 上游行为变化排查

| 上游变化 | 排查内容 | 期望 |
|---|---|---|
| #462 被 drop 的 Responder 自动回错误 | 检查 `server.rs`、`extension.rs`、`elicitation.rs` 中是否有分支在未调用 `respond` 时结束（如提前 `return`、`?` 之外的丢弃） | 每个请求都有显式响应；若存在故意不回复的路径，改为显式错误响应 |
| #485 未处理的 session 请求会被应答，parked 消息有上限 | 确认没有依赖"请求一直悬挂"的行为 | 无需改代码 |
| #482 未处理的 `tool_call/cancel`、`terminal/cancel` 默认回 `-32602` | 服务端不声明这两个能力，WebUI 也不发送这两个请求 | 无影响，无需改代码 |
| #487 v2 agent 侧取消权限请求 | 对照 `elicitation.rs` 的取消路径 | 既有 elicitation 测试通过 |
| #483 HTTP 未知 session 的 GET 返回 404 | 无代码依赖 | 无需改代码 |
| #486 重复 `initialize` 按首次协商 | 批量能力在每次 `initialize` 时重新判定（§4.1） | 与上游一致 |

## 4. 私有批量协议

### 4.1 协商

客户端在 `initialize` 请求的 `_meta.<产品命名空间>` 中声明：

```json
"_meta": { "clawcode": {
  "sessionRecovery": { "version": 1, "sessions": [] },
  "sessionUpdateBatch": { "version": 1 }
} }
```

服务端规则：

- 能力只对当前连接生效。`version` 等于 1 时启用；字段缺失、不是对象或版本不认识时一律禁用，不报错。
- 同一连接再次收到 `initialize` 时按新的请求重新判定。
- 服务端不在 `initialize` 响应中回声明；客户端无论是否收到载体都能正确工作。

### 4.2 线格式

载体本身是一条标准 `session/update` 通知。外层 params 原样承载第 1 条更新（含它自己的 `_meta`），第 2..N 条放在 `_meta.clawcode.sessionUpdateBatch` 数组里：

```json
{"jsonrpc":"2.0","method":"session/update","params":{
  "sessionId":"s1",
  "update":{ "sessionUpdate":"agent_message_chunk", "...": "第 1 条" },
  "_meta":{"clawcode":{
    "sessionRecovery":{ "...": "第 1 条自己的 group meta" },
    "sessionUpdateBatch":[
      {"sessionId":"s1","update":{ "...": "第 2 条" },"_meta":{"clawcode":{"sessionRecovery":{}}}},
      {"sessionId":"s1","update":{ "...": "第 3 条" }}
    ]
  }}
}}
```

约束：

- 数组非空，外层加数组总数不超过 `[app.recovery] max_batch_size`。
- 所有嵌套项的 `sessionId` 与外层相同。原因是 HTTP/SSE 传输按 `params.sessionId` 把通知路由到对应 session 的流，跨 session 打包会被送错流。
- 嵌套项内不得再含 `sessionUpdateBatch`，不允许多层嵌套。
- 应用顺序为外层在前，之后按数组顺序。
- 每个嵌套项保留自己的 `sessionRecovery` group meta，恢复语义（`recovery.ts` 的 projection group 收齐后原子提交）不变。

### 4.3 服务端：`batching.rs`

`BatchComponent` 仍是包在 Agent 组件外的传输层组件，改动如下：

- **入站**：帧原样转发，包括 `TransportFrame::Malformed`，由内部的 SDK 连接负责应答。遇到 `initialize` 请求时，按 §4.1 从 `_meta.<ProductIdentity::ACP_NAMESPACE>` 读取能力，写入本连接与出站共享的启用标记（`Arc<AtomicBool>`）。标记必须在把该帧转发给内部组件**之前**写入：内部组件只有收到 `initialize` 后才可能产生 `session/update`，这样出站一定能看到最新的标记。
- **出站取数**：保持"只取已在队列中的消息"（`now_or_never`），不引入定时器，不延迟首个 token。出现以下任一情况时停止取数：
  - 达到 `max_batch_size`；
  - 遇到非 `session/update` 帧；
  - 遇到 `sessionId` 不同的更新；
  - 源已关闭。
  
  触发停止的帧保留为下一轮的第一帧，不改变顺序。现有实现会跨 session 合包，这里收紧为只合同一 session。
- **合并**：先对取到的消息执行 `coalesce_updates`，对所有客户端都做（合并后仍是标准通知）。
- **输出**：
  - 未启用批量或只剩 1 条时，逐条输出 `TransportFrame::Single`。
  - 启用且多于 1 条时，按 §4.2 生成一条载体。
  - 第 1 条的 `_meta` 或 `_meta.clawcode` 缺失时创建空对象；已存在但不是对象时放弃打包，逐条输出。
- `max_batch_size` 含义与配置均不变（`claw.toml` 无需修改）。
- 已知代价：多个 Session 同时流式输出、更新交错到达时（A、B、A、B），每批只有 1 条，帧数比现在多。全量重放只涉及单个 Session，不受影响。如果实测发现多 Session 场景有性能问题，再考虑在单次取数窗口内按 Session 分组。

### 4.4 WebUI

- `web/src/acp/connection.ts`：`receive` 只接受单个 JSON 对象；收到数组时报 diagnostic 并丢弃该帧。
- `web/src/acp/protocol.ts`：新增载体展开函数，写法与现有 `sessionRecovery` 解码一致，按命名空间读取：
  - 没有批量键时原样返回；
  - 有批量键时，校验数组非空、元素都是记录、`sessionId` 与外层一致且不再嵌套批量键，然后从外层移除该键（其他产品键保留），展开为 `[外层, ...嵌套项]`；
  - 校验失败时抛错。
- `web/src/workspace/controller.ts`：
  - `applyNotifications` 对每条 `session/update` 调用展开函数，展开后的条目一并交给 `updateRouter.applyMany`，仍是每帧提交一次 store。展开在这里而不是 `connection.ts` 做，因为这里才拿得到产品命名空间。
  - 展开时抛出的错误走现有"应用通知失败"路径：断开连接后重连恢复。丢弃一帧会让投影状态悄悄错位，重连恢复不会。
  - `initialize`（`controller.ts:590`）的 `_meta` 中、`sessionRecovery` 旁边增加 `sessionUpdateBatch: { version: 1 }`。

## 5. HTTP/2

- 只改根 `Cargo.toml` 的 axum features（§2.1），`crates/app/src/main.rs` 的 `axum::serve` 不变。hyper-util 的 auto builder 会在同一端口同时接受 HTTP/1.1 与 h2c（prior knowledge）。
- 已知限制（不修复，写进文档）：
  - 服务端没有 TLS，浏览器不会使用 h2c，WebUI 仍是 HTTP/1.1。
  - h2 上的 WebSocket 走扩展 CONNECT，axum 会开启这项能力，但 SDK 在 `/acp` 上只注册了 `GET`，CONNECT 请求会得到 405。因此 WebSocket 只支持 HTTP/1.1。
  - SDK 的 Rust `HttpClient` 未开启 reqwest 的 http2 feature，它本身不会使用 h2。
- `README.md` 与 `README_zh.md` 的 HTTP 传输段落（`README.md:360`、`README_zh.md:321`）补一句：支持 h2c prior knowledge，WebSocket 仅支持 HTTP/1.1。

## 6. 标准 `session/set_title`

### 6.1 服务端

- 在 `server.rs:878` 的 `SessionCapabilities` 中声明 `title(SessionTitleCapabilities::new())`。
- 在 `server.rs` 中按现有请求处理器的写法注册 `SetSessionTitleRequest`：
  1. `SessionTitle::try_from(title)`，失败时返回 `-32602`（`Error::invalid_params`）；
  2. 调用 `kernel.set_session_title`（§6.2），持久化完成后才响应；
  3. 响应 `SetSessionTitleResponse::new(title.as_str())`，即规范化后的标题。
  
  会话不存在等 Kernel 错误沿用其他 session 请求的 `KernelFailure` 映射。
- 规范要求设置成功后不被自动标题覆盖，现状已满足：`set_default_name` 只在没有标题时写入（`transcript.rs:389`），无需改动。

### 6.2 Kernel：补发标题事件

现状：`rename_session` 只调用扩展钩子，不发 `SessionTitleChanged`，导致其他连接收不到 `session_info_update`；只有 `/name` 命令会在调用后用自己 turn 的 emitter 发事件（`builtin.rs:166-173`）。

`rename_session` 保持不变。它还用于子代理会话创建时设置标题（`ownership.rs:147`），`lifecycle_order.rs:1457` 也断言它的持久化顺序只有 `set_name`、`sync`；在它内部发事件会影响这两处。

新增 `Kernel::set_session_title(&SessionId, SessionTitle)`：先调用 `rename_session`，再发一条 `SessionTitleChanged`。发送方式沿用 `queue.rs:381-400` 的写法：

- 有活跃 run 时，使用它的 `turn_id` 与 sink；
- 没有活跃 run 时，新分配一个 `TurnId`，并使用 `kernel.session_sink`。

只有 ACP `session/set_title` 处理器调用这个方法。`/name` 命令保持不变，它已经在自己的 turn 里发了事件。

### 6.3 删除私有扩展

- `crates/protocol/src/identity.rs`：删除 `AcpExtensionMethod::SessionRename` 及其字符串映射和枚举列表项。
- `crates/protocol/src/acp.rs:104`：删除 `AcpSessionRenameParameters`，同时删除 `lib.rs:24` 的导出。
- `crates/acp/src/extension.rs:325`：删除 `SessionRename` 分支。
- `crates/acp/src/server.rs:832`：从声明的扩展方法列表中删除该项。
- `crates/protocol/tests/identity.rs`：删除对应断言。
- `web/src/acp/extensions.ts`：删除 `sessionRename`。
- `web/src/workspace/controller.ts:150`：`renameSession` 改为调用 `session/set_title`，并显示响应中的 `title`。不按能力做门控，因为服务端与 WebUI 一同发布。

## 7. 测试与验收

### 7.1 Rust 自动化测试

- **`batching.rs` 单元测试**（`#[cfg(test)] mod tests`）：
  - 未协商时逐条原样输出；
  - 协商后同一 session 的连续更新打成一条载体，顺序保持；
  - 达到 `max_batch_size` 时截断；
  - 遇到非 update 帧或换 session 时截断且不重排；
  - 只有 1 条时不加批量键；
  - 先合并后打包；
  - 嵌套项保留各自的 `sessionRecovery`；
  - `_meta.clawcode` 不是对象时放弃打包；
  - 重复 `initialize` 会重新判定能力；
  - `Malformed` 入站帧原样透传；
  - 组件级测试：通过 `BatchComponent` 的真实 channel，`initialize` 声明能力后收到载体，不声明时收到标准帧。在组件层测，排队时机可控，结果是确定的。
- **`crates/acp/tests` 集成测试**：
  - `session/set_title` 正常时返回规范化标题；
  - 空白标题返回 `-32602`；
  - 改名后，监听该 session 的连接收到 `session_info_update`。
- **Kernel 测试**：
  - `set_session_title` 恰好发出一条 `SessionTitleChanged`，有活跃 run 和没有活跃 run 两种情况都要覆盖；
  - `rename_session` 仍不发事件（现有 `lifecycle_order.rs` 断言不变）；
  - `set_session_title` 之后再跑一轮对话，标题不会被默认标题覆盖。
- **h2c**：`crates/app/tests` 增加一个测试：在 `127.0.0.1:0` 上用 `axum::serve` 启动应用 router，用 reqwest 的 `http2_prior_knowledge()` 请求 `/health`，断言响应版本为 HTTP/2。为此在 `crates/app` 的 dev-dependencies 中给 reqwest 开启 `http2` feature。
- **回归**：
  - 现有 `crates/acp/tests`（disconnect、elicitation、extension、mapping、tracing、v2_alignment）全部通过；
  - `crates/app/tests/logging.rs` 通过（确认 tracing target 未变）。

### 7.2 WebUI e2e

按项目规则，在生产 `app` 后端及其配置的 provider 上执行：

1. 普通对话的流式输出：首个 token 没有可感知的延迟，最终渲染完整。
2. 断线后重连：
   - 增量 cursor 恢复：进行中的 Session 继续流式输出；
   - 全量重放：用超过 512 条更新的大会话，结果与断线前一致，Inspector 中没有 diagnostic。
3. 多 Session 并行：后台 Session 的更新落到各自 Session。
4. 在 WebUI 中重命名：标题立即更新；同时打开的另一个标签页也收到新标题；刷新后标题保持。
5. 权限请求和 elicitation 的确认、取消路径正常。

### 7.3 性能对比

同一个大会话全量重放，对比升级前后的 WebSocket 帧数与 WebUI 完成渲染的耗时。帧数应与升级前同一数量级，耗时不明显变差。

## 8. 不在范围

- 内置 TLS，以及浏览器通过 HTTP/2 访问。
- HTTP/2 上的 WebSocket（RFC 8441），需等上游 SDK 支持 CONNECT 路由。
- 用上游 `unstable_session_fork` 替换 `_clawcode/session/fork`（语义含分支点 `source_leaf`，需另行评估）。
- 其他新的 unstable 能力：session status / archive、auth status、background work、tool call / terminal 取消、proxy chains。
- SSE 的 `Last-Event-ID` 续传，以及 RFD 推迟到 v2 的重连与 ping 语义；断线恢复继续由 `_clawcode/event` 恢复扩展负责。

## 9. 实施后修订

以下几点在审查后调整，以代码为准：

- `session/set_title` 对不存在的 Session 返回 `-32002`（schema 1.15.1 要求），不再走 `KernelFailure` 的内部错误映射；`-32002` 统一由 `UnavailableSession` 构造。
- `set_session_title` 同样支持已列出但未加载的 Session：持有 `reserve_session_build` 保留后直接写入 Store 的 name fact，不发事件（未加载的 Session 没有观察者）；遇到其他保留会等待其释放；遗留的 Starting 运行时直接返回 `SessionStarting`。
- 标题落盘后通知失败只记 `warn`，请求仍然成功。
- 观察者事件的 Turn 与 sink 选择抽为 `Kernel::observer_emitter`，由 `set_session_title` 与队列快照发布共用。
