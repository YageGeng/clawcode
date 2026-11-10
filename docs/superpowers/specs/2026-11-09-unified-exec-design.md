# Unified Exec 进程管理设计规格

日期：2026-11-09。状态：五段设计已在对话中逐段确认，待审阅本文。参考：codex 仓库 `8703d32bd0` 的 `codex-rs/core/src/unified_exec/`（进程管理）、`codex-rs/core/src/tools/handlers/shell_spec.rs`（工具定义）、`codex-rs/utils/pty/`（PTY 与进程组）、`codex-rs/core/src/shell_snapshot.rs` 与 `codex-rs/shell-command/src/shell_snapshot_capture.rs`（shell snapshot）、`codex-rs/core/src/context/world_state/command_sessions.rs`（`<command_sessions>`）。前置：[Plan 模式设计](./2026-11-05-plan-mode-design.md) 定义了 `custom` 条目、模式说明消息的放置方式与 plan 门控。

## 1. 目标与已确认决策

目标：把 codex 的 unified exec 移植到 clawcode。模型通过 `exec_command` 启动命令，命令在 `yield_time_ms` 内没结束就自动转为后台进程并返回 session ID；之后通过 `write_stdin` 轮询输出、写入输入或中断。后台进程跨 turn 存活，由会话统一管理。

成功标准：

- 模型启动 dev server，10 秒后拿到 session ID，turn 不被卡住；之后能轮询到新输出。
- `tty:true` 启动的进程能回答 `[y/N]`、操作 REPL；`\x03` 能中断进程。
- 用户按 Esc 后后台进程仍在；关闭会话后进程全部退出。
- `/ps` 列出后台进程，`/stop` 全部终止。
- WebUI 中后台进程的卡片在 turn 结束后持续刷新输出，进程退出时显示退出码；输入框附近显示后台终端数量，可以逐个终止或全部停止。

| 决策 | 结论 |
|---|---|
| 范围 | codex unified exec 全部能力，去掉沙箱相关部分（见第 13 节） |
| `bash` 工具 | 删除。`enable_shell = true` 时只注册 `exec_command`、`write_stdin` |
| 历史兼容 | 不考虑历史会话的迁移与兼容。删除 `ToolResultDetails::Bash` 及其映射；含旧 `bash` 工具结果的会话不保证能打开或正确显示 |
| 用户 `!` 命令 | 继续使用 `BashExecutor`，不进入进程表 |
| 架构 | 方案 A：进程层放 `tools` crate，Kernel 负责会话接线、生命周期与事件 |
| PTY | 新增 workspace 依赖 `portable-pty`；`tty` 默认 `false` |
| 后台卡片 | 照 codex：原卡片持续刷新，进程退出时收尾 |
| 输出推送 | 沿用现有整段快照替换，100 ms 节流；不发增量 |
| 截断 | codex 的 token 预算与头尾保留，另保留 clawcode 的完整输出文件 |
| 工具名 | `exec_command`、`write_stdin` 为 Kernel 保留名，扩展不能覆盖 |
| `<command_sessions>` | 实现，默认开启（codex 中为开发中、默认关闭） |
| 配置 | 新增 `[tools.exec]` 段（第 10 节） |

## 2. 现状

