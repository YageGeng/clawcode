# Codex 风格 PTY 终端与进程管理设计

## 背景

Clawcode 当前向 Agent 暴露 `bash` 工具。该工具通过 `tokio::process::Command` 启动一次性子进程，关闭 stdin，合并并流式展示 stdout/stderr，随后等待进程退出。这个模型适合构建和测试命令，但不能支持 REPL、确认提示、长时间服务、全屏终端程序，也不能让 Agent 在后续工具调用或后续 Turn 中继续与同一个进程交互。

本设计以 Codex 的 unified exec 行为为目标：模型只使用 `exec_command` 和 `write_stdin`，Session 持有独立的终端管理器，进程可以跨工具调用和 Turn 存活，同时由宿主可靠地列出、终止和清理。

## 目标

- 用 `exec_command` 和 `write_stdin` 直接替换模型侧 `bash` 工具。
- 同时支持普通 pipe 命令和真实 PTY 命令。
- 允许 Agent 写入 stdin、轮询新增输出、查看退出状态和发送中断。
- 后台进程跨 Turn 存活，但不能跨 Session、应用进程或 Session 恢复存活。
- 为 ACP/WebUI 提供 Session 级终端列表、单个终止、全部清理和实时状态通知。
- 对输出、进程数量、并发交互和进程树生命周期设置明确边界。
- 使用真实 PTY 和生产 `app` 后端完成最终端到端验收。

## 非目标

- 不实现用户直接操作的完整浏览器终端模拟器。
- 不持久化终端进程或在应用重启后恢复进程。
- 不公开任意 OS PID 的管理接口。
- 第一阶段不采集 CPU、RSS 等平台相关资源指标。
- 不改变用户 `!` / `!!` 一次性 shell 命令的公开语义。

## 总体架构

采用 Session 级进程管理器：

```text
Agent ── exec_command ─┐
Agent ── write_stdin ──┼── ToolExecutionContext ── Session TerminalManager
ACP/WebUI ─────────────┘                              │
                                                     ├── PTY process
                                                     └── Pipe process
```

`BuiltinToolFactory` 仍然只创建可跨 Session 复用的无状态工具注册表。`Session` 创建并持有一个 `Arc<TerminalManager>`，在每次工具执行时通过 `ToolExecutionContext` 注入对应 Session 的 `Arc<dyn TerminalService>`。这样不需要把 `ToolFactory` 改为 Session 感知，也不会创建按 Session 重复的工具定义。

`TerminalManager` 的具体实现位于 `crates/tools`，Kernel 负责所有权和生命周期，ACP 只通过 Kernel 能力访问它。每个消费者都必须先定位 Session，不能访问一个全局进程表。

## 模型工具协议

### `exec_command`

参数：

- `cmd: String`：交给当前平台 shell 执行的完整命令。
- `workdir: Option<PathBuf>`：可选工作目录；省略时使用当前 `ToolExecutionContext.cwd`。
- `tty: bool`：默认 `false`。为 `true` 时分配真实 PTY，为 `false` 时使用普通管道。
- `yield_time_ms: Option<u64>`：默认 10000ms，有效等待范围为 250–30000ms；范围外的值作为无效参数拒绝。
- `max_output_tokens: Option<usize>`：默认 10000，有效范围为 1–10000；范围外的值作为无效参数拒绝。

工具先创建进程，再等待“进程退出、等待窗口结束或当前 Turn 被取消”。后台 reader 在等待期间持续收集输出，但单独出现输出不会提前结束默认等待。响应包含：

- `output: String`：本次可见输出。
- `wall_time_seconds: f64`：本次交互消耗的墙钟时间。
- 进程已经退出时包含 `exit_code: Option<i32>`，不包含 `session_id`。
- 进程仍在运行时包含 `session_id: u32`，不包含 `exit_code`。

进程必须在初始等待开始前登记到 `TerminalManager`。如果初始等待期间 Turn 被取消，进程仍由 Session 持有，避免最后一个进程句柄随工具 Future 被丢弃。此时 Agent 可能尚未看到 `session_id`，但 ACP/WebUI 可以列出并清理该进程。

### `write_stdin`

参数：

