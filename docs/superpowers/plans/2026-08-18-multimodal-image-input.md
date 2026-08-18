# 模型图片输入能力实现计划

> **供执行 Agent 使用：** 必须使用 `superpowers:subagent-driven-development`（推荐）或 `superpowers:executing-plans` 逐任务执行。本计划使用复选框跟踪状态。

**目标：** 为模型增加 `text`/`image` 输入能力配置，打通 Kernel、ACP v2 与 WebUI 图片链路，并使用生产 `app` 后端和 `moonshotai/kimi-k3` 完成真实图片理解验收。

**架构：** `ModelInputModality` 是配置和运行时共享的强类型能力；`RunInput` 保存原始有序内容块，纯文本路径继续支持命令和扩展，图片路径直接形成用户消息。ACP 负责标准图片块验证，Kernel 在模型请求边界按当前模型能力保留或降级图片，WebUI 只负责文件选择、base64 编码、排队和展示。

**技术栈：** Rust 2024、Serde、typed-builder、base64、agent-client-protocol v2、React 19、TypeScript 6、Vite、生产 Axum/WebSocket app。

**规格：** `docs/superpowers/specs/2026-08-18-multimodal-image-input-design.md`

## 全局约束

- 不使用 worktree；用户明确要求在当前工作区实现。
- 未经用户明确允许不创建 commit；每个任务以测试和 `git diff` 复核结束。
- Rust 生产代码新增函数必须有英文函数级注释，非平凡逻辑必须有英文注释。
- 超过三个字段的 Rust struct 使用 `typed-builder`，并通过 builder 构造。
- 新增日志使用完整 `tracing::` 路径和正文参数，禁止记录图片 base64、凭据或完整模型请求。
- 后端行为严格测试先行；WebUI 按仓库规则免单元测试，但必须通过类型、lint、构建和生产后端端到端验收。
- 图片仅接受 PNG、JPEG、GIF、WEBP；每条 Prompt 最多 5 张，单张解码后最多 10 MiB，总计最多 20 MiB。
- 不修改音频、视频、文档、图片生成或扩展图片变换协议。

---

### 任务 1：模型输入能力类型和配置

**文件：**
- 修改：`crates/protocol/src/capability.rs`
- 修改：`crates/protocol/src/lib.rs`
- 修改：`crates/config/src/llm.rs`
- 修改：`crates/config/tests/loading.rs`
- 修改：`README.md`
- 修改：`README_zh.md`
- 修改：`claw.toml`

**接口：**
- 产出：`ModelInputModality::{Text, Image}`。
- 产出：`ModelInputModalities`，默认只含 `Text`，提供 `supports(ModelInputModality) -> bool`。
- 产出：`ModelProfile.input: ModelInputModalities` 和 `LlmModel.input: ModelInputModalities`。

- [ ] **步骤 1：编写配置失败测试**

在 `crates/config/tests/loading.rs` 添加三个行为测试，分别断言省略 `input` 时 Profile 为 text-only、显式 `input = ["text", "image"]` 支持图片、`input = []`、`input = ["image"]` 与 `input = ["text", "text"]` 返回配置错误。期待值使用字面量枚举，不调用被测归一化代码生成期待值。

```rust
assert!(profile.input.supports(ModelInputModality::Text));
assert!(!profile.input.supports(ModelInputModality::Image));

assert!(profile.input.supports(ModelInputModality::Image));

assert!(matches!(config.validate(), Err(ConfigValidationError::InvalidModelInput { .. })));
```

- [ ] **步骤 2：运行配置测试并确认红灯**

运行：

```bash
rtk cargo test -p config --test loading model_input -- --nocapture
```

预期：编译失败，原因是 `ModelInputModality`、`ModelInputModalities` 或 `ModelProfile.input` 尚不存在。

- [ ] **步骤 3：实现强类型能力与校验**

