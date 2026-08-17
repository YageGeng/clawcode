# 流式滚动与压缩上下文恢复设计

## 问题

WebUI 当前在消息、Thinking、工具或压缩状态每次变化后都执行 `scrollTop = scrollHeight`，并同时启用 CSS 平滑滚动。流式 token 会连续创建新的滚动目标，因此页面持续抖动；用户向上滚动后，下一次 token 又会把视口拉回底部。展开或折叠消息详情时也会触发布局变化，现有逻辑无法尊重用户位置。

压缩记录已经以精确十进制字符串持久化 `tokensBefore`，WebUI 也能显示该值，但 provider 边界只把摘要正文投影为用户消息，丢弃了压缩前 token 数。因此恢复会话后的模型只能看到摘要，无法知道例如 `156,605` 这一精确上下文规模。

## 设计

### 滚动跟随

新增一个会话视口 Hook，集中管理“是否跟随最新消息”的状态：

- 初次进入或切换会话时跟随底部；
- 视口距离底部不超过 32 像素时视为位于底部；
- 用户向上滚动超过阈值后立即停止跟随，后续流式更新不修改滚动位置；
- 用户自行滚动到底部后恢复跟随；
- 消息、Thinking、工具、压缩状态变化时，仅在跟随状态下于浏览器绘制前同步定位到底部；
- 展开或折叠 `details` 后重新计算实际底部距离，不强制覆盖用户选择。

移除 transcript 的平滑滚动，并为滚动条预留稳定槽位。自动跟随使用即时定位，避免连续 token 重定向动画造成抖动和横向尺寸变化。

### 压缩上下文

由 `CompactionSummaryMessage` 提供模型侧文本投影，将精确 `tokens_before` 写入自然语言前缀：

```text
The conversation history before this point was compacted from 156605 tokens into the following summary:

<summary>
...
</summary>
```

Provider 统一调用该类型方法。持久化结构、摘要正文和 WebUI 卡片保持不变，因此不会重复展示内部前缀；实时压缩和会话回放都会通过同一 provider 转换恢复该数字。

这是相对 pi v4 的有意修正：pi v4 保存 `tokensBefore`，但其模型投影没有包含该字段，无法满足精确恢复要求。

## 验证

- Kernel provider 集成测试断言模型请求包含精确 `tokensBefore` 和摘要正文；
- Web 静态检查和生产构建通过；
- Rust 格式、目标集成测试、完整 workspace 测试和 Clippy 通过；
- 启动正式后端与 WebUI，使用真实 Provider 验证流式期间向上滚动、回到底部、Thinking/工具详情展开折叠，以及压缩后继续对话能读取压缩前 token 数。
