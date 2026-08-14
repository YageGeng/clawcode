# WebUI 事件顺序、Thinking 与会话删除设计

## 问题

当前 WebUI 将消息和工具调用分别渲染：先输出全部消息，再输出全部工具，因此工具调用会脱离 ACP 通知中的真实顺序。事件面板也只按抵达顺序追加，没有使用产品元数据中的 `sequence`。

Thinking 已通过 ACP v2 的 `agent_thought_chunk` 和 `agent_thought` 进入前端状态，但展示组件默认收起，用户无法直接看到推理流。整条消息更新还使用了字符串回退逻辑，无法严格表达 ACP v2 的“省略保持、`null`/空数组清空、具体数组替换”语义。

会话侧边栏的 `X` 当前调用 `session/close`。该方法只释放运行时，不删除持久化会话。ACP v2 已原生提供 `session/delete`，不应再定义同义的产品扩展。

## 设计

### 统一协议顺序

前端为每个收到的 `session/update` 分配 `EventOrder`：

- `receivedOrder` 是当前连接内单调递增的本地序号；
- ACP v2 要求 whole-message upsert 与 chunk 按收到顺序应用，WebSocket 也保证帧顺序，因此该序号是渲染顺序的权威来源；
- 产品元数据中的 `sequence` 保留用于诊断，但 kernel runtime 和回放可能分别从 1 开始，不能作为跨恢复的全局排序键。

Workspace 保存统一的 transcript 项目列表。消息和工具第一次出现时写入该列表，后续 upsert 只更新实体，不改变首次出现位置。对话区按 transcript 渲染，不再分别输出消息和工具。

事件面板使用相同的接收顺序模型，保证回放、实时通知以及同一 kernel 事件映射出的多条 ACP 通知都有确定顺序。

### ACP v2 整条消息

保留流式 chunk 的追加行为，并严格实现整条消息 upsert：

- 未提供 `content`：保留已有内容；
- `content: null` 或 `content: []`：清空已有内容；
- `content` 为具体数组：替换已有内容；
- 后续 chunk：继续追加。

当前后端回放已经通过 `session/resume` 的 `replayFrom: { type: "start" }` 发送 `user_message`、`agent_message`、`agent_thought` 整条更新，因此不增加私有批量协议。

### Thinking 展示

Thinking 与对应 Assistant 消息共享 `messageId`，保持现有数据模型。推理块首次出现时默认展开，流式 chunk 实时追加；完成后内容保留，用户可以手动收起。

### 标准会话删除

StoreFactory 新增按 `SessionId` 幂等删除接口。JSONL 实现扫描 v4 header 定位文件并删除，不创建全局索引，也不删除无关目录。

Kernel 删除流程先取消并等待活动运行结束，释放运行时和扩展资源，再删除 JSONL。不存在的会话按 ACP 语义返回成功。

ACP Server：

- 在 `capabilities.session.delete` 声明支持；
- 注册标准 `session/delete` 请求；
- 成功返回空结果。

WebUI 初始化时检查删除 capability。侧边栏使用删除图标和行内二次确认，确认后调用标准 `session/delete`。删除当前会话时清空工作区，再刷新会话列表。

## 验证

- Store 集成测试覆盖硬删除和重复删除；
- Kernel 集成测试覆盖活动及持久化会话删除；
- ACP 集成测试覆盖 capability 与标准方法；
- 运行完整 Rust 测试、Clippy 和 Web 检查；
- 启动正式后端，使用真实配置和真实 Provider 完成 WebUI E2E，检查消息/Tool/Thinking 顺序以及会话删除。