在 `capability.rs` 定义小写 Serde 枚举和封装集合。封装类型通过 `TryFrom<Vec<ModelInputModality>>` 拒绝空值、重复项和缺少 text 的组合，`Default` 返回 text-only；`LlmModel::to_profile` 把已解析能力复制到 Profile。

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelInputModality {
    Text,
    Image,
}

impl ModelInputModalities {
    /// Returns whether the model accepts one input modality.
    #[must_use]
    pub fn supports(&self, modality: ModelInputModality) -> bool {
        self.0.contains(&modality)
    }
}
```

`ModelProfile.input` 使用 builder 默认值，避免无关测试 fixture 全量修改；配置反序列化默认 text-only，但显式非法数组必须保留到校验阶段并报错，不能静默修正。

- [ ] **步骤 4：运行配置测试并确认绿灯**

```bash
rtk cargo test -p config --test loading model_input -- --nocapture
rtk cargo test -p config
```

预期：所有 config 测试通过，无 warning。

- [ ] **步骤 5：更新示例配置和 kimi-k3**

README 的普通模型示例显式写 `input = ["text"]`；本地 `claw.toml` 的 `kimi-k3` 写 `input = ["text", "image"]`。不显示或改写 API key。

- [ ] **步骤 6：复核任务差异**

```bash
rtk git diff -- crates/protocol/src/capability.rs crates/protocol/src/lib.rs crates/config/src/llm.rs crates/config/tests/loading.rs README.md README_zh.md claw.toml
```

预期：只包含能力类型、校验、文档和 kimi-k3 配置；没有 commit。

### 任务 2：Kernel 保留多模态 Prompt

**文件：**
- 修改：`crates/protocol/src/session.rs`
- 修改：`crates/kernel/src/runtime/input.rs`
- 修改：`crates/kernel/src/runtime/run.rs`
- 修改：`crates/kernel/tests/slash_commands.rs`
- 修改：`crates/kernel/tests/turn.rs`

**接口：**
- 消费：`ContentBlock`、`ModelInputModalities`。
- 产出：`RunInput::Text(String)` 和 `RunInput::Blocks(Vec<ContentBlock>)`。
- 产出：`RunInput::slash_command_text()`、`RunInput::text()`、`RunInput::into_blocks()`。

- [ ] **步骤 1：编写多模态 Prompt 失败测试**

在 `turn.rs` 添加测试模型，捕获一次真实 `ModelRequest`，提交文本加图片的 `RunRequest`，断言用户消息块顺序和数据完全保留：

```rust
assert_eq!(
    user_blocks,
    &[
        ContentBlock::Text { text: "read this".to_string() },
        ContentBlock::Image {
            data: "iVBORw0KGgo=".to_string(),
            mime_type: "image/png".to_string(),
        },
    ],
);
```

在 `slash_commands.rs` 添加测试，证明包含图片的 `/compact` 作为普通用户 Prompt，不执行 Slash Command。

- [ ] **步骤 2：运行 Kernel 目标测试并确认红灯**

```bash
rtk cargo test -p kernel --test turn image_prompt -- --nocapture
rtk cargo test -p kernel --test slash_commands image_prompt -- --nocapture
```

预期：编译失败或断言失败，因为 `RunInput` 仍把复合输入保存为字符串。

- [ ] **步骤 3：实现内容块输入路径**

将 `RunInput::Composite(String)` 替换为 `RunInput::Blocks(Vec<ContentBlock>)`。纯文本继续经过 Bash、Slash、Input Hook、Skill 和 Template；Blocks 路径跳过这些文本处理，直接生成 `MessageContent::User { blocks }`。`PromptInputExpansion` 增加从 `RunInput` 生成消息内容的关联方法，不创建只调用一次的自由 helper。

```rust
match request.input {
    RunInput::Text(text) => session.expand_prompt_input(&request.session_id, &text)?,
    RunInput::Blocks(blocks) => PromptInputExpansion::from_blocks(blocks),
}
```

Blocks 中只有一个 Text 时也不能重新获得命令资格；命令资格由输入来源显式决定，避免 ACP 复合输入被意外执行。

- [ ] **步骤 4：运行目标测试并确认绿灯**

```bash
rtk cargo test -p kernel --test turn image_prompt -- --nocapture
rtk cargo test -p kernel --test slash_commands image_prompt -- --nocapture
rtk cargo test -p kernel --test slash_commands
```

预期：图片顺序保留，图片 Prompt 不执行命令，既有 Slash 测试通过。

- [ ] **步骤 5：复核任务差异**

```bash
rtk git diff -- crates/protocol/src/session.rs crates/kernel/src/runtime/input.rs crates/kernel/src/runtime/run.rs crates/kernel/tests/turn.rs crates/kernel/tests/slash_commands.rs
```

预期：只改变输入表示和相应测试，不修改 Provider。

### 任务 3：模型边界按能力降级图片

**文件：**
- 修改：`crates/protocol/src/model.rs`
- 修改：`crates/kernel/src/runtime/run.rs`
- 修改：`crates/kernel/src/runtime/compaction.rs`
- 修改：`crates/kernel/src/runtime/mcp.rs`
- 修改：`crates/kernel/tests/turn.rs`
- 修改：`crates/kernel/tests/provider_runtime.rs`

**接口：**
- 产出：`ModelRequest::adapt_input(&mut self, profile: &ModelProfile) -> bool`，返回是否发生图片降级。
- 保证：只修改请求副本，不修改 Kernel history、Store 或 ACP 回放。

- [ ] **步骤 1：编写 text-only 降级失败测试**

在 `turn.rs` 使用 text-only Profile 捕获模型请求，断言模型看到占位符，但 `kernel.session_messages` 仍保存原图片；另加连续图片只生成一个占位符的断言。生产改动若漏掉降级或改写历史，这些断言必须失败。

```rust
assert_eq!(model_user_blocks, &[ContentBlock::Text {
    text: "(image omitted: model does not support images)".to_string(),
}]);
assert!(matches!(stored_user_blocks[0], ContentBlock::Image { .. }));
```

在 `provider_runtime.rs` 使用 image Profile 断言图片仍转换为 Provider 的 base64 Image，而不是占位符。

- [ ] **步骤 2：运行目标测试并确认红灯**

```bash
rtk cargo test -p kernel --test turn text_only_model -- --nocapture
rtk cargo test -p kernel --test provider_runtime image_capable_model -- --nocapture
```

预期：text-only 模型仍收到图片，导致断言失败。

- [ ] **步骤 3：实现请求副本降级**

在 `ModelRequest` 的关联方法中遍历 User 和 ToolResult 的块，将连续 Image 合并成一个占位 Text，保留所有其他块和顺序。普通 Turn 在 Context Hook 完成后调用；Compaction 与 MCP 模型请求也在调用模型前调用，确保所有模型入口一致。

```rust
let downgraded = request_for_model.adapt_input(turn_model.profile());
if downgraded {
    tracing::debug!(
        "replaced unsupported images before calling model {}/{}",
        turn_model.profile().provider_id,
        turn_model.profile().model_id
    );
}
```

- [ ] **步骤 4：运行目标测试并确认绿灯**

```bash
rtk cargo test -p kernel --test turn text_only_model -- --nocapture
rtk cargo test -p kernel --test provider_runtime image_capable_model -- --nocapture
rtk cargo test -p kernel --test compaction
rtk cargo test -p kernel --test mcp_host
```

预期：text-only 请求降级、历史保留、image 模型原样发送。

- [ ] **步骤 5：复核任务差异**

```bash
rtk git diff -- crates/protocol/src/model.rs crates/kernel/src/runtime/run.rs crates/kernel/src/runtime/compaction.rs crates/kernel/src/runtime/mcp.rs crates/kernel/tests/turn.rs crates/kernel/tests/provider_runtime.rs
```

预期：没有把降级写进存储或 ACP 层。

### 任务 4：多模态 follow-up 队列

**文件：**
- 修改：`crates/kernel/src/runtime/queue.rs`
- 修改：`crates/kernel/src/runtime/extension/host.rs`
- 修改：`crates/kernel/tests/lifecycle_order.rs`
- 修改：`crates/kernel/tests/retry.rs`

**接口：**
- 修改：`Kernel::queue_message(&SessionId, QueueKind, RunInput) -> Result<QueuedMessage, KernelError>`。
- 保证：Text 继续支持 Skill/Template 展开和 Slash 拒绝；Blocks 直接排队完整用户消息。

- [ ] **步骤 1：编写图片队列失败测试**

在 `lifecycle_order.rs` 排入带文本和图片的 follow-up，完成当前 Turn 后断言下一 Turn 收到原始块，并且 pending snapshot 已自动移除。测试通过 Store 恢复路径再次加载，证明 base64 内容未丢失。

```rust
assert_eq!(pending.follow_up[0].message.content, MessageContent::User {
    blocks: image_blocks.clone(),
});
assert!(kernel.pending_messages(&session_id)?.follow_up.is_empty());
```

- [ ] **步骤 2：运行队列测试并确认红灯**

```bash
rtk cargo test -p kernel --test lifecycle_order queued_image -- --nocapture
```

预期：编译失败，因为 queue 只接受 String。

- [ ] **步骤 3：实现队列多模态输入**

`queue_message` 接受 `RunInput`。Text 分支保持现有 Slash 检查和展开；Blocks 分支直接构造 `MessageContent::User`。扩展 Host 现有文本 API 使用 `RunInput::Text(text)` 适配，不升级第三方扩展契约。

- [ ] **步骤 4：运行队列相关测试并确认绿灯**

```bash
rtk cargo test -p kernel --test lifecycle_order queued_image -- --nocapture
rtk cargo test -p kernel --test lifecycle_order
rtk cargo test -p kernel --test retry
rtk cargo test -p kernel --test session_capabilities
```

预期：图片 queue 可恢复、消费后清除，所有现有队列行为不回归。

- [ ] **步骤 5：复核任务差异**

```bash
rtk git diff -- crates/kernel/src/runtime/queue.rs crates/kernel/src/runtime/extension/host.rs crates/kernel/tests/lifecycle_order.rs crates/kernel/tests/retry.rs
```

### 任务 5：ACP 图片校验、能力和日志脱敏

**文件：**
- 创建：`crates/acp/src/input.rs`
- 修改：`crates/acp/src/lib.rs`
- 修改：`crates/acp/src/server.rs`
- 修改：`crates/acp/src/extension.rs`
- 修改：`crates/acp/src/trace/parameters.rs`
- 修改：`crates/acp/Cargo.toml`
- 修改：`crates/protocol/src/acp.rs`
- 修改：`crates/protocol/src/lib.rs`
- 修改：`crates/acp/tests/extension.rs`
- 修改：`crates/acp/tests/tracing.rs`
- 修改：`crates/acp/tests/mapping.rs`

**接口：**
- 产出：ACP 内部 `PromptInput(RunInput)`，同时被标准 Prompt 和 follow-up 扩展复用。
- 修改：follow-up 参数为 `{ sessionId, prompt: ContentBlock[] }`。
- 产出：ACP 初始化能力 `session.prompt.image = {}`。

- [ ] **步骤 1：编写 ACP 入站和能力失败测试**

在 `extension.rs` 的生产 Server 集成夹具中增加请求类型，测试初始化结果包含图片能力；发送 Text+Image Prompt 后捕获模型请求并断言图片块；运行中发送 follow-up 图片后断言 pending snapshot 保存图片。

```rust
assert_eq!(initialize["capabilities"]["session"]["prompt"]["image"], serde_json::json!({}));
assert_eq!(captured_image.mime_type, "image/png");
```

再加入表驱动 invalid params：无效 base64、`image/svg+xml`、6 张图、单图超过 10 MiB、总计超过 20 MiB。

- [ ] **步骤 2：运行 ACP 测试并确认红灯**

```bash
rtk cargo test -p acp --test extension image_prompt -- --nocapture
rtk cargo test -p acp --test extension image_limits -- --nocapture
```

预期：初始化无 image 能力，Image Prompt 返回 unsupported content，follow-up schema 不接受 prompt 数组。

- [ ] **步骤 3：实现统一 ACP Prompt 转换和校验**

新增 `input.rs`，从 `Vec<wire::ContentBlock>` TryFrom 为 `PromptInput`。Text 保留，ResourceLink 继续转换为 Markdown Text，Image 校验 MIME、base64 和解码大小后转成协议 Image。Audio、Resource 和未知块返回 capability 错误。

```rust
impl TryFrom<Vec<wire::ContentBlock>> for PromptInput {
    type Error = PromptInputError;