- `crates/tools/src/bash.rs` 的 `BashExecutor::execute` 一次完成启动、读输出、等待和终止，被 `bash` 工具（`crates/tools/src/builtin/bash.rs`）和用户 `!` 命令（`crates/kernel/src/runtime/command/user_bash.rs`）共用。子进程 stdin 为 `Stdio::null()`，stdout/stderr 为管道，用 `process_group(0)` 建新进程组，取消或超时时对进程组发 SIGKILL；没有 PTY，没有默认超时。
- `bash` 工具由 `tools` 内置工厂（`crates/tools/src/builtin/mod.rs`）注册，结果带 `ToolResultDetails::Bash`（`crates/protocol/src/tool.rs`）。
- Kernel 的 `Session`（`crates/kernel/src/runtime.rs`）持有会话级状态；`BackgroundSubagentTool`（`crates/kernel/src/runtime/subagent/background.rs`）是持有 `Weak<Session>` 的 Kernel 工具，`SessionBackgroundWork.shutdown` 在 `close_session`（`crates/kernel/src/runtime/session/close.rs`）开头被取消。
- Kernel 工具通过 `SessionToolState::register_kernel`（`crates/kernel/src/runtime/tools/state.rs`）注册，名字记入 `kernel_owned`；plan 门控（`crates/kernel/src/runtime/plan/gate.rs`）对 `kernel_owned` 工具直接放行。
- ACP 把 `bash` 映射为 terminal：`ToolExecutionStart` 打开 terminal（terminal id 即工具调用 ID），`ToolExecutionUpdate` 整段替换输出，工具结果收尾并带退出状态（`crates/acp/src/mapping.rs`、`crates/acp/src/terminal.rs`）。WebUI 的 `ToolCallCard.tsx` 渲染 terminal 实体。
- run 之外的操作（如用户 `!` 命令）自己生成 turn id 并发事件；`custom` 条目（如 plan 模式的 `session_mode`）在重放时按 `customType` 识别；模式说明消息是隐藏的 `MessageContent::Extension`，有未回结果的工具调用时排在结果之后（`crates/kernel/src/runtime/plan/note.rs`）。
- ACP 扩展方法集中在 `AcpExtensionMethod`（`crates/protocol/src/identity.rs`），命名为 `_clawcode/session/...`。
- 应用退出时（`crates/app/src/main.rs`）只停止 transport，不逐个关闭会话。ACP 客户端断开不会关闭会话，只有 `session/close` 会。
- 按名字 `"bash"` 或参数 `command` 做判断的位置：`crates/prompt/src/system.rs`、`crates/acp/src/tool_call.rs`、`crates/acp/src/mapping.rs`、`crates/acp/src/projection.rs`、`crates/kernel/src/runtime/plan/gate.rs`、`extensions/src/available/command_guard.rs`、`web/src/features/conversation/ToolCallCard.tsx`。

## 3. 架构与模块边界

```
tools crate（不依赖 kernel）
  exec/
    process.rs   ExecProcess：PTY（portable-pty）与管道（tokio + setsid）两种句柄
    output.rs    头尾保留缓冲；pending（模型轮询）与 transcript（UI、退出记录）
    manager.rs   ExecProcessManager：进程表、编号分配、淘汰、终止
    request.rs   参数解析与 yield 规则
    response.rs  结果头部格式、token 截断、完整输出文件
    env.rs       env policy 与强制覆盖
    shell.rs     用户 shell 识别与 argv
    snapshot.rs  shell snapshot 生成与命令改写
  trait ExecEventSink   后台输出快照、进程退出

kernel
  Session 新字段 exec: Option<Arc<ExecProcessManager>>（enable_shell = false 时为 None）
  runtime/exec/
    mod.rs       注册；SessionExecSink 实现 ExecEventSink（发 protocol 事件、写 exec_exit 条目）；查询与终止接口
    tools.rs     ExecCommandTool、WriteStdinTool（持 Weak<Session>，register_kernel）
    sessions.rs  <command_sessions> 说明消息
  close_session  acquire_close() 返回后调用 exec.terminate_all()
  command/builtin.rs  /ps、/stop（别名 /clean）

protocol  ToolResultDetails::Exec；事件 ExecOutput、ExecExited；三个 AcpExtensionMethod
acp       exec 结果与事件映射为 terminal 更新；三个扩展方法
web       卡片在 turn 结束后继续接收更新；后台终端计数与列表
```

