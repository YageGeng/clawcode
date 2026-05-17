# Agent 双模式 ACP FS 设计规格

**日期**: 2026-05-17
**状态**: 待用户审核
**参考**: `agent-client-protocol-schema 0.12.0`

---

## 1. 背景

最终目标不是把 clawcode 变成只能通过 ACP 工作的 agent，而是让同一个 agent 能同时支持两种运行模式：

1. **Native 模式**：不经过 ACP 协议，直接由本项目的 CLI/TUI/kernel/tool runtime 工作。
2. **ACP 模式**：作为 ACP agent/server 对外暴露，能够接入 Zed 这类 ACP client。

因此，ACP 只能是 agent 的一层协议 adapter，不能成为 kernel、tool runtime 和文件访问能力的唯一抽象。

当前状态：

- `crates/acp/src/agent.rs::handle_initialize()` 会声明 `PromptCapabilities::image(true)`，但当前 prompt 处理并没有真正消费 image。
- `crates/acp/src/agent.rs::handle_initialize()` 已经接收 `InitializeRequest`，但当前没有使用请求里的 `client_capabilities`。
- `crates/tui/src/acp_client.rs` 作为本地 TUI 的 ACP client，只处理 `session/update` notification 和 `session/request_permission` request。
- `crates/acp/src/bin/client.rs` 作为历史命令行 ACP client 路径，本规格中直接废弃，不再为它补 fs capability 或 request handler。
- 现有内置文件工具在 native 模式下直接访问本地文件系统。

本设计要先补齐 ACP 的 `fs/read_text_file` 和 `fs/write_text_file` 能力协商，并让后续架构能自然扩展到 Zed 接入。

---

## 2. codex-acp 参考结论

参考本地 `zed-industries/codex-acp@156cb0d` 快照，codex-acp 的兼容策略不是把 Codex core 的文件工具替换成 ACP `fs/read_text_file` 或 `fs/write_text_file`。

关键点：

1. `initialize()` 会保存 `InitializeRequest.client_capabilities`。
2. `new_session()` / `load_session()` 会把 ACP request 里的 `cwd` 合并进 Codex session config。
3. Codex core 仍按自己的 tool runtime、approval、sandbox 和 cwd 执行文件读写。
4. codex-acp 主要把 Codex core 的事件、审批、MCP、配置和 session 生命周期适配成 ACP。
5. 该快照没有把 ACP fs request 作为 Codex 文件工具的执行 backend。

对 clawcode 的启发：

- 不应该为了支持 ACP/Zed 而拆掉现有 `read_file` / `write_file` 工具注册。
- ACP adapter 应该保存 client capability，并为 ACP 模式提供能够向 client 发 fs request 的 backend。
- 文件工具应保持模型可见语义不变；执行落点由工具注册时注入的 fs backend 决定。
- Zed 场景第一阶段可以沿用 codex-acp 模式：如果 agent 进程能看到 workspace cwd，就继续使用 local backend。
- ACP fs backend 是后续能力，用于需要 editor/client 代为读写文件的场景，而不是当前阶段强制替换默认文件工具。

---

## 3. 协议结论

`fs/read_text_file` 和 `fs/write_text_file` 是 ACP 的 agent-to-client JSON-RPC request，不是 agent/server 声明给模型的 tool call。

职责边界如下：

1. ACP client 在 `initialize` 请求中声明 `ClientCapabilities.fs`：
   - `read_text_file = true`
   - `write_text_file = true`
2. ACP agent/server 在 `handle_initialize()` 中读取并保存 `InitializeRequest.client_capabilities`。
3. 后续只有当 client 声明支持对应 fs capability 时，agent/server 才可以向 client 发送 `fs/read_text_file` 或 `fs/write_text_file` request。
4. 真实文件读取和写入发生在 ACP client 侧。
5. `terminal/*` 是另一组 agent-to-client request，本规格不声明也不实现。

