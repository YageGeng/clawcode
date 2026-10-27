# 执行记录 — plan: docs/superpowers/plans/2026-10-27-mcp-tool-discovery-and-pi-negotiation-implementation.md

## 执行约束

- 用户已授权执行全部 spec/plan；在当前工作区顺序实施，不使用 SubAgent/worktree，不创建 commit。
- 保留此前七项审查修复，包括版本发布互斥、完整 ServerSnapshot、Roots 会话路由、Sampling 推理过滤、Degraded 可调用、注册表键复用、validator 缓存。
- 以 spec 为最终约束，计划中的签名可以做保持行为的最小适配并在此记录。
- 测试数据使用临时 XDG_DATA_HOME；涉及 localhost 监听的验证通过 exec 权限流程执行，不改个人配置。

## 接口预检查

| 阶段依赖 | 核对结论 |
|---|---|
| 1 → 2/6 | RuntimeMcpServer 改为协议策略，McpRequestControl 保留普通请求的上下文、取消、活动/总时限；移除 MRTR 轮次 |
| 2 → 3/5/7 | 默认借用元数据接口提供 exposure/output policy；保持普通工具定义的三个 Provider 字段 |
| 3 → 4/6 | 共用文本视图计量最终 Provider 内容；资源分页使用完整 JSON 策略，不能字符截断 |
| 5 → 7/8 | 唯一选择提交入口负责持久化及目录重验；搜索只提交增量集合，界面消费服务端投影 |
| 全部 → 9 | 自审代替技能默认的子代理审查；用户禁止 commit，因此使用未提交 diff 与本记录交接 |

## 进度

- 阶段 1：完成。MCP 测试 27 通过、2 ignored；显式运行 stdio/HTTP 四版本互操作 2 通过。协议/配置 16 通过、Kernel Host 3 通过；workspace clippy、前端 check 通过。新用例覆盖握手拒绝顺序、实际 HTTP 版本头、404 不重协商、活动/总时限。
- 阶段 2：完成。协议/配置/工具/MCP/扩展目标测试共 53 通过，覆盖名称 golden、可选注解、配置覆盖及动态冲突保留。
- 阶段 3：完成。输出/截断/Bash/协议测试共 20 通过，覆盖 UTF-8、聚合文本、媒体、64 MiB 文件上限、保存失败与取消。
- 阶段 4：完成 hook 后结算、Provider 共享文本视图、历史 output 字段和 progress 限额。Kernel hook 放大回归先失败后通过；跨 crate 回归 99 通过。
- 阶段 5：选择集合、原子提交、旧格式迁移及导航恢复已落地；状态单元测试 6 通过，Store 同步失败不发布测试通过，现有分支/能力/compaction/turn 回归 58 通过。后续搜索阶段补齐真实搜索后的恢复场景。
- 阶段 6：实现完成。资源分页、游标上下文、旧游标、超大单条、Hidden 访问、未列出 URI 和真实协议取消测试通过；MCP 目标测试 10 通过，Kernel/ACP/tools 回归 58 通过。新增资源及输出边界测试合计 8 通过。
- 阶段 7：实现完成。冻结批次测试先失败后通过；实际搜索后空根、fork、导航、resume 的模型声明断言通过。纯分词、BM25、输入边界及索引复用单元测试通过；workspace clippy 无警告。继续补充最终矩阵中剩余边界。
- 阶段 8：完成。ACP 正常结束/回放 output 契约、含选择通知的 loaded 投影通过；Kernel/ACP/app 目标测试 93 通过，前端 check/build 通过。
- 阶段 9：实现收尾与核心验收完成，外部人工授权项未验证。首次全量 709 通过、2 ignored；已显式通过两种 transport 的四版本互操作。正式 app + DeepSeek V4 Flash Vision Exp 跑通搜索→下一轮调用、203035→20436 字节输出、50/2 条分页、模板 URI、Hidden 零结果；已验证 reconnect、UI 重载回放和 fork 加载状态。收尾新增碰撞/协商边界回归后再跑最终检查。

## 决策记录