- `enable_shell = true` 时，Kernel 为每个会话注册两个工具；子会话各有独立的 `ExecProcessManager`。
- `exec_command`、`write_stdin` 加入 `RESERVED_TOOL_NAMES`，扩展注册同名工具会被拒绝。
- 删除 `BashTool`、`ToolResultDetails::Bash` 及 ACP/WebUI 中对应的映射。`BashExecutor` 保留给用户 `!` 命令。
- 新增 workspace 依赖 `portable-pty`；`nix` 现有的 `process`、`signal` feature 已包含 `setsid`、`setpgid`、`prctl`、`killpg`，不加 feature；`tools` 新依赖 `config`、`fastrand`（均已在 workspace 中）。依赖按 CLAUDE.md 第 11、12 条分组并在根 `[workspace.dependencies]` 声明。
- 代码组织遵循 CLAUDE.md 第 5、6、8、15 条：超过 3 个字段的结构体用 `typed-builder`；共享上下文的函数挂在持有上下文的结构体上；转换用 `From`/`TryFrom`。

## 4. 工具接口与执行语义

### 4.1 `exec_command`

工具描述用 codex 原文："Runs a command in a PTY, returning output or a session ID for ongoing interaction."

| 参数 | 默认值 | 规则 |
|---|---|---|
| `cmd`（必填） | — | shell 命令 |
| `workdir` | 本轮 cwd | 目录不存在时返回错误 |
| `tty` | `false` | `true` 时分配 24×80 的 PTY |
| `yield_time_ms` | 10000 | 限制在 250–30000；Windows 下限 10000 |
| `max_output_tokens` | 10000 | 上限 262144（1 MiB ÷ 4） |
| `shell` | 用户默认 shell | `$SHELL`，否则依次 `/bin/bash`、`/bin/sh` |
| `login` | `true` | `true` 用 `-lc`，`false` 用 `-c` |

不支持 `sandbox_permissions`、`additional_permissions`、`justification`、`prefix_rule`、`environment_id`。

### 4.2 `write_stdin`

工具描述用 codex 原文："Writes characters to an existing unified exec session and returns recent output."

| 参数 | 默认值 | 规则 |
|---|---|---|
| `session_id`（必填） | — | `exec_command` 返回的编号 |
| `chars` | `""` | 为空表示只轮询 |
| `yield_time_ms` | 写入 250；轮询 5000 | 写入上限 30000；轮询限制在 5000 到 `background_terminal_max_timeout_ms` |
| `max_output_tokens` | 10000 | 同 4.1 |

### 4.3 执行流程

1. **启动**：分配 1000–100000 内未被占用的随机编号，启动进程，**在开始等待前**写入进程表，同时启动该进程的流式任务和退出监视任务。
2. **等待**：等到进程退出或时间到，取出并清空 pending 缓冲。
   - 时间内退出：从表中删除，结果带退出码。
   - 到时仍在运行：结果带 session ID。
   - 等待中 turn 被取消：工具返回 `ToolError::Cancelled`，进程保留在表中。
3. **后台退出**：表中记录保留，状态为“已退出、待取结果”，直到模型下一次 `write_stdin` 取走剩余输出和退出码后删除。
4. **写入**：
   - tty 进程：写入字节，等待 100 ms 后开始收集输出。
   - 非 tty 进程：stdin 已关闭。`chars == "\x03"` 时对进程组发 SIGINT；其他非空内容返回错误 `stdin is closed for this session; rerun exec_command with tty=true to keep stdin open`。
   - 写入时进程已退出：忽略写入错误，照常收集剩余输出与退出码（与 codex 相同）。
   - 同一进程的写入和轮询由该进程的交互锁串行执行；取锁后用 `Arc::ptr_eq` 确认仍是同一进程，防止编号复用后误操作。
5. **错误**：编号不存在返回 `Unknown process id N`；启动失败返回 `Failed to create unified exec process: ...`；写入失败返回 `failed to write to stdin`。错误都作为工具错误结果返回，不挂起工具。

### 4.4 返回给模型的文本

```
Chunk ID: 3fa2c1
Wall time: 10.0021 seconds
Process running with session ID 48213
Original token count: 1834
Output:
...
```

