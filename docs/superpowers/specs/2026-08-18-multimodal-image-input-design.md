# 模型图片输入能力设计

## 目标

Clawcode 支持由模型配置声明图片理解能力，并通过 Kernel、ACP v2 和 WebUI 完整传递用户图片。当前只定义 `text` 和 `image` 两种输入能力，不扩展音频、视频或文档能力。最终使用配置中的 `kimi-k3` 模型执行真实图片理解请求，验证图片确实进入模型而不是只完成协议序列化。

## 配置与模型能力

模型配置新增 `input` 数组，与 Pi 的模型定义保持一致：

```toml
[[providers.models]]
id = "kimi-k3"
input = ["text", "image"]
context_tokens = 1000000
max_output_tokens = 384000
```

协议层新增 `ModelInputModality` 枚举，只接受 `text` 和 `image`。`ModelProfile` 和 `LlmModel` 使用同一个强类型能力集合，避免在各层重复解析字符串。省略 `input` 时默认为 `["text"]`，保持现有配置行为。空数组、重复值以及不包含 `text` 的组合在配置校验阶段失败；本阶段只允许 `["text"]` 和 `["text", "image"]`。

## Kernel 多模态输入

`RunInput` 不再把复合 ACP Prompt 展平为字符串，而是保存有序的 `ContentBlock`。它提供两种受约束的投影：

- 只有单个文本块时，允许 Slash Command 和用户 Bash 分发。
- 普通模型输入保留文本和图片的原始顺序、base64 数据及 MIME 类型。

Skill、Prompt Template 和文本输入扩展只处理纯文本 Prompt。包含图片的 Prompt 作为普通用户消息处理，不触发 Slash Command、用户 Bash、Skill 或 Prompt Template 展开。现有扩展 Input Hook 仍只处理文本，因此图片 Prompt 跳过该 Hook；后续若要支持扩展修改图片，需要单独升级扩展协议，避免本次改动扩大到第三方扩展兼容性。

首次 Prompt、steering 和 follow-up 使用相同的多模态输入类型。队列持久化完整 `AgentMessage`，因此图片在排队、消费、恢复和会话回放期间不允许被展平或丢失。

## Text-only 模型行为

图片始终保存在会话原始消息中。每次生成 `ModelRequest` 时，根据该 Turn 实际选择的 `ModelProfile.input` 创建模型侧消息副本：

- 支持 `image` 时原样发送图片块。
- 不支持 `image` 时，将连续图片替换为一个明确的文本块：`(image omitted: model does not support images)`。

该策略与 Pi 一致，可在同一会话切换模型而不破坏历史图片。降级发生在模型请求边界，不能改写持久化会话或 ACP 回放内容。Tool Result 中的图片遵循同样规则。

## ACP v2

Agent 初始化时声明标准 `session.prompt.image` 能力。该能力表示 Agent 能接受、保存和路由图片，不表示每个当前可选模型都能理解图片；具体模型能力由 `ModelProfile.input` 决定。

ACP 入站图片使用标准内容块：

```json
{
  "type": "image",
  "data": "<base64 payload without data URL prefix>",
  "mimeType": "image/png"
}
```

入站转换保留文本和图片块。暂不接受 Audio 和 Embedded Resource。已有 ACP 出站图片映射继续用于实时消息和会话回放。

自定义 follow-up 接口由字符串参数升级为标准 Prompt 内容块数组，使运行中的图片输入与 `session/prompt` 行为一致。空 Prompt、无效 base64、不支持的 MIME、图片数量超限或单图体积超限在进入 Kernel 前返回 invalid params。移除队列和清空队列接口保持不变。

## 图片格式与资源限制

首期接受 `image/png`、`image/jpeg`、`image/gif` 和 `image/webp`。ACP 和 WebUI 都不得把完整 base64 写入 INFO 日志；现有 DEBUG 参数追踪需要对图片 `data` 字段替换为包含字节数的摘要，避免日志文件复制整张图片。

服务端设置固定安全上限：每条 Prompt 最多 5 张图片，单张解码后最大 10 MiB，总解码体积最大 20 MiB。WebUI 在读取文件前检查数量和浏览器提供的文件大小，服务端再次解码并校验，客户端校验不能替代服务端边界。

会话存储本阶段继续保存 base64，以保持 ACP 回放无损；Blob 存储和内容寻址不在本次范围内。

## WebUI

WebUI Composer 增加图片选择入口，支持一次选择多张图片，并显示缩略图、文件名和移除按钮。首期不实现拖放、剪贴板粘贴、自动缩放或格式转换。

发送时浏览器读取文件为 base64，构造 ACP Image ContentBlock。运行中发送时使用升级后的 follow-up 内容块参数，不允许退化成文本。草稿只在当前页面内保存图片对象，不把 base64 写入 `sessionStorage`；页面刷新后图片草稿消失，文本和资源链接草稿行为不变。

会话更新解码器保留消息的文本和图片块。`MessageView` 使用受控 `data:<mime>;base64,<data>` URL 展示图片，只渲染白名单 MIME。排队列表显示文本摘要和图片数量，队列被消费后继续依赖现有原生 user message 更新自动清除。

## 错误处理与日志

关键错误返回前记录包含 session、模型和错误原因的正文日志，但不记录图片内容。调用 Provider 前后沿用现有模型请求生命周期日志；新增日志只用于图片校验失败、text-only 降级和 ACP 图片参数拒绝。text-only 降级每次模型请求最多记录一条 DEBUG 日志，避免按图片数量刷屏。

WebUI 对文件类型、数量和大小错误给出可操作的中文提示。ACP 错误使用 invalid params；Provider 不支持已声明 MIME 时作为模型协议错误返回，并包含 MIME 类型但不包含数据。

## 测试与验收

实现遵循测试先行：

1. 配置测试覆盖默认 `["text"]`、显式图片能力和非法组合。
2. Kernel 测试证明图片块进入 image 模型、text-only 模型收到占位符、原始历史仍保留图片。
3. 队列测试覆盖图片 follow-up 的持久化、消费、恢复和自动移除。
4. ACP 测试覆盖初始化能力、标准图片 Prompt、无效 base64、MIME/数量/大小限制和图片回放。
5. Provider 映射测试覆盖 `kimi-k3` 所用 Provider API 的最终图片请求形状。
6. WebUI 使用生产 `app` 后端和实际配置 Provider 执行端到端测试：选择固定小图，向 `kimi-k3` 提问图中可验证的信息，并断言模型回答包含预期内容。测试同时覆盖运行中图片 follow-up 和队列自动清除。

真实测试图片使用仓库内体积很小、答案唯一的 PNG fixture，例如白底黑字 `CLAW-7319`。最终响应必须包含 `CLAW-7319`；仅验证 HTTP 成功或消息回显不算通过。

## 非目标

- 音频、视频、PDF 或其他文档输入。
- 图片生成或 Assistant 图片输出的新能力声明。
- OCR 预处理、图片压缩、自动缩放和格式转换。
- Blob 存储、去重和会话文件迁移。
- 第三方扩展 Input Hook 修改图片。
- 动态修改 ACP 初始化能力；ACP 表示 Agent 级接受能力，模型级能力单独生效。