    /// Validates ACP prompt media before it reaches the Kernel.
    fn try_from(blocks: Vec<wire::ContentBlock>) -> Result<Self, Self::Error> {
        let mut model_blocks = Vec::new();
        let mut text_parts = Vec::new();
        let mut image_count = 0_usize;
        let mut total_image_bytes = 0_usize;
        let mut text_only = true;
        for block in blocks {
            match block {
                wire::ContentBlock::Text(text) => {
                    text_parts.push(text.text.clone());
                    model_blocks.push(ContentBlock::Text { text: text.text });
                }
                wire::ContentBlock::Image(image) => {
                    text_only = false;
                    image_count = image_count.saturating_add(1);
                    if image_count > MAX_IMAGES {
                        return Err(PromptInputError::TooManyImages);
                    }
                    let data = BASE64_STANDARD
                        .decode(image.data.as_bytes())
                        .map_err(PromptInputError::InvalidBase64)?;
                    if data.len() > MAX_IMAGE_BYTES {
                        return Err(PromptInputError::ImageTooLarge);
                    }
                    total_image_bytes = total_image_bytes.saturating_add(data.len());
                    if total_image_bytes > MAX_TOTAL_IMAGE_BYTES {
                        return Err(PromptInputError::ImagesTooLarge);
                    }
                    model_blocks.push(ContentBlock::Image {
                        data: image.data,
                        mime_type: image.mime_type.to_string(),
                    });
                }
                wire::ContentBlock::ResourceLink(resource) => {
                    text_only = false;
                    model_blocks.push(ContentBlock::Text {
                        text: format!("[{}]({})", resource.name, resource.uri),
                    });
                }
                _ => return Err(PromptInputError::UnsupportedContent),
            }
        }
        if model_blocks.is_empty() {
            return Err(PromptInputError::Empty);
        }
        Ok(Self(if text_only {
            RunInput::Text(text_parts.join("\n\n"))
        } else {
            RunInput::Blocks(model_blocks)
        }))
    }
}
```

标准 Prompt 和扩展 follow-up 都调用该 TryFrom。删除 protocol crate 中字符串型 `AcpQueueMessageParameters`，参数结构留在 ACP crate，避免 protocol 依赖 ACP wire schema。

- [ ] **步骤 4：声明标准图片能力**

初始化构造改为：

```rust
wire::SessionCapabilities::new()
    .delete(wire::SessionDeleteCapabilities::new())
    .prompt(
        wire::PromptCapabilities::new()
            .image(wire::PromptImageCapabilities::new()),
    )