- 进程已退出时第三行为 `Process exited with code N`；被信号终止时无退出码行。
- Chunk ID 为 6 位随机十六进制。token 数按字节数 ÷ 4 估算。
- 输出超过预算时保留开头和结尾，中间替换为 `…N tokens truncated…`，正文前加一行 `Warning: truncated output (original token count: N)`。缓冲区本身溢出时插入 `... N bytes omitted ...`。
- 发生截断时，文本末尾追加 `Full output: <path>`。本次输出没有丢字节时，写入一个只含本次输出的保留临时文件（与 `OutputAccumulator` 相同的 `tempfile` 做法）；缓冲区已丢过字节时，指向该进程的 transcript 落盘文件（见 4.5），内容是进程至今的全部输出。后台退出记录同样优先使用落盘文件。

### 4.5 进程启动

| 项 | tty | 非 tty |
|---|---|---|
| 实现 | `portable-pty` | tokio `Command` |
| 会话 | PTY 子进程自带新会话与控制终端 | `pre_exec` 中 `setsid`，`EPERM` 时退回 `setpgid(0,0)`；Linux 另设 `PR_SET_PDEATHSIG(SIGTERM)` |
| stdin | 可写 | `Stdio::null()` |
| 输出 | PTY 主端一路输出 | stdout、stderr 按到达顺序合并 |
| 终止 | 对进程组 `killpg(SIGKILL)` | 同左 |
| Windows | ConPTY，尽力支持 | `taskkill /F /T` |

每个进程有 pending、transcript 两份头尾保留缓冲，各 1 MiB。transcript 第一次要丢字节之前，把当前保留的全部内容写入一个保留的临时文件，之后每块输出都追加进去，所以文件不缺任何字节。文件上限为缓冲的 64 倍（64 MiB），到达上限时写入 `... output past N bytes was not kept ...` 并停止写入。codex 没有这个文件，超出缓冲的中段直接丢弃。

### 4.6 环境变量

按顺序叠加：

1. 继承父进程环境。
2. 按 env policy 过滤（配置见第 10 节），顺序与 codex 相同：
   1. `inherit`：`all` 全部继承；`core` 只留 `PATH`、`SHELL`、`TMPDIR`、`TEMP`、`TMP`、`HOME`、`LANG`、`LC_ALL`、`LC_CTYPE`、`LOGNAME`、`USER`；`none` 不继承。
   2. `ignore_default_excludes = false` 时，去掉名字匹配 `*KEY*`、`*SECRET*`、`*TOKEN*` 的变量。默认为 `true`（不去掉），与 codex 一致。
   3. 去掉匹配 `exclude` 的变量。
   4. 应用 `set`。
   5. `include_only` 非空时，只留匹配的变量。

   模式只支持 `*` 通配，不区分大小写。
3. 强制覆盖：`NO_COLOR=1`、`TERM=dumb`、`LANG=C.UTF-8`、`LC_CTYPE=C.UTF-8`、`LC_ALL=C.UTF-8`、`COLORTERM=`、`PAGER=cat`、`GIT_PAGER=cat`、`GH_PAGER=cat`、`CLAWCODE_CI=1`。
4. 现有的 session ID、turn ID 环境变量（`ProductIdentity::SESSION_ID_ENV`、`TURN_ID_ENV`）。

## 5. 进程表与生命周期

### 5.1 进程表

`ExecProcessManager` 持有锁保护的进程表（编号 → 记录）与已分配编号集合。记录包含：进程句柄与输出缓冲、发起它的工具调用 ID 与所在 turn id、命令、cwd、是否 tty、最近使用时间、“首次 `exec_command` 仍在等待”标记。

### 5.2 淘汰

- 表中已有 64 个时，放入新进程前淘汰一个；被淘汰的进程在锁外终止，并发出 `ExecExited`。
- 最近使用的 8 个不淘汰；其余按最近使用时间从旧到新，优先已退出的。
- 交互锁被占用的进程跳过；全部被占用时允许暂时超过 64 个。

### 5.3 生命周期