对 Zed 接入来说，关键点是：Zed 会扮演 ACP client，clawcode 扮演 ACP agent/server。clawcode 不能假设自己总能直接访问 Zed workspace 的真实文件系统，而应该通过 client capability 判断是否能使用 ACP fs。

---

## 4. 目标

### 4.1 当前阶段目标

1. ACP agent/server 的 `handle_initialize()` 记录 client fs capability。
2. ACP agent/server 的 initialize response 不再声明 `image = true`。
3. TUI ACP client 在 initialize 时声明支持 `fs/read_text_file` 和 `fs/write_text_file`。
4. TUI ACP client 能处理 agent 发来的 `fs/read_text_file` 和 `fs/write_text_file` request。
5. `crates/acp/src/bin/client.rs` 在本规格中废弃，不为它增加 fs 支持。
6. 不声明、不实现 `terminal/*` capability。
7. 不改变现有 `ToolRegistry::register_builtins()` 和 `register_fs_tools()` 的默认注册行为。

### 4.2 长期架构目标

1. agent core、kernel、tool runtime 继续能在 native 模式下独立工作。
2. ACP adapter 只负责协议转换、capability 协商和 ACP request/notification 传输。
3. 文件工具仍然作为 agent 的普通工具暴露给模型；ACP fs 不直接暴露为模型 tool。
4. 现有 `read_file` / `write_file` / `edit_file` / `apply_patch` 工具名保持稳定，不按 ACP/native 模式暴露两套模型工具。
5. 文件访问能力最终需要有模式化 backend：
   - native 模式使用本地文件系统 backend。
   - ACP/Zed 模式默认可以沿用 local backend，这与 codex-acp 保持一致，前提是 agent 进程能访问 session cwd。
   - ACP/Zed 模式在需要 editor/client 代为读写文件时，可以使用 ACP client fs backend。
6. `ToolRegistry::register_fs_tools()` 在注册 `read_file` / `write_file` 时选择具体 fs backend。
7. 写文件审批仍然发生在 agent/tool runtime 侧，不能因为 ACP fs backend 而绕过现有 approval/policy。

---

## 5. 非目标

1. 当前阶段不迁移现有内置 read/edit/write 工具到 ACP fs backend。
2. 当前阶段不实现 Zed 端专用逻辑；只实现符合 ACP schema 的能力协商和 request handler。
3. 不让模型直接看到 `fs/read_text_file` 或 `fs/write_text_file` 作为 tool。
4. 不实现 `terminal/create`、`terminal/output`、`terminal/wait_for_exit`、`terminal/kill` 或 `terminal/release`。
5. 不实现二进制文件读写。
6. 不实现目录创建、目录读取、复制、删除、watch/unwatch 等 Codex app-server fs API。
7. 不改变 ACP schema 或 fork `agent-client-protocol` crate。
8. 不把 ACP 作为 kernel 的唯一入口。
9. 不为 `crates/acp/src/bin/client.rs` 增加新的 capability、request handler 或测试；该路径按废弃处理。
10. 当前阶段不修改 `ToolRegistry::register_builtins()`、`ToolRegistry::register_fs_tools()` 或默认文件工具名称。

---

## 6. 方案选项

### 6.1 方案 A：只补 TUI ACP client fs handler

只让 `crates/tui/src/acp_client.rs` 声明并处理 ACP fs request。`crates/acp/src/bin/client.rs` 不再扩展，按废弃路径处理。

优点：

- 改动最小。
- 方便本地验证 `fs/read_text_file` 和 `fs/write_text_file` 的 request/response 行为。
- 与当前项目方向一致：本地只支持好 TUI 这一个 ACP client。

缺点：

- 对 Zed 接入只提供 capability 协商基础，因为真正的 Zed 场景中 Zed 是 ACP client。
- agent/server 仍然不会基于 client capability 选择文件访问 backend。
- 容易误以为“本项目支持 ACP fs”已经完成，但实际上只是本地 client 具备 handler。