- `session_id: u32`：`exec_command` 返回的当前 Session 内终端标识。
- `chars: Option<String>`：要写入的字符；省略或空字符串表示只轮询状态。
- `yield_time_ms: Option<u64>`：写入时默认 250ms、最大 30000ms；空轮询时默认至少等待 5000ms，最大值使用可配置的后台终端等待上限，初始默认值为 300000ms。
- `max_output_tokens: Option<usize>`：与 `exec_command` 相同，默认及上限均为 10000。

PTY 进程原样接收 `chars`，包括换行和控制字符。普通 pipe 进程不接受普通输入；唯一例外是 `\x03`，它被解释为向进程组发送中断。写入非空字符后按请求的等待窗口继续收集响应，默认 250ms，使常见提示响应能够在同一次调用中返回。

响应字段与 `exec_command` 一致。进程仍存活时继续返回同一个 `session_id`；进程退出时返回尚未消费的输出和 `exit_code`，然后从活动表回收。再次访问已回收 ID 返回 invalid-session 错误。

## 进程生命周期

```text
Spawn ──> Running ──> Exited ──> Reaped
              │           ▲
              ├──> Failed ┤
              └──> Terminating ──> Reaped
```

- 进程属于 Session，不属于单次 ToolCall、Turn 或 Run。
- Turn cancellation 只终止当前等待，不终止已经登记的后台进程。
- Agent 写入 `\x03` 表示交互式中断；PTY 接收控制字符，pipe 后端向进程组发送中断信号。
- ACP `terminate`、容量回收、Session 关闭或删除以及 Kernel shutdown 都必须最终终止完整进程树。
- 正常退出的进程暂时保留未消费输出；Agent 下一次读取后回收。容量清理可以提前回收已退出条目。
- 输出 reader 或 PTY 后端发生不可恢复错误时，管理器终止进程树、保存失败原因并发布 `failed` 状态。
- 应用重启、Session resume 和 Session fork 都创建空管理器；fork 不复制源 Session 的终端。

每个 Session 最多保存 64 个终端条目。达到上限时，管理器先按最近最少使用顺序回收已退出条目；仍不足时回收较旧的运行中进程，但保护最近使用的 8 个条目。运行中条目只有在完整进程树已经终止后才能移除。

`session_id` 使用 1000–99999 范围内的随机数字，并在当前 Session 活动表内检查冲突。该数字只具有 Session 内含义。

## 内部组件

### `TerminalService`

`TerminalService` 是工具依赖的异步 trait，只公开 `spawn` 和 `interact`。`exec_command` 与 `write_stdin` 是轻量适配器，负责参数解析、上下文默认值、工具响应和错误映射，不直接操作子进程。

### `TerminalManager`

`TerminalManager` 实现 `TerminalService`，并为 Kernel 提供 `list`、`terminate`、`terminate_all` 和事件订阅能力。它负责：

- Session 内 ID 分配和隔离。
- 容量限制与 LRU 回收。
- 进程状态转换。
- 生命周期清理。
- 宿主快照和实时事件。

管理器和字段超过三个的请求、响应及快照类型必须使用 `typed-builder`。所有新增函数都需要英文函数级注释；非平凡逻辑和旧代码变更原因也必须使用英文注释说明。

### `TerminalProcess`

每个进程条目持有平台后端、状态、输出缓冲、Agent 消费游标、最近访问时间以及独立的异步交互锁。同一终端的写入、轮询和回收通过该锁串行化；不同终端之间不共享交互锁，可以并发工作。

管理器表锁只保护条目查找、插入和移除，不能跨进程等待、输出等待或 OS I/O 持有，避免一个慢进程阻塞所有终端。

### 平台后端

- PTY 使用 `portable-pty`。阻塞式 PTY reader/writer 必须放到专用阻塞任务或线程中，不能阻塞 Tokio worker。
- pipe 使用 `tokio::process::Command`，持续异步排空 stdout 和 stderr。
- Unix 子进程进入独立进程组，终止和中断按进程组发送。
- Windows PTY 使用 ConPTY，并使用 Job Object 保证后代进程随终端关闭而终止；普通 pipe 进程也加入 Job Object。
- 后端通过 trait 统一暴露写入、等待、软中断和强制终止，平台差异不能泄漏到工具适配器。

## Shell 和环境

Unix 优先使用有效且可执行的绝对 `$SHELL`，随后回退到 `/bin/bash` 和 `sh`。Windows 使用 PowerShell。命令由 shell 解释，而不是按空格自行拆分参数。