- 决策：不执行技能中的 worktree、commit 和子代理步骤，直接维护本记录与当前 diff；用户明确约束优先。
- 决策：此前完成的审查修复是本次实现基线，不重新实现或撤销。
- 决策：NegotiationTransport 只接收 transport；请求策略归 InitializeClient，支持集合归 protocol，避免重复保存策略。
- 回归修复：服务器关闭导致取消通知失败时，保留原始超时/取消分类；对应时限测试先失败后通过。
- 兼容修复：Sampling 合并相邻文本片段以支持旧协议单内容结果；分片测试先失败后通过，Reasoning 仍被过滤。
- 决策：CatalogMapper 只借用 server_id 完成 wire 转换。配置策略及冲突校验统一放在 RuntimeMcpServer::prepare_catalog、在发布前执行，覆盖所有 McpClient 实现，避免向传输层传递整份宿主配置。
- 阶段 2：稳定名称 golden 测试确认旧实现失败后，加入 SHA-256 长度帧算法；有效原名保持兼容。
- 决策：复用 turn.rs 已有模型请求捕获与扩展测试设施承载 hook 放大测试，避免新建一套重复 fixture；新增用例直接比较事件、后续请求和持久化结果。
- 决策：工具状态写锁同时串行化选择提交和目录发布，锁内同步 Store、无 await。无需额外同职能 Mutex，锁序固定为工具状态 → Transcript；导航也按同一顺序恢复目标分支。
- 决策：分页在完整目录上排序借用引用，二分选取最多 200 条中的最大合法页，序列化缓冲区固定 20 KiB；失败时不发布不前进的游标。
- 决策：搜索除了 16 KiB 文本和深度 8，还限制 4096 次 schema 节点访问，避免大量无描述分支消耗无界 CPU；不增加配置项。
- 决策：Kernel 为 ACP 合并目录与选择的更新通知，仍从原 MCP 快照和 ToolSelection 派生内容，不维护第二份目录。UI 更新版本独立于资源分页游标，加载工具不使资源游标失效。
- 自审修复：偏好旧版本、服务器选择新版本时，声明实现支持的 Host 回调，再按实际协商版本限制调用；回归先失败后通过。请求入队也受总时限/取消控制，取消通知有界等待，关闭的进度流不再参与 select。
- 自审修复：会话合并目录对跨服务器的合法名称碰撞做发布前校验，拒绝变更贡献者并保留最后成功目录；初次冲突报告 Failed。已覆盖 a/_x 与 a_/x，以及冲突后恢复。
- 效率修复：结果 hook 消费并返回同一个拥有的结果，不再复制未结算的大内容；检索结果复用有界描述摘要，不为显示摘要重复克隆 schema。
- 验收环境：/tmp/clawcode-mcp-e2e-lcj2vxtt，app 127.0.0.1:39080、HTTP 参考服务 127.0.0.1:39128。browser-skill 扩展未连接，使用应用内浏览器；不修改浏览器设置。
- 验证异常：一次 rustc/async-trait 宏展开 SIGSEGV，随后相同目标重新运行通过；没有为绕过编译器崩溃修改产品逻辑。
- 收尾回归：补充跨服务器名称冲突、Hidden 直调、保存失败、长服务器过滤游标及 null 参数边界后，全量 714 通过。正式 WebUI 完成真实 resume、compaction、空根归零、导航恢复；app/HTTP/stdio 进程及临时页已关闭，端口 39080/39128 均拒绝连接。
- 最后持久化修复：同步失败后不只保留旧选择，还将 Transcript 标记为需要重新打开，阻止后续写入；只读检查和关闭时的刷新仍允许。新增“第二次写入必须失败”的断言先失败，修复后进行目标及最终回归。

## 首次实现收尾状态

最终 fmt/check/clippy 通过，全量 714 通过、2 ignored（96 suites）；ignored 的 stdio/HTTP 四版本互操作已显式通过。持久化失败写入保护、关闭时刷新及重新打开回归通过，目录撤下后的增量加载重验断言通过。正式 app/真实 provider 的核心路径、reconnect/resume/fork/compaction/tree/空根和历史回放均完成。详细证据与未验证范围见[验证记录](2026-10-27-mcp-tool-discovery-verification.md)。

## 后续六项审查修复

用户授权后已修复：进度饿死总超时、启动目录漏同步、空结果转换失败、最终页二分错误、Provider 分隔符预算遗漏、落盘碎片写入。五个逻辑回归先失败后通过；缓冲写入回归先观察到 20037 次写调用，再验证缓冲及最终 flush 错误传播。新增回归留在对应 crate 测试目录或精确 tests 模块内。

最新全量 721 通过、2 ignored（97 suites）；两种 transport 四版本互操作已显式通过，fmt/check/clippy 均通过。真实 provider/WebUI 未重复运行，详情见验证记录的“后续审查”部分。没有新增依赖、SubAgent、worktree 或 commit。