### 6.2 方案 B：ACP server 直接把文件工具改成发送 ACP fs request

在 ACP 模式下，直接让现有文件工具通过 ACP connection 调用 `fs/read_text_file` 和 `fs/write_text_file`。

优点：

- Zed 接入路径更快见效。
- ACP 模式下的文件访问会落到 client 侧。

缺点：

- 容易把 tool runtime 和 ACP connection 耦合在一起。
- native 模式和 ACP 模式会出现两套工具执行路径。
- 如果设计不好，写文件 approval/policy 可能被绕过或重复实现。

### 6.3 方案 C：注册工具时注入 fs backend

保留 `read_file`、`write_file`、`edit_file`、`apply_patch` 这一组模型可见工具名，但修改注册流程，让 fs tools 在构造时持有 `Arc<dyn FsBackend>`。工具执行时不再直接固定调用 `tokio::fs`，而是调用自身持有的 backend。

运行时选择：

- native 模式：注册 fs tools 时传入 `LocalFsBackend`。
- ACP/Zed 模式默认：也可以注册 `LocalFsBackend`，匹配 codex-acp 的做法，前提是 agent 进程能访问 session cwd。
- ACP/Zed 模式可选：注册 fs tools 时传入 `AcpFsBackend`，backend 先检查 client fs capability，再通过 ACP connection 发送 `fs/read_text_file` 或 `fs/write_text_file`。

优点：

- agent core 不依赖 ACP，native 模式保持独立。
- ACP/Zed 模式可以通过同一套工具语义访问 client workspace。
- approval/policy 仍然留在工具执行边界，backend 只负责执行已授权的 IO。
- 后续可继续扩展目录、metadata、patch 等能力，而不污染 ACP adapter。
- 符合现有 `ToolRegistry` 的生命周期：工具实例在注册时决定依赖，不需要每次 tool execution 从 `ToolContext` 取 backend。

缺点：

- ACP backend 需要拿到当前 session 对应的 ACP client connection，因此不能只是一个无状态 backend。
- 如果 `ToolRegistry` 继续全局共享，ACP backend 需要通过 session id 路由到正确的 client connection；或者后续要支持 session-local registry。
- 当前阶段不会一次性完成全部 edit/patch 迁移，需要先覆盖 read/write。

推荐采用方案 C，但按阶段实施。当前阶段只完成 capability 协商、TUI ACP client fs handler、取消 image 声明；后续单独做 workspace filesystem backend 和工具迁移。历史命令行 ACP client 不进入当前阶段，也不作为后续 Zed 接入基础。

---

## 7. 选定设计

本规格选定“方案 C 的分阶段路线”。

### 7.1 当前阶段数据流

```text
ACP client initialize
  -> ClientCapabilities.fs(read_text_file=true, write_text_file=true)
  -> ACP agent/server handle_initialize records client_capabilities
  -> InitializeResponse no longer advertises PromptCapabilities.image

Local ACP client test path:
  -> agent/server may send fs/read_text_file or fs/write_text_file
  -> local TUI ACP client performs local filesystem operation
  -> local TUI ACP client returns ACP fs response or JSON-RPC error
```

### 7.2 codex-acp 兼容默认路径

```text
Zed ACP client initialize
  -> Zed sends ClientCapabilities.fs
  -> clawcode ACP agent records capabilities
  -> clawcode builds session with cwd from ACP session request
  -> ToolRegistry registers read/write tools with LocalFsBackend

Model calls normal file tool
  -> tool runtime performs approval/policy
  -> LocalFsBackend reads/writes files under session cwd
  -> tool returns normal tool output to model
```

这条路径是第一优先兼容路径，因为它与 codex-acp 的做法一致：ACP 负责 session/cwd/event/permission adapter，核心工具执行仍留在 agent runtime 内。

### 7.3 后续 ACP fs backend 路径