| 场景 | 行为 |
|---|---|
| turn 结束 | 保留 |
| Esc / 中断 | 保留，包括首次等待中被打断的进程 |
| 进程在首次等待内退出 | 由 `exec_command` 的结果报告退出，不发 `ExecExited`、不写 `exec_exit` |
| 进程在后台退出 | 退出监视任务等输出读完后发 `ExecExited`；记录保留到被模型取走 |
| `/ps` | 列出运行中的进程：编号、命令、cwd；已退出待取结果的不列 |
| `/stop`、`/clean` | 全部终止并清空，回复 `Stopped N background terminals` |
| ACP 扩展方法 | 与上面同一逻辑（6.4） |
| `close_session`、`delete_session` | `acquire_close()` 返回后（不再有 run 能启动新进程）全部终止并删除 snapshot 文件；子会话随级联关闭各自清理 |
| 子会话 | 独立进程表 |
| fork 出的会话 | 进程表为空；历史中的旧编号返回 `Unknown process id` |
| 应用正常退出 | `main` 在 transport 停止后调用 `kernel.terminate_exec_processes()`，遍历活着的会话逐个终止；`ExecProcessManager` 被释放时也全部终止，作为兜底 |
| 服务重启、从存储打开会话 | 进程表为空；旧编号返回 `Unknown process id`；`<command_sessions>` 重新声明为空（第 8 节）；遗留卡片补 `lost` 记录（6.2） |

### 5.4 应用崩溃

- tty 进程：PTY 主端关闭后，内核向前台进程组发 SIGHUP，通常随之退出。
- Linux 非 tty 进程：`PDEATHSIG` 使其退出。
- macOS 非 tty 进程：已脱离终端，若不写 stdout 就不会因管道断开退出，会残留为孤儿进程（codex 相同）。代码中以 `shortcut:` 注释写明，需要时再引入 pid 记录文件并在启动时清理。

## 6. 事件、持久化与 UI

### 6.1 protocol

`ToolResultDetails::Exec`：

| 字段 | 说明 |
|---|---|
| `process_id: Option<i32>` | 有值表示转入后台 |
| `exit_code: Option<i32>`、`signal: Option<String>` | 进程已退出时的状态 |
| `chunk_id`、`wall_time_ms`、`original_token_count` | 与模型文本一致 |
| `full_output_path` | 截断时的完整输出文件 |
| `terminal_call_id` | 进程最初由哪次 `exec_command` 启动；`exec_command` 结果中等于自身调用 ID |

新增事件，turn id 使用启动该进程的工具调用所在 turn：

| 事件 | 字段 | 说明 |
|---|---|---|
| `ExecOutput` | `tool_call_id`（启动进程的调用）、`process_id`、`output`（transcript 尾部，最多 2000 行或 50 KB）、`truncation` | 进程整个生命周期内由流式任务每 100 ms 至多发一次；首次等待期间也走这个事件，`exec_command` 不发 `ToolExecutionUpdate` |
| `ExecExited` | `exit: ExecExit`，其中 `tool_call_id`、`turn_id`、`process_id`、`exit_code`、`signal`、`output`（最终尾部）、`truncation`、`full_output_path`、`wall_time_ms`、`lost: bool` | 只针对没有被首次 `exec_command` 报告退出的进程（转入后台，或首次等待被取消）：正常退出、被 `/stop` 或扩展方法终止、被淘汰时各发一次 |

### 6.2 持久化

- `ExecExited` 写为 `custom` 条目：`extensionId:"clawcode"`、`customType:"exec_exit"`，内容同事件字段。不进入模型上下文，只用于重放时给卡片收尾。
- `ExecOutput` 不持久化。
- 打开会话时，Kernel 扫描当前分支：结果中 `process_id` 有值、之后没有对应 `exec_exit` 的调用，各补一条 `lost: true` 的 `exec_exit` 条目。

### 6.3 ACP 映射

terminal id 为启动进程的 `exec_command` 调用 ID。