子进程继承当前服务端允许继承的环境，并注入：

- `CLAW_SESSION_ID`：所属 Session。
- `CLAW_TURN_ID`：创建该进程的 Turn。进程跨 Turn 后此值保持不变。

`workdir` 必须在创建进程前解析为服务端现有目录。无效目录和 shell/PTY 创建失败不会分配或登记终端 ID。

## 输出模型与背压

每个进程使用容量为 1 MiB 的内存环形字节缓冲，并维护单调递增的绝对输出序号。后台 reader 持续排空进程输出，不能因为 ToolCall 已返回或 ACP 消费者变慢而停止读取。

Agent 拥有独立消费游标。每次 `exec_command` 或 `write_stdin` 只返回该游标之后的内容，然后推进游标。ACP/WebUI 使用广播通知或自己的观察游标，不推进 Agent 游标，因此宿主查看输出不会偷走模型应看到的内容。

如果 Agent 游标落后于环形缓冲当前起点，响应在保留内容前插入明确的 omission 标记。每次交互先原子地取得“当前 Agent 游标到本次快照末尾”的字节范围，并把游标推进到快照末尾；`max_output_tokens` 随后只限制这次响应的展示内容。响应裁剪时优先保留最近输出并加入 omission 标记，被裁掉的旧内容视为已经明确省略，不在后续调用中倒序补发。

现有 `OutputAccumulator` 继续服务 `!` / `!!`。它的 2000 行/50KB 可见尾部、截断后完整落盘和一次性完成语义不适用于持续终端，因此不与 `TerminalOutputBuffer` 合并。

进程输出只进入 1 MiB 环形缓冲并唤醒 Agent 交互等待，不发送到 ACP。终端管理器为可观察生命周期状态和每次显式 Agent 输入维护单调 `revision`，ACP 只发送快照失效通知；每个 ACP 连接、每个 Session 最多保留一个未被 list 确认的通知。显式输入使 WebUI 的最近活动时间及时更新，而慢客户端仍不能对进程 reader 施加背压，也不能让 ACP SDK 的无界发送队列随输出增长。

## ACP 与 WebUI

新增 ACP 扩展请求：

- `_clawcode/terminal/list`：参数为 `sessionId`，返回当前终端快照列表。
- `_clawcode/terminal/terminate`：参数为 `sessionId` 和 `terminalId`，幂等终止指定终端，返回是否找到并执行终止。
- `_clawcode/terminal/clean`：参数为 `sessionId`，终止并回收全部终端，返回清理数量。

宿主协议使用 `terminalId`，模型工具继续使用 Codex 风格的 `session_id`；两者表示同一个数字，命名差异用于避免与 ACP 的所属 `sessionId` 混淆。

终端快照包含：

- `terminalId`
- `command`
- `cwd`
- `tty`
- `status`：`running`、`exited` 或 `failed`
- `exitCode`
- `startedAt`
- `lastActivityAt`

`_clawcode/terminal/list` 返回 `{ revision, terminals }`。新增 `_clawcode/terminal/update` 通知，只承载 `{ sessionId, revision }`，表示该 Session 的终端快照已经失效。客户端收到更高 revision 后重新调用 list；低于已知目标 revision 的响应必须丢弃，并继续读取直到追平。通知是可合并的失效信号，`list` 是断线重连和并发竞争后的唯一状态真相。

终端事件不写入 transcript，也不复用原始 ToolCall 的局部更新，因为 `exec_command` ToolCall 完成后进程可能继续跨 Turn 运行。工具卡只展示每次 `exec_command` 或 `write_stdin` 的本次结果；WebUI 增加 Session 级后台终端列表及单个终止、全部清理操作，但不在本阶段实现完整终端模拟器。

## 错误与取消语义

- 参数类型或范围错误返回 `ToolError::InvalidArguments`。
- 工作目录、shell、PTY 或进程创建错误返回 `ToolError::Execution`，且不留下管理器条目。
- 未知、跨 Session 或已回收终端 ID 返回明确的 invalid-session 执行错误。
- 同一进程的并发交互按到达顺序排队，不返回竞争错误。
- 当前 ToolCall 被取消时返回 `ToolError::Cancelled`，但已登记进程继续存活。
- 正常信号退出可能没有数字退出码，此时 `exit_code` 为 `None`。
- 宿主 terminate 和 clean 保持幂等；已经不存在的目标不升级为服务器内部错误。
- 强制终止先阻止新交互，再终止完整进程树，等待后端确认，最后从管理器移除并发布 `removed`。