```

- [ ] **步骤 5：编写日志脱敏失败测试**

在 `tracing.rs` 发送含唯一 base64 标记的图片 Prompt，断言 DEBUG 日志包含图片字节摘要但不包含标记；现有凭据字段递归脱敏断言保持通过。

```rust
assert!(!logs.contains("UNIQUE_IMAGE_BASE64_MARKER"));
assert!(logs.contains("image data omitted"));
```

- [ ] **步骤 6：运行日志测试并确认红灯**

```bash
rtk cargo test -p acp --test tracing image_data -- --nocapture
```

预期：完整 data 仍出现在 DEBUG 参数日志，断言失败。

- [ ] **步骤 7：实现上下文相关图片 data 脱敏**

`RedactSensitive` 处理对象时先判断 `type == "image"`，只把该对象的 `data` 替换为摘要；普通扩展对象的 `data` 保持现有完整 DEBUG 行为。禁止用全局 key 黑名单误伤所有 data 字段。

- [ ] **步骤 8：运行 ACP 测试并确认绿灯**

```bash
rtk cargo test -p acp --test extension image_prompt -- --nocapture
rtk cargo test -p acp --test extension image_limits -- --nocapture
rtk cargo test -p acp --test tracing image_data -- --nocapture
rtk cargo test -p acp --test mapping
rtk cargo test -p acp
```

预期：能力、图片 Prompt、follow-up、限制和日志测试全部通过。

- [ ] **步骤 9：复核依赖和差异**

```bash
rtk git diff -- Cargo.toml crates/acp/Cargo.toml crates/acp/src crates/acp/tests crates/protocol/src/acp.rs crates/protocol/src/lib.rs
```

预期：`base64` 从 workspace dependency 继承，依赖表分组和排序符合仓库规则。

### 任务 6：WebUI 图片附件、排队和展示

**文件：**
- 修改：`web/src/acp/protocol.ts`
- 修改：`web/src/domain/model.ts`
- 修改：`web/src/workspace/controller.ts`
- 修改：`web/src/workspace/updateDecoder.ts`
- 修改：`web/src/features/composer/Composer.tsx`
- 修改：`web/src/features/conversation/MessageView.tsx`
- 修改：`web/src/theme/workbench.css`

**接口：**
- 产出：`ImageContentBlock` 和 `PromptImage`。
- 修改：`PromptInput` 增加 `images: readonly PromptImage[]`。
- 保证：非运行状态使用 `session/prompt`；运行状态使用 `{ sessionId, prompt }` follow-up；两者块数组完全一致。

- [ ] **步骤 1：添加 WebUI 强类型内容块**

```ts
export type ImageContentBlock = Readonly<{
  type: "image";
  data: string;
  mimeType: "image/png" | "image/jpeg" | "image/gif" | "image/webp";
}>;

