# Steer / follow-up 实现验收

日期：2026-10-30。范围：当前工作区的 Kernel、ACP、配置、WebUI，依据同日 spec / plan 的 K1–K4、A1、W1–W2。所有变更留在工作区；没有创建 worktree，也没有 commit。

## 结果

- `rtk cargo test --workspace --no-fail-fast`：**953 passed、9 ignored，107 suites**，最终退出码 0。
- `rtk cargo clippy --workspace --tests --examples -- -Dwarnings`：通过，无警告。
- `rtk cargo fmt --all -- --check`、`git diff --check`：通过。
- `web` 目录的 `npm run check`、`npm run build`：通过。
- `npm run test:elicitation`：3/3 通过。
- 真实生产 `app` + 当前配置 DeepSeek + headless Chrome：**12/12 场景通过，控制台错误 0**。

首次 workspace 执行暴露了旧 remove 返回值断言、input hook 提前后诊断缺少会话事件出口，以及一个既有 Provider 日志测试的瞬时失败。前两项已更新/修复，Provider 定向和最终全量复跑通过。沙箱中的本地监听、系统 secret-store 权限限制导致的环境失败，已在允许相应访问的验证进程中重跑通过。

## 按任务核对

| 任务 | 实现与验证 |
| --- | --- |
| K1 队列与认领 | 仅 Agent Run 开放队列；最后一次检查与入队共享锁。认领项在消费写入前发生 Turn 上限、preflight 或 MessageStart 失败时归还；已认领项不可移除/转换。最终检查后的新 prompt 回退到独立 Run，不滞留。扩展在非 Agent hook 中发送消息不自锁。 |
| K2 接纳与 hook | 同步领取 admission ticket；即使任务调度顺序颠倒也按到达顺序接纳。停止和丢弃 ticket 不留下序号空洞。普通输入 hook 仅一次；改写为 shell/slash 文本保持普通输入，Handled 不入持久历史。原始直接命令等锁时不占队列，成功及取消顺序均有回归。配置默认和显式 placement 均验证。 |
| K3 撤回 | 停止、重复停止、移除、清空与 Run 异常按存活等待者规则结算。Receipt 不再启动救回 Run；失联输入保留。hook 中取消不会继续插入；插入与取消共享写入边界。 |
| K4 转换与快照 | follow-up 转到 steering 尾部，保留 queue/message/entry/submission 标识；追加单条可重放记录。恢复按同一 queueId 移动，并用已存在 entry 去重。队列事件读取/发送串行；close 等待慢事件出口完成最终快照。 |
| A1 ACP | 标准 session/prompt 统一接纳；非法 whenBusy/submissionId 为 -32602。连续 5 个请求按序插入，返回各自 messageId。close 前发送最终空队列并为等待请求返回 -32800。新增 promote；initialize 不再广告旧 followUp。bootstrap 与 QueueChanged 映射覆盖。 |
| W1 发送与面板 | 立即发送并清空编辑器，连续发送保持可用。默认 follow-up/steer 两种配置下 Enter 与 Alt+Enter 对调；分组、转换、跨页同步及真实插入顺序通过。 |
| W2 退还与断线 | 每页按 submissionId 保存原始文本/图片/资源；停止和确定拒绝合并回当前草稿之前。失联后用新队列快照核对；结果未知不自动重发，可明确取回原输入。页面删除同时清理本地提交/退还计时器。 |

Kernel 队列专项位于 `crates/kernel/tests/protocol_alignment/queue.rs`，18 个测试通过；ACP v2 对齐套件 `crates/acp/tests/v2_alignment.rs`，44 个测试通过。示例测试按场景合并，复用了现有集成测试设施，没有为生产代码增加测试专用字段或测试后端。

### 持久化故障

基于真实 JsonlStore 的集成测试包装器注入同步失败和消费记录追加失败：