```text
Zed ACP client initialize
  -> Zed sends ClientCapabilities.fs
  -> clawcode ACP agent records capabilities
  -> ToolRegistry registers read/write tools with AcpFsBackend

Model calls normal file tool
  -> tool runtime performs approval/policy
  -> AcpFsBackend checks client capability and resolves the session client
  -> clawcode ACP agent sends fs/read_text_file or fs/write_text_file to Zed
  -> Zed performs file IO and returns content/result
  -> tool returns normal tool output to model
```

重要限制：

- `fs/read_text_file` 和 `fs/write_text_file` 仍然不是模型 tool。
- 模型看到的仍然是 clawcode 自己的文件工具。
- ACP fs backend 只能作为已授权 IO 的执行 backend。
- native 模式不能依赖 ACP connection 才能工作。
- 是否使用 ACP fs backend 是工具注册阶段的选择；模型可见工具名不变。

---

## 8. 运行模式边界

### 8.1 Native 模式

适用场景：

- 本项目 CLI/TUI 直接启动 agent，且不要求经过 ACP client。
- 没有外部 ACP client。
- 本地工具直接访问当前工作目录。

行为要求：

1. agent 不需要 ACP initialize。
2. 文件工具注册时使用 `LocalFsBackend`。
3. 现有 approval/policy 行为保持不变。
4. 没有 ACP client capability 时，不影响 native 模式。
5. 继续默认注册 `read_file`、`write_file` 以及 provider 相关的 edit/patch 工具。

### 8.2 ACP/Zed 模式

适用场景：

- Zed 作为 ACP client 启动或连接 clawcode ACP agent。
- clawcode 通过 ACP session request 接收 cwd、prompt、permission 等交互。

行为要求：

1. `handle_initialize()` 必须保存 client capability。
2. agent/server 必须以 client capability 决定是否可以发送 ACP fs request。
3. 第一阶段对齐 codex-acp，继续允许文件工具通过 `LocalFsBackend` 访问 session cwd。
4. 后续 ACP/Zed 模式可以在注册 fs tools 时选择 `LocalFsBackend` 或 `AcpFsBackend`。
5. 如果 session 选择 ACP backend，但 client 不支持某个 fs capability，对应文件能力必须返回清晰错误，不能静默退回不可靠的本地路径假设。
6. 如果 session 选择 local backend，则不需要 client fs capability。

### 8.3 本地 TUI in-process ACP 模式

适用场景：

- TUI 通过 in-process ACP server 解耦 UI 和 kernel。

行为要求：

1. TUI 作为本地 ACP client，可以声明并处理 fs capability。
2. 这条路径主要用于本地协议完整性验证。
3. 即使 TUI 支持 ACP fs，也不代表 native 模式必须经过 ACP。
4. `crates/acp/src/bin/client.rs` 不再作为需要维护的本地 ACP client 路径。

### 8.4 废弃的命令行 ACP client

`crates/acp/src/bin/client.rs` 在本规格中直接废弃。

行为要求：

1. 不为该文件新增 fs capability。
2. 不为该文件新增 `fs/read_text_file` 或 `fs/write_text_file` request handler。
3. 不为该文件新增测试。
4. 后续如果仍需要命令行 agent，应走 native CLI 路径，而不是维护这个历史 ACP client。

---

## 9. 兼容现有工具注册与执行流程

当前 `crates/tools/src/builtin/mod.rs::register_builtins()` 会固定调用 `register_fs_tools(false)`，而 `crates/tools/src/builtin/fs/mod.rs::register_fs_tools()` 会默认注册 `read_file` 和 `write_file`。`crates/kernel/src/turn.rs::execute_turn()` 每轮构造 `ToolContext`，再调用 `ToolRegistry::execute_structured()` 执行工具。

兼容策略：

