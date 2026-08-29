# Codex 风格 PTY 终端加固实施计划

> **执行要求：** REQUIRED SUB-SKILL：使用 `superpowers:executing-plans` 在当前会话逐任务执行。项目禁止 SubAgent 和 worktree，因此直接在当前 `dev` 工作区保留未提交检查点。

**目标：** 修复终端进程所有权、关闭清理、ACP 背压、Web 状态竞争、参数边界和 UI 错误处理问题，使已实现的 Codex 风格终端在异常与并发场景下仍可可靠回收和重新同步。

**架构：** `TerminalManager` 串行化会改变进程表的操作，只有在后端确认停止后才移除条目；永久 shutdown 先关闭 spawn 入口，再清理所有 retained process。ACP 不再转发高频输出块，只发送带单调 revision 的状态失效通知；Web 以 revision 驱动单航班重取快照并丢弃过期响应。应用入口无论 transport 如何退出都执行 Kernel shutdown。

**技术栈：** Rust 2024、Tokio、portable-pty、ACP v2 JSON-RPC、React 19、TypeScript 6、Zustand。

**Spec：** `docs/superpowers/specs/2026-08-29-codex-style-pty-terminal-design.md`

## 全局约束

- 所有 shell 命令以 `rtk` 开头；命令链中的每个子命令也单独使用 `rtk`。
- 禁止 SubAgent 和 worktree；未经用户再次明确允许不创建 commit。
- Rust 行为修改使用 TDD：先运行新增测试并观察预期失败，再写最小实现。
- 新增函数使用英文函数级注释，非平凡逻辑和旧逻辑变动原因使用英文注释。
- 字段超过 3 个的 struct 使用 `typed-builder`；`Arc<T>` 字段使用 `Arc::clone`。
- WebUI E2E 连接生产 `app` 和当前真实 provider，不使用 fixture backend。

---

### 任务 1：保证 manager 持有进程直到终止确认

**文件：**

- 修改：`crates/tools/src/terminal/manager.rs`
- 修改：`crates/tools/src/terminal/process.rs`
- 修改：`crates/tools/src/terminal/backend.rs`
- 修改：`crates/tools/tests/terminal_tools.rs`

**接口：**

- `ManagerState` 维护是否接受新 spawn 的状态和单调 revision。
- `TerminalManager::terminate`、容量回收和批量清理先克隆当前 `Arc<TerminalProcess>`，后端终止成功后再以 `Arc::ptr_eq` 条件移除。
- `TerminalManager::shutdown` 永久关闭 spawn；`terminate_all` 仍用于可恢复的 Session 内 clean。

- [ ] **步骤 1：写失败测试**

在 manager 的准确 `#[cfg(test)] mod tests` 或 crate 集成测试中加入受控后端，验证：第一次 terminate 失败后条目仍可 list 且第二次可重试；shutdown 与挂起 spawn 串行时不会留下晚插入进程；shutdown 后 spawn 被拒绝；容量回收失败不会丢失 victim。

- [ ] **步骤 2：观察 RED**

运行：`rtk cargo test -p tools terminal_manager_ -- --nocapture`

预期：旧实现会在 terminate 失败后返回空列表，或在 shutdown 竞态后留下/接受新进程。

- [ ] **步骤 3：实现最小所有权修复**

所有表变更操作持有 `operations` 异步锁；终止 I/O 期间不持有同步 state 锁。成功后重新锁表，仅当 ID 仍指向同一个 `Arc` 时移除并增加 revision；失败时保留条目。shutdown 在同一操作锁内先设置 `accepting_spawns = false`，再逐个终止并只移除已确认停止的条目。

- [ ] **步骤 4：验证 GREEN**

运行：`rtk cargo test -p tools terminal_manager_ -- --nocapture && rtk cargo test -p tools`

预期：新增测试和 tools crate 全部通过。

---

### 任务 2：让后端资源析构具备兜底回收语义

**文件：**

- 修改：`crates/tools/src/terminal/backend/pipe.rs`
- 修改：`crates/tools/src/terminal/backend/pty.rs`
- 修改：对应 `#[cfg(test)] mod tests` 或 `crates/tools/tests/terminal_tools.rs`