- 入队同步失败、消息 entry 已追加但 Consumed 失败时，prompt 返回 -32603 且 `data.outcomeUnknown = true`。
- remove、clear、promote 同步失败也保留相同错误标记；后续 stop 不能误报告安全撤回。
- transcript 进入写入失败状态后拒绝继续写入；close/resume 通过重放恢复，部分消费不会重复插入。
- 排队请求错误不会把仍在运行的主 Run 标为空闲；曾观察该断言失败，修复后通过。

这验证了存储返回错误和部分写入后的恢复边界；没有模拟操作系统断电或真实磁盘硬件故障。

## 真实浏览器验收

运行 `target/debug/clawcode serve`，使用 `web/dist`。默认配置副本保持当前 provider `deepseek/deepseek-flash`、服务地址及凭据环境；另一个副本仅将 `kernel.when_busy` 改为 `steer`。数据目录和测试工作目录隔离于 `/tmp/clawcode-queue-e2e`。使用独立 headless Chrome，不操作用户浏览器数据。未使用 fixture backend。

| 场景 | 结果 |
| --- | --- |
| 连续 Enter、转换、Alt+Enter、两页同步、停止后 3 条按提交顺序放回当前草稿前 | 通过 |
| 默认 steer 时提示和 Enter/Alt+Enter 分流对调 | 通过 |
| 停止恢复图片附件 | 通过 |
| 真正服务端拒绝超限图片，恢复原始输入并显示原因 | 通过 |
| 排队后刷新再停止，孤立队列项保留且不误报未知 | 通过 |
| 确认排队后丢失终态应答，重连显示未知并可取回原输入 | 通过 |
| 请求未传输便断线，重连保留未知输入 | 通过 |
| 两页各自提交，停止后只恢复到各自页面 | 通过 |
| 后发送的 follow-up 转 steer 后，实际先于原 follow-up 插入 | 通过 |
| 直接 shell prompt 等锁时被取消，恢复到新草稿之前 | 通过 |
| 同时恢复图片与 resource_link | 通过 |

确定拒绝场景通过传输拦截向真实请求追加第 6 张图片，原编辑器保有 5 张；断线场景只丢弃真实网络请求/应答，不伪造成功或服务端返回。运行中的队列由真实模型调用短时 `sleep` 工具维持。

本地原始证据：

- `/tmp/clawcode-queue-e2e/evidence/results.json`：8/8，errors 为空。
- `/tmp/clawcode-queue-e2e/evidence/extra-results.json`：3/3，errors 为空。
- `/tmp/clawcode-queue-e2e/evidence/restore.png`：停止恢复后的页面。
- `/tmp/clawcode-queue-e2e/evidence/workspace-tests-final.log`：最终 workspace 汇总。
- `/tmp/clawcode-queue-e2e/e2e.mjs`、`extra.mjs`：本次临时验收脚本。

两个验收专用 app 服务已停止；上述临时证据保留供本机核对。

## 实现裁定与边界

- `SubmissionReceipt` 通道携带 Result，以保留持久化结果未知类型；直接命令复用原执行路径并传递 admission，不新建调度服务。
- input hook 提前到 Run 之外后，诊断使用会话事件出口；不丢失原扩展错误提示。
- 按 spec 退役 `_clawcode/followUp`，外部调用方需改用 `session/prompt` 的 `_meta.clawcode.whenBusy`。本次已迁移仓库内调用方，未验证外部旧客户端。
- 退还合并窗口为 spec 规定的 50 ms；跨窗口到达的响应属于不同批次，不保证跨批次全局顺序，代码已标注此限制。
- 未强制运行 workspace 中 9 个 ignored 测试；没有部署、提交或 worktree 操作。


## 效率与代码组织复审修复（2026-10-30）

本轮完成四项修改：