1. 引入 `FsBackend` trait，表达 read/write 工具需要的文本文件操作。
2. `ReadFile` / `WriteFile` 在构造时接收 `Arc<dyn FsBackend>` 并保存到字段中。
3. `register_fs_tools()` 负责选择 backend 并注册工具实例。
4. 保留默认入口 `register_builtins()`，它内部使用 `LocalFsBackend`，确保 native 模式不需要改调用点。
5. 增加显式入口，例如 `register_builtins_with_fs_backend(is_anthropic, fs_backend)` 或 `register_fs_tools_with_backend(is_anthropic, fs_backend)`，供 ACP server bootstrap 选择 `AcpFsBackend`。
6. 后续迁移时，不注册 `read_file_local` / `read_file_acp` 这类双份工具；模型可见工具名保持稳定。
7. `execute_turn()` 继续按现在的固定流程执行 `ToolRegistry::execute_structured()`，不需要知道 fs backend 细节。
8. approval/policy 仍然在工具执行前完成，backend 不负责向用户申请权限。

这能解决“工具已经默认注册、kernel 固定流程执行”的兼容问题：kernel 执行流程不变，模型工具名不变，差异只发生在 `ToolRegistry` 注册工具实例时选择的 backend。

---

## 10. FsBackend trait 设计

trait 放在 `crates/tools`，因为 `ReadFile` / `WriteFile` 属于 tools crate，且 native 实现也应随 tools crate 一起工作。

建议接口：

```rust
#[async_trait]
pub trait FsBackend: Send + Sync {
    /// Read a UTF-8 text file after the tool resolves the user path.
    async fn read_text_file(
        &self,
        request: FsReadRequest,
    ) -> Result<FsReadResponse, FsBackendError>;

    /// Write UTF-8 text content after approval has been handled by the caller.
    async fn write_text_file(
        &self,
        request: FsWriteRequest,
    ) -> Result<FsWriteResponse, FsBackendError>;
}
```

配套 request/response 类型放在 tools crate 内部或 public API 中，第一版只包含 `read_file` 和 `write_file` 需要的字段：

- `FsReadRequest { cwd, path, offset, limit }`
- `FsReadResponse { content }`
- `FsWriteRequest { cwd, path, content }`
- `FsWriteResponse { bytes_written, display_path }`

设计约束：

1. tool 继续负责解析模型参数和执行 approval 判断。
2. backend 负责把已解析的文件操作落到具体 IO 实现。
3. `LocalFsBackend` 保持当前行为，包括相对路径基于 `cwd`、读文件 canonicalize、写文件自动创建父目录。
4. `AcpFsBackend` 将 `offset` 转成 ACP 的 1-based `line = offset + 1`，将 `limit` 原样传给 ACP read request。
5. `AcpFsBackend` 写文件时调用 `fs/write_text_file`；如果 ACP client 不支持写 capability，则返回 backend error。
6. `AcpFsBackend` 无法支持本地 canonicalize 语义，路径解析应在发送 request 前转成绝对路径。

---

## 11. AcpFsBackend 设计

`AcpFsBackend` 不应该放在 `crates/tools`，因为 tools crate 不应该依赖 ACP schema 或 connection。推荐把 trait 放在 tools，把 ACP 实现放在 `crates/acp`。

关键问题是 session routing：ACP request 需要 `session_id` 和 `ConnectionTo<Client>`。如果 fs tools 在注册时选 `AcpFsBackend`，backend 必须能在执行时找到当前 session 的 client connection。

推荐实现一个 ACP fs router：

```text
AcpFsBackend
  -> Arc<AcpClientFsRouter>
  -> map SessionId -> ConnectionTo<Client>
```

执行流程：

```text
ReadFile.execute()
  -> backend.read_text_file(FsReadRequest { session_id, cwd, path, offset, limit })
  -> AcpFsBackend resolves session_id to ConnectionTo<Client>
  -> send ReadTextFileRequest(session_id, absolute_path).line(offset + 1).limit(limit)
  -> return content to ReadFile
```

为了支持这个流程，`ToolContext` 需要包含当前 `session_id`，但不包含 backend。backend 仍然是工具注册时选好的，只是执行 request 需要 session id 做路由。

