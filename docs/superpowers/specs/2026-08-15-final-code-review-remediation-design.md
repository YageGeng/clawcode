# 最终代码审查修复设计

## 目标

本轮完整修复最新代码审查确认的生命周期、内存、安全、协议状态、可观测性和模块边界问题，并同时将实验扩展实现从 `crates/extensions` 迁移到 workspace 根目录的 `extensions`。

修复不增加 Pi 不存在的新能力，不保留被删除 API 的兼容层，不修改 Provider 和 Config 的既有主体职责。

## Session 关闭状态与重入控制

Session 生命周期使用类型化状态表达 `Active`、`Closing` 和 `Closed`。关闭流程先在 `run_gate` 保护下进入 `Closing`，再执行 `session_shutdown`，最后使 Extension Runtime 失效并从 Kernel 移除 Session。

进入 `Closing` 后，Extension Host 拒绝所有会重新进入 Agent Run 或重新获取 `run_gate` 的能力，包括 `send_user_message`、`compact`、Fork 和 Tree 操作，并返回明确的 `SessionClosing` 错误。不会重新进入运行门禁的受控清理能力仍可在 shutdown hook 中使用。这样既保持关闭过程串行，也消除 shutdown hook 自锁。

新增集成测试，让 shutdown hook 尝试发送用户消息和压缩，验证请求立即返回类型化错误且 Session 最终关闭。

## 有界输出与工具快照合并

Bash stdout/stderr reader 改用有界 Tokio Channel。Reader 在缓冲区满时等待消费者，形成背压；现有可见输出限制和溢出文件继续负责结果大小，而不是依赖无限内存队列。

工具执行更新表达的是“当前最新快照”，因此不使用普通消息队列。新增类型化的更新发布器，以 Tokio `watch` 保存最新值；工具高频发布时覆盖尚未消费的旧快照，Kernel 仍保证最终快照和执行结束事件被处理。公共 Tool Context 不直接暴露 Channel 实现。

测试覆盖大体积 Bash 输出、高频工具更新、最终快照保留和执行完成，证明内存队列有界且不会丢失最终状态。

## Provider Header 安全边界

Extension Provider Hook 不再通过固定敏感 Header 黑名单构造可见请求。Provider 层引入明确的安全 Header 投影，只向扩展公开协议允许的非凭证 Header；认证、Cookie、Token、Key、签名和 Provider 私有凭证不进入 Hook Context。

Extension 仍可在 Hook 结果中增加或修改普通业务 Header，但不能读取已有凭证。Provider 在 Hook 完成后合并受保护凭证，扩展不能覆盖这些 Header。

测试覆盖标准认证头以及 `x-goog-api-key`、`x-auth-token` 和自定义凭证头，验证它们既不可见也不可被扩展替换。

## 动态 Tool 与 Pi API 对齐

动态 `register_tool` 成功后同时更新当前 Session 的可用工具和活动工具，新工具从下一次模型请求起可见。注册、活动集合更新和必要的 Session 状态持久化在同一 Kernel 操作中完成，避免只注册但不可用的中间状态。

删除 Pi Extension API 不存在的公开 `unregister_tool` 和 `unregister_command` 能力，不提供兼容包装。内部 Registry 只保留构造新快照和 Runtime 失效所需的移除能力，不向扩展暴露推测性生命周期 API。

集成测试覆盖运行期注册后模型工具列表立即包含新工具，以及 Session 之间的动态工具隔离。

## User Bash 类型化结果与 Guard

协议层为 User Bash 结果增加类型化 disposition，至少区分正常完成和被策略阻止。Shell 退出码只表达真实进程结果，WebUI 和 ACP 不再通过 `126` 推断策略状态。

`command-guard` 返回明确的 blocked disposition 和原因。WebUI 根据 disposition 渲染 blocked 状态，并保留真实退出码 `126` 的普通失败语义。

Guard 从原始字符串包含判断改为解析直接 Shell 命令段，只在命令可执行文件为 `rm` 或其路径形式、且参数实际同时启用递归和强制选项时阻止。它覆盖 `-rf`、`-fr`、`-r -f` 和 `/bin/rm -rf`，不会阻止 `echo 'rm -rf'`。该扩展仍不是完整 Shell 沙箱，文档明确其直接命令保护边界。