| 输入 | 输出 |
|---|---|
| `ToolExecutionStart(exec_command)` | 卡片 kind 为 Execute，内容为 terminal；`TerminalUpdate` 带 cwd 与 `cmd` |
| `ExecOutput` | `TerminalUpdate`，整段替换输出 |
| `exec_command` 结果，已退出 | terminal 写入结果文本中 `Output:` 行之后的正文并带 exit status 收尾；卡片状态：退出码 0 为 `completed`，否则 `failed` |
| `exec_command` 结果，转入后台 | 不设置状态（保持 `ToolExecutionStart` 的 `in_progress`），不写 terminal；`raw_output` 带 `details.process_id` |
| `ExecExited` | terminal 写最终输出并带 exit status 收尾；卡片状态规则同上，`lost` 为 `failed` 并附文本“进程已不在本会话中运行（服务重启或会话分叉）” |
| `write_stdin` | 卡片 kind 为 Execute，内容为“向 session N 发送：`<chars>`”或“等待 session N 输出”；`raw_output` 保留完整结果；不重复显示输出 |

**谁给卡片收尾**：每个进程恰好由一方收尾。在首次等待内退出的，由 `exec_command` 的结果收尾，这类进程不发 `ExecExited`，会话文件里也不会把输出存第二份；其余进程由 `ExecExited` 收尾。

**顺序无关**：后台进程可能在 yield 截止后、工具结果发出前退出，`ExecExited` 因此可能先于“转入后台”的结果到达；重放时 `exec_exit` 条目也可能排在工具结果消息之前。“转入后台”的结果既不设置状态也不写 terminal，所以两者无论谁先到，卡片最终状态都一样。

`acp/src/tool_call.rs` 的 kind 映射、`acp/src/projection.rs` 中对 bash 结果的处理同步改为 exec。

### 6.4 ACP 扩展方法

| 方法 | 参数 | 返回 | 类别 |
|---|---|---|---|
| `_clawcode/session/exec/list` | `sessionId` | 运行中的进程：`processId`、`command`、`cwd`、`toolCallId` | 只读查询 |
| `_clawcode/session/exec/terminate` | `sessionId`、`processId` | `terminated: bool` | 变更 |
| `_clawcode/session/exec/clean` | `sessionId` | `terminated: usize` | 变更 |

### 6.5 WebUI

- `ToolCallCard` 以 `exec_command` 替换对 `bash` 的判断；turn 结束后卡片继续接收 `TerminalUpdate` 与状态更新，直到收尾。
- 输入框附近显示“N 个后台终端”。数量为 terminal 已打开、且还没有 exit status 的 `exec_command` 卡片数。这样首次等待被 Esc 打断、卡片显示 `cancelled` 但进程仍在运行的情况也会计入。点开时调用 `exec/list` 取得各进程的编号与命令，可单个终止（`terminate`）或全部停止（`clean`）。

## 7. Plan 门控与名字迁移

- `gate.rs` 中按名字的判断移到 `kernel_owned` 放行之前：
  - `exec_command`：`cmd` 走 `ShellCommandLine::read_only`。
  - `write_stdin`：只放行 `chars` 为空或 `\x03`，否则拒绝：`Plan mode is active: write_stdin may only poll or interrupt.`
- `prompt/system.rs`：fallback 改为 `Use exec_command for file operations like ls, rg, find`。
- `command_guard` 扩展：改为识别 `exec_command` 的 `cmd` 参数；同时对 `write_stdin` 的 `chars` 逐行套用同样的检查，防止先用 `exec_command` 开一个交互 shell、再通过 `write_stdin` 发送危险命令来绕过它。
- ACP、WebUI 中读取 `command` 参数的位置改读 `cmd`。

## 8. `<command_sessions>` 注入

