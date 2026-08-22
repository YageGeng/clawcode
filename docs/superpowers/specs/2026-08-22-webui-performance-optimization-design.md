# WebUI 长会话性能优化设计

## 1. 背景

Clawcode WebUI 已支持 ACP v2 multi-message、`replayFrom`、cursor 恢复、流式 Markdown、语法高亮、Context Lens 和 Session Tree。当前实现虽然把一个物理 WebSocket frame 最终合并为一次 Zustand 提交，但 frame 内仍逐条执行不可变 reducer；同时，任何消息 delta 都会触发完整 Conversation、AppShell 和 Inspector 的渲染链路。

长 Session 或大批量 replay 下主要存在以下热点：

- 每个 update 重复复制 `sessionWorkspaces`、消息 Map 和事件数组。
- 事件数组每次插入都完整排序，并永久保留每个流式 payload。
- Conversation 为每条消息扫描 Tree，并为每个 Tool 复制和反向扫描全部消息。
- 历史 Markdown 和语法高亮会随当前流式消息重复执行。
- AppShell 和 Inspector 订阅完整 Session workspace，外围 UI 随每个 token 重渲染。
- Events 页签会立即序列化和挂载全部诊断 payload。

## 2. 目标

- 一个物理 ACP frame 对每个 Session 的可变集合最多复制一次。
- 保留 ACP projection 顺序、cursor 原子提交、恢复分界线和全量 replay fallback 语义。
- 将 replay 的主要归约复杂度从“批大小乘以历史集合大小”降低为“批大小加历史集合大小”。
- 流式更新时只重新渲染实际变化的 Message 或 Tool，不重新解析历史 Markdown。
- Context Lens 的 Message/Tool 到 Tree 映射使用索引或点击时惰性解析，不在 transcript render 内做平方级扫描。
- 限制诊断事件的常驻内存和 Events 页签 DOM 数量，同时不影响消息、工具、Tree、cursor 等业务投影。
- 保持现有 UI、ACP 协议、Session URL、键盘操作和恢复行为不变。

## 3. 非目标

- 不修改 ACP Rust 后端或 WebSocket frame 格式。
- `[app.recory] max_batch_size` 保持部署配置；仓库当前明确配置为 `512`，WebUI 只消费形成后的物理 frame，不硬编码、覆盖或解释该配置。
- 不引入 Immer、虚拟列表框架或新的状态管理依赖。
- 不改变 Message、Tool Call、recovery divider 和 Tree 的用户可见顺序。
- 不把诊断事件作为持久化或恢复数据来源；它仍只服务 Inspector 的调试视图。
- 不在本次优化中实现完整 transcript windowing。历史 row 继续保留在 DOM 中，但通过 memo 和浏览器内容可见性减少重复工作。

## 4. Frame 级批量归约

### 4.1 Session frame draft

为单个 Session 引入 frame 级 staging 对象。它从当前 `SessionWorkspaceState` 开始，按通知顺序应用 `SessionWorkspaceAction`，但采用 copy-on-write：

- `messages`、`tools`、`bashExecutions`、`extensions`、`compactions`、`mcpElicitations` 首次写入时复制，后续 action 复用同一可写集合。
- transcript 新条目和 diagnostics 在 frame 内追加，frame 结束时最多排序一次。
- events 在 frame 内只收集新增事件，结束时统一合并并排序一次。
- 标量状态按 action 顺序覆盖，确保后续 decoder 能读取前一 action 的最新投影。
- `transcript/cleared` 必须重置 draft，并保留现有 `outcomeUnknown` 语义。

Staging 对象只存在于 `applyMany()` 调用期间，不进入 Zustand，不泄露可变引用。完成后冻结为新的只读 `SessionWorkspaceState`。

### 4.2 Workspace frame draft

`SessionUpdateRouter.applyMany()` 按 Session 缓存 staging 对象。每条 notification 仍按到达顺序经过 recovery buffer 和 decoder；Session action 直接应用到对应 staging 对象，global action 保持现有 reducer 语义。

每个完整 projection group 在 staging 成功应用后立即 provisional commit recovery cursor，使同一物理 frame 内的下一组可以继续校验。若后续组失败，异常路径调用 full replay fallback 清除该 provisional cursor，因此不会对外暴露未提交的 UI 状态。

frame 结束时：

1. 完成每个 Session staging 对象。
2. 每个变更 Session 只更新一次 `sessionWorkspaces` Map。
3. 根据最终 `running` 状态一次性更新 `runningSessionIds`。
4. 只 dispatch 一次 `workspace/committed`。
5. 统一提交 UI state；异常路径保持 full replay fallback，并清除 provisional cursor。

decoder 读取 staging 当前视图，因此 whole-message 的 omitted-field upsert 语义不变。

## 5. 诊断事件内存策略

每个 Session 只保留最近 2,000 条 `SessionEvent`，常量命名为 `MAX_RETAINED_SESSION_EVENTS`。这是 Inspector 调试窗口，不影响业务投影和 cursor。