**接口：**

- Unix pipe 的进程组拥有者在 Drop 时发送 kill；PTY 控制对象在 Drop 时关闭/终止 slave 进程。
- Windows Job Object 继续使用 kill-on-close；Drop 不等待异步确认，只作为 manager 显式终止之外的最后防线。

- [ ] **步骤 1：写失败测试并观察 RED**

启动会创建子进程的真实 pipe/PTY，丢弃最后一个 manager/process owner 后按条件轮询 OS 状态，断言父子进程均退出。运行：`rtk cargo test -p tools terminal_drop_ -- --nocapture`。

- [ ] **步骤 2：实现 RAII guard**

将进程组、PTY terminator 或 Job handle 封装为拥有资源的后端字段；Drop 只执行幂等 best-effort kill，并在失败时写含 terminal 生命周期上下文但不含命令正文的警告日志。

- [ ] **步骤 3：验证 GREEN**

运行：`rtk cargo test -p tools terminal_drop_ -- --nocapture && rtk cargo test -p tools`。

---

### 任务 3：统一 Kernel 与应用 transport 关闭路径

**文件：**

- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/terminal.rs`
- 修改：`crates/app/src/main.rs`
- 修改或新增：`crates/kernel/tests/` 下对应集成测试

**接口：**

- Kernel shutdown 阻止新的 Session 操作、取消活动 run，并调用每个 Session 的永久 `TerminalManager::shutdown`。
- `app::main` 保存 stdio/HTTP transport 结果，始终执行 cleanup，再按“transport 原始错误优先”的规则返回错误。

- [ ] **步骤 1：写失败测试并观察 RED**

验证 Kernel shutdown 后 terminal spawn 被拒绝、多个 Session 即使一个清理失败也继续清理其余 Session。运行：`rtk cargo test -p kernel terminal_shutdown_ -- --nocapture`。

- [ ] **步骤 2：实现统一关闭**

删除 transport 分支中绕过 cleanup 的 `?`；将运行结果和 cleanup 结果显式合并。正常 Ctrl-C、stdio EOF、HTTP serve 错误都进入同一 shutdown 路径。

- [ ] **步骤 3：验证 GREEN**

运行：`rtk cargo test -p kernel terminal_shutdown_ -- --nocapture && rtk cargo test -p kernel -p app`。

---

### 任务 4：以 revision 状态失效通知替代 ACP 原始输出流

**文件：**

- 修改：`crates/protocol/src/terminal.rs`
- 修改：`crates/protocol/tests/terminal.rs`
- 修改：`crates/tools/src/terminal/manager.rs`
- 修改：`crates/tools/src/terminal/process.rs`
- 修改：`crates/acp/src/terminal.rs`
- 修改：`crates/acp/src/server.rs`
- 修改：`crates/acp/tests/extension.rs`

**接口：**

- `TerminalListResult { revision, terminals }`。
- `TerminalUpdateNotification { session_id, revision }` 只表示宿主快照失效，不携带输出正文。
- lifecycle/start/remove/可观察状态变化增加 revision；高频 output 只写 1 MiB ring 并唤醒 Agent，不进入 ACP client queue。

- [ ] **步骤 1：写协议和 ACP 失败测试**

测试 list 带 revision、通知只包含 `sessionId` 和 `revision`、大量 output 不产生 ACP 通知、生命周期变化产生更高 revision、广播 lag 后下一条 revision 仍可触发重新同步。

- [ ] **步骤 2：观察 RED**

运行：`rtk cargo test -p protocol --test terminal && rtk cargo test -p acp terminal_ -- --nocapture`。

- [ ] **步骤 3：实现最小 revision 协议**

由 manager 在同步 state 临界区分配 revision，并广播 revision 值；ACP watcher 将每次变化映射为轻量 invalidation。遇到 `Lagged` 时不尝试重放事件，只等待/发送当前 manager revision 以驱动客户端 list。

- [ ] **步骤 4：验证 GREEN**

运行：`rtk cargo test -p protocol --test terminal && rtk cargo test -p acp`。

---

### 任务 5：Web 使用严格解码和 revision 单航班同步

**文件：**

- 修改：`web/src/domain/model.ts`
- 修改：`web/src/workspace/sessionState.ts`
- 修改：`web/src/workspace/controller.ts`

**接口：**

- `TerminalSnapshot.exitCode?: number | null`，list 与 update 均有安全整数 revision。
- 所有 ACP terminal payload 经过运行时 decoder；非法 payload 只进入 diagnostics。
- 每个 Session 最多一个 list 请求在途；请求返回 revision 小于已知目标 revision 时丢弃并继续刷新。

- [ ] **步骤 1：实现运行时边界**

使用显式 `unknown` 类型守卫验证 terminal ID 范围、字符串、布尔值、status、时间戳、revision 和 nullable exitCode；移除 `as TerminalUpdateNotification`。

- [ ] **步骤 2：实现无竞争刷新**

在 Session state 保存 `terminalRevision`；notification 只提高目标 revision，controller 的 per-session in-flight promise 在循环中 list 到 revision 追平，reducer 拒绝旧快照。

- [ ] **步骤 3：验证 Web 静态检查**

运行：`rtk npm --prefix web run check && rtk npm --prefix web run build`。

---

### 任务 6：修复 `write_stdin` 等待边界和面板交互

**文件：**

- 修改：`crates/tools/src/builtin/write_stdin.rs`
- 修改：`crates/tools/tests/terminal_tools.rs`
- 修改：`web/src/features/inspector/TerminalPanel.tsx`
- 修改：`web/src/theme/workbench.css`

**接口：**

- 非空写入接受 `250..=30_000ms`；空轮询默认至少 5,000ms 并接受到 300,000ms；范围外参数被拒绝，JSON Schema 最大值为 300,000。
- 面板在 terminate/clean 期间禁用冲突操作，捕获错误写入 diagnostics，只对 running terminal 显示 terminate，并正确显示 `exit null` 与最近活动时间。

- [ ] **步骤 1：写 Rust 边界失败测试并观察 RED**

用 recording `TerminalService` 断言空轮询 300,000ms 不被压到 30,000ms，而非空写入仍为 30,000ms。运行：`rtk cargo test -p tools --test terminal_tools write_stdin_`。

- [ ] **步骤 2：实现等待上下文规则并验证 GREEN**

解析 `chars` 后按是否为空选择默认值和有效上限，空轮询将低于 5,000ms 的有效请求提升到 5,000ms，Schema 对外允许 300,000。运行：`rtk cargo test -p tools --test terminal_tools write_stdin_ && rtk cargo test -p tools`。

- [ ] **步骤 3：实现面板状态和错误反馈**

组件维护 busy terminal/clean 状态，所有 promise 使用 `try/catch/finally`，失败调用 controller diagnostics 入口；按钮添加 disabled/aria 状态，不允许重复提交。

- [ ] **步骤 4：验证 Web**

运行：`rtk npm --prefix web run check && rtk npm --prefix web run build`。

---

### 任务 7：全量验证与真实生产 E2E

**文件：**

- 不新增 fixture；仅在发现真实缺陷时修改上述生产代码与既有测试。

- [ ] **步骤 1：运行完整静态与自动测试门禁**

运行：

```bash
rtk cargo fmt --check
rtk cargo test --workspace
rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
rtk npm --prefix web run check
rtk npm --prefix web run build
rtk git diff --check
```

当前 `web/package.json` 没有测试脚本，因此前端门禁以 TypeScript、ESLint 和生产构建为准。

- [ ] **步骤 2：启动生产 app 与真实 provider**

使用项目当前配置启动 production `app` 和 WebUI；记录 provider/model，不能替换为 fixture 或 mock。

- [ ] **步骤 3：运行真实浏览器验收**

覆盖真实 PTY 的 TTY 检测、交互输入、跨 Turn 访问、pipe、workdir、Web 列表/终止/clean、刷新重新同步、应用退出后的进程树清理，并检查终端操作失败会产生可见 diagnostics。

- [ ] **步骤 4：确认无遗留进程并复审 diff**

通过显式 PID/命令标记检查测试进程和 app 已退出；运行 `rtk git diff --check && rtk git status --short`。不 stage、不 commit。
