# TerminalBackend 抽象层设计方案

## 动机

clawcode 已为文件系统（FS）实现了 `FsBackend` trait 抽象（`LocalFsBackend` + `AcpFsBackend`），允许 FS 工具（read/write/edit）在本地执行和委托 ACP 客户端执行之间平滑切换。但 shell 命令执行仍直接硬编码 `tokio::process::Command`，缺少同等的抽象层。

这导致两个问题：

1. **ACP 模式下 shell 只能在 agent 本地执行**：即使 ACP 协议已定义完整的 `terminal/*` 客户端能力（create/output/wait_for_exit/kill/release），`ShellCommand` 也无法利用它们。对于远程 ACP 客户端场景，shell 应该运行在客户端机器上，而非 agent 机器上。
2. **架构不对称**：FS 已有 backend 抽象而 shell 没有，导致两条工具路径不一致。引入 `TerminalBackend` 后，所有需要"在某个环境下执行操作"的工具都有一致的 backend 模式。

## 核心设计

### 两阶段 API

```
TerminalBackend::create(params)
  │
  └─ 返回 Box<dyn RunningTerminal>
       │
       ├─ .output()      → TerminalOutputSnapshot（非阻塞快照）
       ├─ .wait_for_exit() → TerminalExitResult（阻塞等待）
       ├─ .kill()        → 终止进程
       └─ drop           → 自动 release（清理资源）
```

与 `FsBackend` 的一次性 API（read/write 直接返回结果）不同，`TerminalBackend` 是两阶段的：`create()` 返回一个长期存活的 handle，调用者通过 handle 轮询输出和控制生命周期。

### 实现层次

```
TerminalBackend trait (crates/tools/src/terminal_backend.rs)
├── LocalTerminalBackend
│   └── LocalRunningTerminal  — 内部持有 tokio::process::Child + pipe reader tasks
│       - stdout/stderr 分离到独立 buffer
│       - output() 返回累积快照
│       - wait_for_exit() 轮询直到退出
│       - 依赖 tokio::sync::Mutex 保护共享状态
│
└── AcpTerminalBackend (crates/acp/src/terminal_backend.rs)
    └── AcpRunningTerminal   — 内部持有 ACP TerminalId
        - output()           → terminal/output ACP 请求
        - wait_for_exit()    → terminal/wait_for_exit ACP 请求
        - kill()             → terminal/kill ACP 请求
        - drop               → terminal/release（fire-and-forget）
        - stdout/stderr 合并（ACP 协议不区分）
```

### 与 FsBackend 的对称性

| 维度 | FsBackend | TerminalBackend |
|------|-----------|-----------------|
| 定义位置 | `crates/tools/src/fs_backend.rs` | `crates/tools/src/terminal_backend.rs` |
| trait 方法 | `read_text_file()`, `write_text_file()` | `create()` 返回 `RunningTerminal` |
| 本地实现 | `LocalFsBackend` (直接 tokio::fs) | `LocalTerminalBackend` (直接 tokio::process) |
| ACP 实现 | `AcpFsBackend` (fs/read_text_file, fs/write_text_file) | `AcpTerminalBackend` (terminal/create, terminal/output, ...) |
| 路由器 | `AcpClientFsRouter` | `AcpClientTerminalRouter`（独立实例） |
| 能力检查 | `capabilities.fs.read_text_file` | `capabilities.terminal` |
| 构造方式 | `with_backend(backend)` | `with_backend(backend)` |
| 注册方法 | `register_fs_tools_with_backend` | `register_builtins_with_backends`（term + fs 一起） |

### ShellCommand 的适配

重构前：
```
ShellCommand (单元结构体)
  └─ execute_streaming()
       └─ 直接 tokio::process::Command::new("/bin/sh")
              ├─ spawn 子进程
              ├─ 读 stdout/stderr pipe → Delta 事件
              └─ 进程退出 → End 事件
```

重构后：
```
ShellCommand { backend: Arc<dyn TerminalBackend> }
  └─ execute_streaming()
       └─ backend.create(params) → Box<dyn RunningTerminal>
            └─ 轮询 handle.output() → 对增量内容发送 Delta
                 └─ handle.wait_for_exit() → End 事件
```

