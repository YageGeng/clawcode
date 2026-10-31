# 会话删除与 NotFound 轮询修复验收

用户要求：WebUI 删除会话时停止相关状态轮询；服务端已明确 SessionNotFound 时不能无限重试。本次仅修改前端控制器、ACP 错误识别和后台会话轮询组件，未修改后端错误级别或协议，未创建 commit。

## 修复行为

- 删除请求发出前标记目标和已知的全部子会话；runtime、tree、MCP 等自动快照查询统一检查生命周期。
- 删除成功立即移除本地列表、运行标记和恢复句柄；随后列表刷新失败或旧列表晚到都不能重新启动已删除会话的轮询。
- 仅识别当前后端的精确错误契约：code=-32603 且 data 等于该 ID 的 `session not found: <id>`。普通内部错误、网络错误以及其他 ID 的 NotFound 不会被当成删除。
- runtime 查询检查了内存和持久化文件，其 NotFound 才作为会话真正不存在的依据。树/MCP 的 NotFound 可能仅表示运行态关闭：清除附件状态并等待显式重开，不移除仍有存档的会话。
- 丢弃旧连接、删除期间和确认不存在后的晚到快照/通知；晚到的打开操作也不能激活已经删除的会话。
- 连接未 ready 或轮询目标全部失效时结束定时器；删除失败恢复查询，临时错误保持重试，正常会话不受失效 ID 影响。
- 新连接取得权威会话列表后重置失效标记，允许恢复的会话重新参与查询。新增子会话的完成通知仍可触发列表发现。

## 定向验证

直接加载当前 TypeScript 控制器、Zustand store 和 reducers，在受控传输边界做断言。不是 WebUI 端到端替代后端，也未新增前端测试框架。

- 修复前两个复现失败：删除中仍查询父子会话；同一不存在 ID 连续查询 3 次。
- 修复后 **13 项检查通过**：删除期间屏蔽、NotFound 停查、其他会话继续、临时错误重试、删除失败恢复、晚到成功抑制、删除后列表刷新失败、旧连接隔离、MCP 关闭状态、精确错误匹配、新子会话发现、晚到打开抑制、恢复流程删除检查。
- 脚本及日志：`/tmp/clawcode-session-polling/contract.cjs`、`red.log`、`green.log`。
- `npm --prefix web run check`（TypeScript / ESLint）、`npm --prefix web run build`、`git diff --check` 均通过。后端未改动，没有重复运行 Rust 全量测试。

## 生产浏览器验证

证据根目录：`/tmp/clawcode-session-polling-nqddtpji`。使用当前 production app、构建后的 WebUI 和原配置的真实 DeepSeek。配置副本为 0600，provider 参数保持不变；工作目录、数据目录和 Chrome profile 独立。

准备了四个隔离的空 JSONL 会话，其中两个只有 background ownership 元数据，用于触发真实的后台会话 UI；没有启动或调用子代理。会话的读取、查询、删除及恢复都由生产 backend 执行。

| 场景 | 结果 |
|---|---|
| 原有后台会话轮询 | 两个持久化子会话均正常查询 |
| 文件被其他操作移除 | 首次真实 SessionNotFound 后观察 6500ms，后续请求为 0；另一个正常子会话继续查询至少两次，失效条目退出列表 |
| 恢复并重新连接 | 恢复同一隔离文件后重连，权威列表重新接纳该 ID，查询恢复 |
| 当前会话继续使用 | production WebUI 调用真实 DeepSeek，返回 SESSION_POLLING_HEALTHY，未调用工具或子代理 |
| WebUI 删除父会话 | 点击真实删除按钮和确认按钮；生产后台真正执行删除，仅在浏览器延迟实际确认响应 3500ms，期间对父子会话无新查询；确认后再观察 4500ms，仍无查询 |
| 其他数据 | 父会话及两个子会话已实际删除，独立会话保留 |

最终浏览器脚本退出 0，未捕获浏览器异常。逐项检查全部出站消息，删除请求之后对这三个 ID 的请求总数为 **0**（不仅是 runtime）。唯一 RPC 错误是首次主动复现的 SessionNotFound，对应 3 行 WARN；没有其他 RPC 错误或无限告警。

证据：`/tmp/clawcode-session-polling-nqddtpji/evidence/results.json`、`wire.json`、`validated.json`、`app.log`、`missing-stopped.png`、`deleted.png`。临时脚本为 `/tmp/clawcode-session-polling-nqddtpji/browser.mjs`，保留了实际删除确认的延迟逻辑，没有伪造成功响应或替换服务端。

## 边界

删除前已发出的在途请求不能从网络撤回，但其晚到结果不会恢复已删除状态，也不会引发后续轮询。此次未修改或清理用户的真实会话，也未重启用户正在运行的服务；本轮隔离测试 app 和浏览器已停止。刷新 WebUI 后会加载新构建。