export type PromptImage = Readonly<{
  id: string;
  name: string;
  size: number;
  data: string;
  mimeType: ImageContentBlock["mimeType"];
}>;
```

`PromptContentBlock` 包含 Image；`MessageEntity` 增加 `images`；所有构造 MessageEntity 的 decoder 分支显式保留已有 images。

- [ ] **步骤 2：实现 Composer 文件读取和限制**

使用隐藏 `input[type=file]` 和 `accept="image/png,image/jpeg,image/gif,image/webp"`。选择时先校验最多 5 张、单张 10 MiB、总计 20 MiB，再通过 `FileReader.readAsDataURL` 读取并剥离逗号前缀。文件读取逻辑写成 `PromptImage.fromFile` 不适用于 TypeScript 类型，因此直接放在 Composer 事件处理附近，避免增加多个一次性 helper。

图片草稿只存在 React state；`StoredDraft` 仍只写 text/resources。提交成功后清空图片，失败时保留。

- [ ] **步骤 3：统一发送和 follow-up 协议**

Controller 构造一次 `PromptContentBlock[]`：Text、ResourceLink、Image。运行中调用扩展 follow-up 时发送 `prompt`，不再把资源或图片展平为字符串。

```ts
const prompt: PromptContentBlock[] = [
  ...(input.text.length === 0 ? [] : [{ type: "text", text: input.text }]),
  ...input.resources.map((resource) => ({ type: "resource_link", ...resource })),
  ...input.images.map(({ data, mimeType }) => ({ type: "image", data, mimeType })),
];
```

- [ ] **步骤 4：展示消息图片和队列摘要**

Decoder 从 whole-message `content` 中提取白名单 Image；MessageView 在正文下渲染缩略图，`src` 仅由受控 MIME 和 base64 组成。QueuedMessageView 在文本后显示 `N 张图片`，不把 base64 拼入 DOM 文本。

- [ ] **步骤 5：添加样式并运行 WebUI 检查**

```bash
rtk npm run check --prefix web
rtk npm run build --prefix web
```

预期：TypeScript、ESLint 和 Vite 构建全部退出 0，无 warning。

- [ ] **步骤 6：复核 WebUI 差异**

```bash
rtk git diff -- web/src/acp/protocol.ts web/src/domain/model.ts web/src/workspace/controller.ts web/src/workspace/updateDecoder.ts web/src/features/composer/Composer.tsx web/src/features/conversation/MessageView.tsx web/src/theme/workbench.css
```

预期：没有动态 import、`any`、图片 sessionStorage 持久化或非白名单 MIME。

### 任务 7：全量验证和 kimi-k3 真实端到端验收

**文件：**
- 不新增生产文件。
- 临时生成：`/tmp/clawcode-kimi-k3-vision.png`，验收结束后删除。

**接口：**
- 消费：生产 `claw.toml` 中 `moonshotai/kimi-k3`。
- 产出：可复现的测试命令、日志和模型回答证据。

- [ ] **步骤 1：格式化并运行后端全量检查**

```bash
rtk cargo fmt --all -- --check
rtk cargo test --workspace
rtk cargo clippy --workspace --all-targets -- -D warnings
```

预期：全部退出 0；若 fmt check 失败，先运行 `rtk cargo fmt --all`，再重新执行完整三条命令。

- [ ] **步骤 2：重新运行 WebUI 检查**

```bash
rtk npm run check --prefix web
rtk npm run build --prefix web
```

预期：全部退出 0。

- [ ] **步骤 3：生成唯一答案测试图片**

使用可用的本地图片工具生成白底黑字 PNG，内容为 `CLAW-7319`。如果系统没有 ImageMagick，则使用已安装 Python Pillow 生成；临时脚本写入 `/tmp`，执行后删除脚本。图片文件保留到 E2E 结束。

```text
画布：640x240，白底
文字：CLAW-7319，黑色，居中，字号至少 64
```

- [ ] **步骤 4：启动生产 app 后端**

使用仓库现有生产启动命令和 `claw.toml`，监听一个未占用本地端口。确认启动日志显示 active provider/model 为 `moonshotai/kimi-k3`，日志中不得出现 API key 或图片 data。

- [ ] **步骤 5：通过真实 WebUI 发送图片**

使用 in-app Browser 打开生产 WebUI，新建 Session，选择 `/tmp/clawcode-kimi-k3-vision.png`，发送：

```text
只回答图片中的完整编号，不要解释。
```

等待真实 Provider 返回，断言回答包含精确字符串 `CLAW-7319`。HTTP 2xx、图片回显或占位符不算成功。

- [ ] **步骤 6：验证运行中图片 follow-up 和自动清除**

启动一个足够长的真实 Turn，在 running 状态选择同一图片并发送 follow-up。确认 pending 区出现 `1 张图片`，消息被消费后自动消失，模型后续回答仍包含 `CLAW-7319`。

- [ ] **步骤 7：检查运行日志和会话回放**

关闭并重新打开 Session，确认用户图片仍可展示。搜索运行日志，确认没有测试图片 base64 前缀，只有脱敏摘要；确认 text-only 占位符没有出现在 kimi-k3 请求对应的模型回答上下文。

- [ ] **步骤 8：停止服务并清理临时文件**

停止生产 app 进程，删除 `/tmp/clawcode-kimi-k3-vision.png` 和临时生成脚本。不得删除会话数据，保留真实验收会话供用户复查。

- [ ] **步骤 9：最终差异和需求复核**

```bash
rtk git status --short
rtk git diff --check
rtk git diff --stat
```

逐项确认：配置能力、Kernel 图片保留、text-only 降级、图片队列、ACP 标准能力、限制、日志脱敏、WebUI 附件和展示、kimi-k3 真实识图均有新鲜证据。不得创建 commit。