## Extension 诊断可观测性

Extension handler 的业务失败继续不终止 Agent 主流程，但诊断基础设施自身的锁、序列化、Store 和 Event Sink 错误不得静默丢弃。

诊断发送逻辑由一个有明确职责的 reporter 类型负责。每个失败路径使用 `tracing::warn` 或 `tracing::error` 记录 Session、Extension、Hook Point 和失败阶段，不记录 Prompt、Header 或工具敏感载荷。能够持久化时继续写入可回放的 Extension Error；不能持久化时至少留下服务端日志。

测试通过失败 Store 和失败 Sink 验证主流程继续运行，同时诊断失败可被捕获。

## Session 命令作用域

删除未使用且通过 `HashMap::values().next()` 随机选择 Session 的 `Kernel::extension_commands()`。需要查询命令的调用路径必须持有明确的 `SessionId`，并通过 Session Runtime 返回结果；锁或 Runtime 错误返回类型化错误，不伪装为空列表。

## Web 更新路由与类型驱动解码

`SessionUpdateRouter` 不再混用注入的 dispatch 和全局 Zustand Store。Router 依赖一个完整的 typed state access contract，其中同时包含 `get_state`、dispatch 和必要的异步刷新操作，所有读写指向同一 Store 实例。

ACP 传输形状解析从 Router 的大分支中移动到协议解码类型。Decoder 负责把未知 JSON 转换成穷举的领域更新枚举，Router 只根据领域更新和目标 Session 归约状态。拆分按“协议解码”和“状态应用”两项语义进行，不创建大量单调用 helper。

前端不新增单元测试；通过 TypeScript 检查、生产构建和真实后端 WebUI E2E 验证事件顺序、Thinking、Tool、User Bash blocked/exit 126 以及多 Session 隔离。

## Rust 构造规则

`HandlerRegistry`、`ToolBatchResult` 以及本轮触及的其他超过三个字段的结构体统一使用 `typed-builder`。仅用于内部归约且由宏生成的存储类型也必须提供一致的构造入口，避免绕过项目规则的结构体字面量散落在业务流程中。

删除单调用的短 helper；只有一个自定义类型参数的行为优先下沉为该类型的关联方法或标准 trait 实现。

## 实验扩展目录迁移

按照《实验扩展目录迁移设计》，只将 `crates/extensions` 移动为根目录 `extensions`。`crates/extension` 继续作为稳定扩展框架保留在核心 crates 中。

根 Cargo workspace member 和 workspace dependency 更新到新路径。构建脚本显式从新 manifest 目录解析 workspace 根目录，继续优先支持 `ProductIdentity::CONFIG_PATH_ENV`，默认读取根目录 `claw.toml`。crate 名称和 `extensions::compiled_extensions()` 入口保持不变。

## 实施顺序

1. 先迁移实验扩展目录并恢复 Cargo 构建基线；
2. 修复 Session 关闭状态和 Host 重入边界；
3. 修复 Bash 与 Tool Update 的有界数据流；
4. 修复 Provider Header 安全投影；
5. 对齐动态 Tool API，并删除 Pi 不存在的注销能力；
6. 类型化 User Bash 结果并更新 Guard、ACP 和 WebUI；
7. 修复诊断可观测性和 Session 命令作用域；
8. 拆分 Web 更新解码和状态应用，并落实 typed-builder 规则；
9. 执行全量验证和真实后端 WebUI E2E。

该顺序先稳定物理路径和生命周期，再修改跨协议类型，最后处理模块整理，减少重复迁移和交叉冲突。

## 验证标准

后端新增行为测试必须位于 crate 的 `tests/` 集成测试目录，不向正文增加测试支撑代码。最终执行：

1. `cargo fmt --all --check`；
2. `cargo check --workspace --all-targets`；
3. `cargo test --workspace --all-targets`；
4. `cargo clippy --workspace --all-targets --all-features -- -D warnings`；
5. WebUI TypeScript 检查和生产构建；
6. 启动正式后端和真实 Provider，运行 WebUI E2E，验证正常对话、Thinking、工具执行、Guard 阻止、真实退出码 `126`、Session 删除与消息回放。

本轮不创建 fixture backend，不创建 commit，不使用 SubAgent 或 worktree。