轮询间隔 100ms。`ShellCommand` 自身不再包含进程管理逻辑——它只负责：
1. 解析工具参数（command, cwd）
2. 构建 `TerminalCreateParams`
3. 管理流式事件生命周期（Begin/Delta/End）
4. 截断输出（`OUTPUT_MAX_LEN`）

### stdout/stderr 处理差异

| Backend | stdout | stderr | 原因 |
|---------|--------|--------|------|
| `LocalTerminalBackend` | 分离 | 分离 | 本地 pipe 天然分离，产生 `ExecOutputStream::Stdout` / `Stderr` Delta |
| `AcpTerminalBackend` | 合并（放入 stdout） | 空字符串 | ACP `TerminalOutputResponse.output` 是合并字符串 |

选择不新增 `ExecOutputStream::Combined` 变体，因为 ACP 客户端实际展示时也不区分 stdout/stderr。

### 路由器设计

`AcpClientTerminalRouter` 与 `AcpClientFsRouter` 结构完全一致，但独立实例。不复用同一路由器是因为：
- 两个 backend 的协议方法不同（`fs/*` vs `terminal/*`）
- 客户端能力检查字段不同（`fs.read_text_file` vs `terminal`）
- 独立实例避免单一 router 成为耦合点

### 向后兼容

- `ShellCommand::new()` 默认使用 `LocalTerminalBackend`，行为不变
- `register_builtins_with_fs_backend()` 保留，内部默认 `LocalTerminalBackend`
- `ClawcodeAgent::new()` 和 `with_fs_router()` 保留，内部默认空 `AcpClientTerminalRouter`
- `acp::run()` 和 `run_with_fs_router()` 保留

---

## 类型定义

```rust
pub struct TerminalCreateParams {
    pub session_id: SessionId,
    pub command: String,            // "/bin/sh"
    pub args: Vec<String>,          // ["-c", "user command"]
    pub env: Vec<TerminalEnvVariable>, // 环境变量
    pub cwd: PathBuf,               // 必传，ACP 侧始终 Some
    pub output_byte_limit: Option<u64>,  // 输出截断上限，暂不做实际支持
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,  // ACP 扩展元数据，暂不做实际支持
}

/// 环境变量（与 ACP EnvVariable 对齐，不含 meta）
pub struct TerminalEnvVariable {
    pub name: String,
    pub value: String,
}

pub struct TerminalOutputSnapshot {
    pub stdout: String,        // 自启动以来的全部 stdout（ACP 合并到此字段）
    pub stderr: String,        // 自启动以来的全部 stderr（ACP 为空）
    pub exit_status: Option<TerminalExitResult>,
}

pub struct TerminalExitResult {
    pub exit_code: i32,
}

pub enum TerminalBackendError {
    InvalidRequest(String),    // 能力不满足、session 无路由
    Io(String),                // 进程/ACP 传输失败
}
```

---

## 修改范围

| 文件 | 操作 | 说明 |
|------|------|------|
| `crates/tools/src/terminal_backend.rs` | 新建 | trait + 类型 + LocalTerminalBackend |
| `crates/tools/src/lib.rs` | 修改 | 导出新模块 |
| `crates/tools/src/builtin/shell.rs` | 修改 | 添加 backend 字段，重构 |
| `crates/tools/src/builtin/mod.rs` | 修改 | 新注册方法 |
| `crates/acp/src/terminal_backend.rs` | 新建 | AcpTerminalBackend + Router |
| `crates/acp/src/agent.rs` | 修改 | terminal_router 字段和 session 生命周期 |
| `crates/acp/src/lib.rs` | 修改 | 公开 terminal_backend 模块，新增 run_with_routers |
| `crates/acp/src/main.rs` | 修改 | 创建并传入 terminal backend |
| `crates/tui/src/acp/server/mod.rs` | 修改 | 同上 |

## 不在范围

- FS backend 功能增强（create_directory, get_metadata 等）
- ACP terminal `output_byte_limit` 参数支持
- 端到端 ACP 客户端 terminal 能力集成测试