`ToolContext` 调整：

- 增加 `session_id: Option<protocol::SessionId>` 或直接 `session_id: protocol::SessionId`。
- `execute_turn()` 构造 `ToolContext` 时填入当前 session id。
- `ToolContext::for_test()` 使用固定测试 session id。

这样既满足“tools 注册时选 fs 实现”，又能让 ACP backend 在执行时知道该向哪个 ACP client 发 request。

---

## 12. ACP Agent/Server 初始化

修改位置：

- `crates/acp/src/agent.rs`

行为要求：

1. `handle_initialize(request: InitializeRequest)` 不再忽略 `request`。
2. 将 `request.client_capabilities` 保存到 `self.client_capabilities`。
3. `InitializeResponse.agent_capabilities.prompt_capabilities` 不再调用 `.image(true)`。
4. 继续保持现有 `embedded_context(true)`，因为本轮用户只要求取消 image 声明。
5. 继续保持现有 MCP HTTP、load session、auth logout、session close/list capability。
6. 不新增任何 terminal capability。

验收标准：

- 当 client 传入 fs capability 时，agent/server 内部保存的 client capability 能反映 `read_text_file = true` 和 `write_text_file = true`。
- initialize response 中的 prompt capability 不再声明 image 支持。
- 没有 client fs capability 时，agent/server 不能假设可以发送 ACP fs request。

---

## 13. ACP Client 初始化

修改位置：

- `crates/tui/src/acp_client.rs`

行为要求：

1. TUI ACP client 发送 `InitializeRequest::new(ProtocolVersion::V1)` 时附带 `client_capabilities`。
2. `client_capabilities.fs.read_text_file = true`。
3. `client_capabilities.fs.write_text_file = true`。
4. 不声明 `client_capabilities.terminal`。

验收标准：

- TUI client initialize request 明确携带 fs capability。

说明：

- 这部分只影响本项目作为 ACP client 的本地路径。
- 当前本地路径只支持 TUI 这一个 ACP client。
- Zed 接入时，fs capability 来自 Zed 的 initialize request，不来自本项目的 TUI client。
- `crates/acp/src/bin/client.rs` 不参与该能力建设。

---

## 14. ACP Client FS Handler

修改位置：

- `crates/tui/src/acp_client.rs`

TUI client handler 只负责 ACP client request 到本地文件系统操作的转换，不参与 agent/server tool runtime。因为命令行 ACP client 已废弃，本轮不需要为了多 client 复用而抽 shared helper；除非实现时能明显降低复杂度，否则优先把逻辑保持在 TUI ACP client 边界内。

### 14.1 `fs/read_text_file`

输入：

- `session_id`
- `path`
- `line`
- `limit`

行为：

1. `path` 必须是绝对路径；相对路径返回 invalid params。
2. 文件按 UTF-8 文本读取。
3. `line` 是 1-based 起始行。
4. 未提供 `line` 时，从第 1 行开始。
5. `line = 0` 返回 invalid params。
6. 未提供 `limit` 时，返回从起始行到文件末尾。
7. 提供 `limit` 时，最多返回 `limit` 行。
8. 读取时保留原始换行符。
9. 起始行超过文件总行数时，返回空字符串。
10. 文件不存在、权限错误、非 UTF-8 内容返回 JSON-RPC error。

### 14.2 `fs/write_text_file`

输入：

- `session_id`
- `path`
- `content`

行为：

1. `path` 必须是绝对路径；相对路径返回 invalid params。
2. 将 `content` 原样写入文件。
3. 父目录必须已经存在；本轮不自动创建父目录。
4. 如果目标文件已存在，则覆盖目标文件。
5. 如果目标文件不存在且父目录存在，则创建文件。
6. 权限错误、父目录缺失、路径不是普通文件等情况返回 JSON-RPC error。

---

## 15. 后续 Workspace FS Backend 方向