1. ACP journal 与 WebUI 事件列表复用现有快照替换机制，仅保留最新完整队列。快照 key 区分 queue 与 terminal，避免终端 ID 碰撞。实时事件照常发送，旧游标跨过被替换的快照后可恢复最新队列。
2. 队列关闭或 prompt 已取消属于正常接纳分支，降为 DEBUG；真正的写入失败仍保留 ERROR 和原始错误。两个真实验收实例的正常回退 ERROR 记录均为 0。
3. 清空队列在同一把锁内选择目标、提交取消记录并 drain 两条队列，删除二次筛选函数。消除每条记录都复制全量 ID 的二次复杂度；写入失败时仍不改变内存队列。
4. controller 比较 router 应用前后的 pending 引用，仅在队列实际变化时核对提交；重连首次快照仍执行显式核对。普通 token、工具更新及被忽略的重放组不再触发队列扫描。

新增回归先确认失败，再修复通过：ACP 队列历史快照本应保留序号 `[1,3,5]`，修复前保留 `[1,2,3,4,5]`；正常空闲 prompt 的无 ERROR 断言修复前失败。修复后 ACP projection 19/19、Kernel 队列专项 18/18 通过。既有队列关闭日志断言已更新为 DEBUG 上下文，并继续验证不泄露输入正文。

全量首跑还出现既有 `provider_stream_lifecycle_is_logged_without_message_content` 偶发缺少 started 日志，定向与最终全量复跑通过；没有宣称修复该既有测试的偶发性。

最终 `cargo test --workspace --no-fail-fast`：953 passed、9 ignored；fmt、Clippy（含 tests/examples）、diff check、WebUI check/build、elicitation 3/3 通过。真实生产 app + 当前 DeepSeek 配置重新执行原 11 项验收，加上“图片队列快照只保留最新、丢失转换事件后重连、清空后只剩空快照”的新场景，共 12/12 通过，无控制台错误。临时 app 进程已停止。

新增证据：

- `/tmp/clawcode-queue-e2e/evidence/review-fixes-workspace-final.log`
- `/tmp/clawcode-queue-e2e/evidence/review-fixes-results.json`
- `/tmp/clawcode-queue-e2e/review-fixes-app.log`
- `/tmp/clawcode-queue-e2e/review-fixes-app-steer.log`

本轮未做大队列峰值内存或吞吐压力测试；快照仍按协议全量发送，本次优化的是历史保留、清空遍历和无关通知上的重复计算。


## 按用户要求独立重跑真实 E2E（2026-10-30 08:03–08:04，America/Los_Angeles）

重新构建当前工作区的生产 app 与 WebUI，使用当前仓库配置副本及独立的新数据目录。两个实例 bootstrap 均确认 activeModel.id = deepseek/deepseek-flash（DeepSeek V4 Flash），默认 placement 分别为 follow_up 和 steer。浏览器使用独立 headless Chrome；未使用 fixture backend，没有伪造模型成功应答。

本次独立执行三个套件：基础行为 8/8、实际消费顺序/直接命令取消/附件恢复 3/3、快照最新保留与断线恢复 1/1；**共 12/12 通过，所有脚本退出码 0，控制台错误 0，正常回退 ERROR 0**。持久化得到 12 个隔离测试会话文件。

覆盖连续发送、两种默认队列与 Alt+Enter、follow-up 转 steer 后实际先插入、双标签页同步与各自恢复、文本/图片/资源恢复、真实服务端超限拒绝、页面刷新后的孤立队列、发送前断线和应答丢失、快照去重及重连后的转换/清空。故障场景只拦截真实 WebSocket 的发送或接收，确定拒绝由真实服务端返回。

本轮证据根目录：/tmp/clawcode-e2e-20261030-080322。

- evidence/summary.json：总结果与运行结束时间。
- evidence/results.json、extra-results.json、review-fixes-results.json：逐场景结果。
- evidence/e2e.mjs.log、extra.mjs.log、review-fixes.mjs.log：脚本输出。
- evidence/bootstrap-3445.json、bootstrap-3446.json：运行中确认的模型与默认队列。
- evidence/restore.png：停止后的草稿恢复截图。
- app.log、app-steer.log：服务端日志。

两个本轮测试服务已停止。没有修改业务代码、创建 worktree 或 commit。本轮检查了前端 check/build 和 app 构建，没有重跑 Rust 全量单元/集成测试，也没有进行压力测试；此前 953 passed 的结果属于上一轮验证。