- 每次向模型发请求前，计算当前进程表快照，与当前上下文中最后一条 `command_sessions` 消息比较；不同则追加一条。上下文中没有这类消息时视为上一次快照为空，所以从未启动过后台进程的会话不会出现这条消息。
- 快照写为 `custom` 条目（`extensionId:"clawcode"`、`customType:"command_sessions"`，`data` 为快照内容），同时在上下文中放一条隐藏的 `MessageContent::Extension` 说明消息。放置逻辑沿用 plan 说明消息（有未回结果的工具调用时排在结果之后）；重放时由条目重建说明消息，与 `session_mode` 的做法相同，因此得到相同上下文。只在变化时追加，不破坏 prompt 前缀缓存。
- 正文用 codex 原文：

  ```
  Current command sessions for this thread (supersedes earlier snapshots). Exited sessions retain results for write_stdin. In-use sessions are temporarily busy with a tool interaction or result finalization.
  48213: running
  50122: exited; poll to collect
  51007: running; in use
  ```

  没有进程时为 `None.`；最多列 16 个，超出时追加 `N additional sessions omitted; this snapshot is incomplete.`。首次 `exec_command` 仍在等待的进程不列出。
- 压缩后，若当前表非空，按 `ModeNote::after_compaction` 的方式重新声明一次。
- 服务重启或 fork 后表为空，与历史中最后一条不同，下一次请求自动追加 `None.`。
- 配置 `command_sessions = false` 时不追加。

## 9. Shell snapshot

去掉 codex 中的凭据代理与沙箱部分。

- **生成**：会话的首次 `exec_command` 触发后台生成，限时 10 秒。只打开会话查看历史、或从不执行命令的会话（包括子代理会话）不会启动 shell。生成完成前的命令照常用 `-lc`；失败记一条 `warn`，该会话不再使用 snapshot。
- **支持**：bash、zsh、sh；Windows 不生成。
- **采集**：以 login + interactive 方式运行采集脚本，记录函数（`declare -f` / `functions`）、alias（`alias -p` / `alias -L`）、选项（`shopt -p`、`set +o` / `setopt`）与导出变量（`export -p`），写入 `<data_root>/shell_snapshots/<session_id>.sh`，再以 `. <文件>` 校验一次。`data_root` 与 `sessions/`、`credentials/` 同级（`crates/app/src/lib.rs`：`session_persistence.data_home`，未配置时为 `dirs::data_local_dir()/clawcode`），由 app 组装时传给 Kernel。
- **使用**：`login = true` 的命令改写为

  ```
  <shell> -c '. <snapshot>; <再导出强制覆盖变量与 session/turn ID>; exec <shell> -c <cmd>'
  ```

  快照中的变量不会覆盖第 4.6 节第 3、4 步的变量。
- **清理**：会话关闭时删除；应用启动时删除超过 3 天的文件。
- 配置 `shell_snapshot = false` 时不生成。

## 10. 配置

新增 `[tools.exec]`：

| 键 | 默认值 | 说明 |
|---|---|---|
| `background_terminal_max_timeout_ms` | `300000` | `write_stdin` 空轮询的等待上限 |
| `shell_snapshot` | `true` | 第 9 节 |
| `command_sessions` | `true` | 第 8 节 |
| `env.inherit` | `"all"` | `all` / `core` / `none` |
| `env.ignore_default_excludes` | `true` | 为 `false` 时去掉名字含 KEY/SECRET/TOKEN 的变量 |
| `env.exclude`、`env.include_only` | `[]` | 变量名模式 |
| `env.set` | `{}` | 追加或覆盖的变量 |

`enable_shell` 继续控制是否注册 exec 工具。

## 11. 日志

按 CLAUDE.md 第 13、14 条，用 `tracing::info!("...", ...)` 正文格式，不用结构化字段。

- `info`：进程启动（编号、命令前 80 个字符、是否 tty）、转入后台、退出（编号、退出码、运行时长）、被淘汰、`/stop` 与扩展方法终止、会话关闭时终止的数量、snapshot 生成完成。
- `warn`：所有错误返回前、snapshot 生成失败、终止失败。
- 不记录命令输出、`chars` 内容与环境变量。