本节定义后续设计边界，不属于当前阶段必须实现的代码。

后续应在 agent/tool runtime 内引入一个 filesystem backend，原则如下：

1. backend 接口表达 agent 需要的文件操作语义，而不是照搬 ACP method 名称。
2. local backend 直接使用本地文件系统。
3. ACP backend 通过 ACP connection 发送 `fs/read_text_file` 和 `fs/write_text_file`。
4. 工具的 approval/policy 发生在调用 backend 之前。
5. ACP backend 必须检查 client capability；不支持时返回 capability unsupported。
6. kernel 和工具不能直接依赖 Zed；只能依赖 `tools::FsBackend` trait。
7. backend 通过工具构造函数注入，不通过 `ToolContext` 注入。
8. `ToolContext` 只提供执行上下文，例如 cwd、approval mode、session id。
9. 只有当工具注册时选择 ACP backend，才要求 client fs capability；local backend 不要求该 capability。

这样可以保证：

- native 模式不需要 ACP。
- ACP/Zed 模式能使用 client workspace 文件系统。
- 模型可见工具语义保持一致。
- codex-acp 风格的 local cwd 文件访问仍然可用。

---

## 16. 错误策略

推荐错误分类：

- 请求参数不合法：返回 invalid params。
- session 选择 ACP backend 但 client capability 不支持：返回清晰的 capability unsupported 错误。
- 文件系统 IO 失败：返回 internal error，并附带简短错误信息。
- 非 UTF-8 文件：返回 internal error，并说明文件不是有效 UTF-8 文本。

错误信息应避免泄露不必要的环境信息，但可以包含目标 path，便于本地调试。

---

## 17. 安全边界

ACP fs request 是 agent/server 发给 client 的能力调用。client handler 本身只做参数校验和文件系统操作，不负责判断模型是否应该拥有写权限。

安全责任分层：

1. 模型调用 clawcode 的普通文件工具。
2. agent/tool runtime 在工具执行边界执行 approval/policy。
3. 已授权后，工具持有的 fs backend 执行实际 IO。
4. native backend 写本地文件。
5. ACP backend 向 client 发送 `fs/write_text_file`。

当前阶段没有把文件工具迁移到 ACP backend，因此不会引入新的写文件路径。

---

## 18. 测试计划

### 18.1 ACP agent/server 测试

覆盖点：

1. `handle_initialize()` 会记录 client fs capability。
2. initialize response 不再声明 image capability。
3. 没有 fs capability 时，server 内部保存状态反映不支持 fs。

建议测试文件：

- `crates/acp/src/agent.rs` 内的单元测试模块。

### 18.2 TUI client fs handler 测试

覆盖点：

1. `fs/read_text_file` 能按 `line = 2`、`limit = 2` 返回第 2 到第 3 行，并保留换行符。
2. `fs/read_text_file` 遇到 `line = 0` 返回错误。
3. `fs/read_text_file` 遇到相对路径返回错误。
4. `fs/write_text_file` 能在父目录存在时创建新文件并写入原始内容。
5. `fs/write_text_file` 遇到缺失父目录返回错误。
6. `fs/write_text_file` 遇到相对路径返回错误。

### 18.3 FsBackend 工具注册测试

覆盖点：

1. `register_builtins()` 默认注册使用 `LocalFsBackend` 的 `read_file` / `write_file`。
2. 显式注册入口可以注入 fake backend。
3. `ReadFile` 执行时调用注入的 backend，而不是直接读本地文件。
4. `WriteFile` 执行时调用注入的 backend，而不是直接写本地文件。
5. 工具名仍然是 `read_file` / `write_file`。

### 18.4 固定工具名兼容检查

覆盖点：

1. 不新增 `read_file_acp`、`write_file_acp` 或其他重复模型工具。
2. `ToolRegistry::definitions()` 中只出现原有文件工具名。

### 18.5 废弃命令行 ACP client 检查

覆盖点：