- frame 完成时统一合并并排序新增事件。
- 超过上限时只保留顺序最新的 2,000 条。
- Events 页签顶部显示“当前展示最近 N 条事件”的说明。
- Event row 默认只渲染摘要；用户展开某一项后才执行 `JSON.stringify(payload)`。
- Events 列表使用 `content-visibility: auto`，降低折叠项的布局和绘制成本。

diagnostics 文本沿用现有行为，本次不截断；它的产生频率远低于 ACP token update。

## 6. Conversation 渲染隔离

### 6.1 Tree 索引

根据 `SessionTree` 构建一次 `messageId -> SessionTreeEntry` 索引。Tree identity 未变化时复用索引。Message row 通过 O(1) 查询获得 entry，不再为每条 transcript message 调用线性 `find()`。

Tool Call 不在 render 阶段解析 owner。点击 Context Lens 时，从当前 store 读取 Session workspace，反向查找同一 Turn 最近的 assistant message，再通过 Tree 索引获得 entry。该成本只在用户点击时发生一次。

### 6.2 稳定 transcript row

Message 和 Tool row 拆为小型 memo 组件：

- props 只包含实体、必要 Session 信息和稳定 callback。
- 未变化实体保持引用相等，不重新执行 row render。
- Context Lens selection 只改变之前选中和当前选中的 row。
- `running` 状态变化允许相关 MessageActions 统一刷新。
- Conversation 自身可以随 scroll revision 重渲染，但历史 row 被 memo 隔离。

`onInspect` 使用 `useCallback`，不在 transcript `.map()` 内为每个 frame 创建导致 memo 失效的语义回调。

### 6.3 Markdown 与语法高亮

`MarkdownContent` 使用稳定的 remark/rehype plugin 配置，并通过 memo 跳过相同文本。

- settled Message、Reasoning、Tool output 和 Compaction 继续使用 Markdown 与语法高亮。
- streaming Message 和 streaming Reasoning 继续渲染 Markdown，但暂不执行 `rehype-highlight`。
- Message 结束后执行一次完整语法高亮。
- Tool payload 继续只在 Tool details 展开时挂载。

这一策略保留流式 Markdown 可读性，同时避免对不断增长的未完成代码块重复高亮。

## 7. Zustand 订阅边界

AppShell 不再订阅完整 `SessionWorkspaceState`，而是使用细粒度 selector 读取：

- runtime status 所需的 `contextUsage`、`retry` 和 `compaction`。
- 当前 Context Lens 所需的单个 Message 或 Tool。
- Skills、MCP 和 elicitation 所需的独立字段。

Inspector 按页签拆分订阅：

- Tree panel 只订阅 `tree` 和 cwd。
- Events panel 只订阅 events 和 diagnostics。
- Tools panel 只订阅 tools。

因此普通 message delta 不再刷新 SessionSidebar、主导航、Tree 或未打开的 Inspector 页签。

## 8. DOM 和绘制策略

- Message、Tool、Event 和 Tree row 增加适合的 `content-visibility: auto` 与 `contain-intrinsic-size`。
- 不对当前 streaming Message 使用内容跳过，确保自动滚动获取准确高度。
- SessionTree 深度计算改为缓存父节点深度，避免链式 Tree 的平方级父链遍历。
- Tree payload 和 Event payload 仅在对应 details 展开时序列化。

## 9. 兼容性和错误处理

- copy-on-write staging 不允许把可写 Map 暴露给旧 state 或 React 订阅者。
- action 顺序、EventOrdering、projection group 原子性和 recovery cursor 提交时点保持不变。
- 任意 decoder 或 staging 异常继续使 touched Session 进入 full replay fallback。
- 事件截断只作用于 Inspector diagnostics，不参与恢复和去重。
- reduced-motion、Context Lens、Session URL 和焦点恢复不受本次改动影响。

## 10. 验证

项目规则允许前端免单元测试，因此本次不引入新的测试框架，使用以下门禁和真实场景验证：

1. `rtk npm --prefix web run check`。
2. `rtk npm --prefix web run build`。
3. 使用可重复的本地 benchmark 对比 2,000 条历史消息下应用 128 条 delta 的归约耗时，并检查主要集合的 copy-on-write 次数。
4. 使用 production `app` 后端恢复真实长 Session，验证 transcript 顺序、recovery divider、Tree、Tool 和 Events。
5. 发送包含 Thinking、长 Markdown 和代码块的真实 prompt，确认流式阶段保持响应，结束后语法高亮正确。
6. 打开 Context Lens，验证 Message 和 Tool 的 Tree 定位不变。
7. 打开 Events，验证最多展示最近 2,000 条且 payload 仅在展开时出现。
8. 检查浏览器日志无 React、WebSocket 或序列化错误。

## 11. 验收标准

- 128 条 multi-message frame 对每个 Session 的主要集合最多复制一次。
- 历史 Message 的 Markdown 不随无关 delta 重解析。
- Conversation render 不再为每个 Message/Tool 扫描完整 Tree 或 messages。
- Session events 常驻数量不超过 2,000。
- 普通流式 token 不触发 AppShell 外围区域和未打开 Inspector 页签的 workspace 订阅更新。
- 现有前端检查、生产构建和真实恢复/流式交互全部通过。