## 12. 测试与验收

测试只放在 crate 的 `tests/` 目录或 `#[cfg(test)] mod tests` 中（CLAUDE.md 第 10 条）。

| 位置 | 覆盖 |
|---|---|
| `crates/tools/tests/exec_{output,env,process,manager,snapshot}.rs` | 管道与 PTY 启动；yield 与退出码；轮询；向 tty 进程（`cat`）写入；非 tty 下 `\x03` 中断；stdin 已关闭错误；头尾截断与完整输出文件；env policy 与强制覆盖；`read </dev/tty` 立即失败（验证 setsid）；终止进程组时孙进程一同退出；淘汰规则；全部终止；snapshot 生成与命令改写 |
| `crates/kernel/tests` | 转入后台 → 轮询 → 退出；Esc 后进程保留；`close_session` 清理；子会话隔离；`/ps`、`/stop`；plan 门控；`command_sessions` 的放置、重放与压缩后重新声明；`exec_exit` 条目；重开会话后补 `lost` 条目；保留名 |
| `crates/acp/tests` | terminal 打开、更新、收尾；转入后台时 `in_progress`；`ExecExited` 收尾；`write_stdin` 卡片；三个扩展方法 |
| `extensions/tests` | `command_guard` 识别 `exec_command` 的 `cmd` 与 `write_stdin` 的 `chars` |
| WebUI e2e（生产后端，CLAUDE.md 第 9 条） | 后台命令的卡片在 turn 结束后继续刷新；后台终端计数；单个终止与全部停止 |

删除或改写依赖 `bash` 工具与 `ToolResultDetails::Bash` 的现有测试：`crates/tools/tests/bash.rs`（仅保留 `BashExecutor` 部分）、`crates/acp/tests/mapping.rs`、`crates/kernel/tests/extensions_host.rs`、`crates/kernel/tests/protocol_alignment/queue.rs`。

## 13. 不在范围

- 沙箱与权限：`sandbox_permissions`、`additional_permissions`、沙箱拒绝后重试、网络代理与网络审批、stdin 审核。
- 远程 exec-server、插件统计与归因。
- `awaiter` 子代理角色（clawcode 子代理没有角色定义机制）。
- 审批弹窗期间暂停等待计时（clawcode 工具执行中不会等待用户审批）。
- 历史会话中 `bash` 工具结果的迁移与兼容。
- 用户 `!` 命令接入进程表。

## 14. 风险与已知限制

- **run 外并发发事件**（已核实）：`Kernel::observer_emitter`（`crates/kernel/src/runtime.rs`）在有活动 run 时用它的 sink，否则用会话 sink；序号来自 `SessionExecution.sequence` 原子计数。会话改名的通知已经这样在 run 外发事件，exec 事件沿用。
- **关闭时的退出记录**：`terminate_all` 终止进程后，最多等待 2 秒让各进程的退出监视任务写完 `exec_exit` 并发出 `ExecExited`，再继续关闭流程；超时的只记 `warn`，重开时由 `lost` 补记录。
- **run 中写 `custom` 条目**（已核实）：`SessionTranscript::writable()` 只检查会话是否已 sealed、之前是否写失败，与 run 状态无关，也不涉及 `WriteDeferred`（那是恢复用的 lane 记录）。条目即使排在助手工具调用和工具结果之间，`history_from_store` 也会跳过未识别的 `custom` 条目，不影响上下文。`append_store_only_entry` 目前是 `pub(super)`，需放宽为 `pub(in crate::runtime)`。
- **会话内切换分支**：另一分支上的后台卡片要等进程退出、实时收到 `ExecExited` 才会收尾；重开会话时也可能被补成 `lost`。属于边缘场景，不处理。
- **macOS 孤儿进程**：见 5.4。
- **Windows**：PTY 与中断尽力支持，测试以 macOS、Linux 为准。
- **扩展覆盖**：`bash` 原可被扩展覆盖，`exec_command`、`write_stdin` 为保留名后不再可以。