1. 不为 `crates/acp/src/bin/client.rs` 新增 fs capability 测试。
2. 不为 `crates/acp/src/bin/client.rs` 新增 request handler 测试。
3. 当前阶段不要求该历史 client 支持 ACP fs。

### 18.6 后续 Zed/ACP 集成测试方向

当前阶段不做 Zed 专用集成测试。后续 workspace fs backend 落地后，需要增加两类 fake ACP/client 测试：

1. codex-acp 兼容路径：ACP 模式注册 `LocalFsBackend`，普通文件工具通过 session cwd 读写本地文件。
2. ACP backend 路径：ACP 模式注册 `AcpFsBackend`，fake ACP client 声明 fs capability 并返回 fs response，验证普通文件工具会通过 ACP fs backend 读写 client 文件系统。

---

## 19. 分阶段实施边界

本节不是 implementation plan，只描述后续 plan 的任务边界。用户明确 review 通过前，不进入代码实现。

### 19.1 当前阶段

1. 增加 ACP agent/server initialize 测试，验证 client fs capability 被保存、image capability 被移除。
2. 修改 `handle_initialize()`，让测试通过。
3. 增加 TUI ACP client fs handler 测试。
4. 实现 `fs/read_text_file` handler。
5. 实现 `fs/write_text_file` handler。
6. 在 TUI client builder 注册 fs request handler，并让 initialize 声明 fs capability。
7. 明确不修改 `crates/acp/src/bin/client.rs`。
8. 明确不修改 `ToolRegistry::register_builtins()` 和 `register_fs_tools()`。
9. 运行相关 cargo test。

### 19.2 后续阶段

1. 在 tools crate 设计并实现 `FsBackend` trait。
2. 实现 `LocalFsBackend`，迁移 `ReadFile` / `WriteFile` 使用注入 backend。
3. 扩展 `register_fs_tools()` 或新增注册入口，支持注册时选择 backend。
4. 保持 `register_builtins()` 默认使用 `LocalFsBackend`，保证 native 路径不破。
5. 在 `ToolContext` 中加入 session id，供 ACP backend 路由 request。
6. 在 acp crate 实现 `AcpFsBackend` 和 session-to-client router。
7. ACP/Zed 模式按需注册 `AcpFsBackend`。
8. 增加 fake ACP client 集成测试，验证 Zed 类场景。

---

## 20. Review Gate

本规格状态为“待用户审核”。

在用户明确回复“review 通过”之前，不允许进入实现阶段，不允许修改 Rust 代码。

review 通过后，下一步按 Superpowers 流程进入 implementation plan，计划文件写入：

```text
docs/superpowers/plans/2026-05-17-acp-fs-client.md
```

---

## 21. 自检

- 没有使用 `TBD` 或 `TODO` 占位。
- 明确了 native 模式和 ACP/Zed 模式都必须长期可用。
- 明确了 ACP 是 adapter，不是 agent core 的唯一入口。
- 当前阶段范围只包含 ACP fs capability、client fs request handler、initialize image capability 修正。
- 当前阶段只支持好 TUI 这一个本地 ACP client，命令行 ACP client 路径按废弃处理。
- 明确当前阶段不修改默认工具注册流程。
- 明确后续兼容固定 kernel 执行流程的方式是工具注册时注入 `Arc<dyn FsBackend>`。
- 明确 `ToolContext` 只补 session id，不承载 backend。
- 明确 codex-acp 兼容路径默认可以继续使用 local backend 和 session cwd。
- 后续阶段明确纳入 workspace filesystem backend，但不混入当前实现范围。
- 明确排除了 terminal、schema 修改和 Zed 专用协议逻辑。
- 明确说明 fs 读写在 ACP 模式下发生在 ACP client 侧。
- 明确说明 `fs/read_text_file` 和 `fs/write_text_file` 不是模型 tool call。
- 明确保留 review gate，避免在 spec 审核前进入代码实现。