## 安全与日志

- 所有宿主操作先按 `sessionId` 定位 Session，再访问该 Session 的管理器。
- ACP 不接受客户端提供 OS PID、进程组 ID 或任意系统句柄。
- 命令正文和进程输出不得写入 INFO 日志。
- DEBUG 也不记录完整输出。ACP 参数日志继续使用项目已有的递归凭据字段脱敏规则。
- 日志只覆盖创建、转为后台、退出、失败、容量回收、强制终止和 Session 清理等生命周期边界。
- 日志携带 Session ID、终端 ID 和状态，但不得包含认证头、cookie、密码、token、完整提示词或完整响应正文。

## 兼容与迁移

- `shell_enabled=true` 时注册 `exec_command` 和 `write_stdin`；不再注册模型侧 `bash`。
- 不提供 `bash` 兼容别名，确保模型提示和工具协议只有一个执行路径。
- 历史 transcript 中的 `bash` ToolCall/ToolResult 保持可读取和回放，但新 Turn 的工具定义不再包含 `bash`。
- 用户 `!` / `!!` 继续使用现有 `BashExecutor`，保留 timeout、Turn cancellation、完整输出落盘和 2000 行/50KB 尾部展示。
- 依赖版本和路径统一声明在根 `[workspace.dependencies]`，子 crate 使用 `{ workspace = true }`；依赖分组和排序遵循根 `Cargo.toml`。

## 测试策略

实现按测试驱动开发推进。测试代码只放在 crate 级 `tests/` 集成测试目录或准确命名的 `#[cfg(test)] mod tests` 中。

### 单元和 crate 集成测试

- 工具 Schema、默认值、边界收敛和错误映射。
- pipe 快速退出、非零退出和无数字退出码。
- 后台进程返回 ID，后续轮询输出无重复、无遗漏。
- 真实 PTY 下 stdin/stdout 是终端，提示输入、换行和 `\x03` 有效。
- 同一终端交互串行，不同终端并发。
- Turn cancellation 在创建前阻止启动，在登记后只取消等待。
- 环形缓冲保持 1 MiB 边界，游标落后和响应 token 裁剪产生正确 omission 标记。
- 64 条容量、已退出优先、LRU 顺序和最近 8 条保护策略。
- Session ID 隔离，未知 ID 和幂等终止。
- Unix 进程组和 Windows Job Object 能清理子孙进程。
- Session close、delete、fork、resume 和 Kernel shutdown 的终端生命周期。
- ACP list、terminate、clean、revision 失效通知、单通知在途背压和 list 确认重置。
- 模型工具列表不再包含 `bash`，用户 `!` / `!!` 回归测试继续通过。

### 真实端到端验收

WebUI 端到端测试必须连接生产 `app` 后端及其配置的真实 provider，不引入 fixture backend。至少覆盖：

1. Agent 调用 `exec_command(tty=true)` 执行可验证 `stdin/stdout` TTY 状态的命令，工具返回后台 `session_id`。
2. Agent 使用 `write_stdin` 与一个真实交互程序通信，例如向等待 `input()` 的 Python 进程发送一行文本，并读取对应响应和最终退出码。
3. 在下一 Turn 继续访问上一 Turn 创建的 PTY，证明进程确实跨 Turn 存活。
4. 启动长时间进程后，通过 WebUI/ACP 查看列表并终止，确认状态通知、UI 更新和完整进程树退出。
5. 刷新或重连 ACP 后重新调用 list，确认活动状态可以重新同步；应用重启后确认不会伪造进程恢复。

真实验收必须观察工具调用、ACP 通知、WebUI 状态和 OS 进程退出结果，不能仅以模拟 manager 或 mocked PTY 的测试替代。

## 完成标准

- `cargo fmt`、相关 crate 测试、workspace 测试和 clippy 全部通过。
- WebUI lint、类型检查和生产构建通过。
- 上述真实 PTY 端到端场景在生产 `app` 后端和已配置 provider 上通过。
- 测试结束后没有遗留后台进程。
- 模型侧只能看到 `exec_command` 和 `write_stdin`，用户 `!` / `!!` 行为无回归。
- ACP/WebUI 能可靠列出、终止并清理当前 Session 的终端。
