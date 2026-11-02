# Provider 改用 rig registry 与模型目录的设计规格

日期：2026-11-01。状态：设计已获口头确认，待审阅本文。前置：[rig-core 迁移设计](./2026-10-30-rig-core-provider-migration-design.md)。本文推翻其中"会话级 Prepared 缓存"的决定（见 §4），其余约束继续有效。

## 1. 目标与已确认决策

目标：每个 agent（主会话、子代理）都能动态切换模型；用户能以尽量少的配置使用 rig 支持的任意 provider 和兼容端点。

| 决策 | 结论 |
|---|---|
| 构造方式 | 每次请求从 rig `ProviderConfig` 现建模型（方案 A）；不用 `Catalog::connect_with`（无法指定路由与 idle timeout，且隐式读取环境变量） |
| 模型列表 | 只显示配置里列出的模型；省略的元数据从 rig 内置目录补齐 |
| Thinking Off | Off 表示不发送推理参数，由模型默认值和 `extra_param` 决定；其余级别映射为 rig 同名 Effort，模型不支持的级别同样不发送 |
| 会话级 Prepared 缓存 | 删除。它只是 rig 历史适配的增量计算缓存，不影响请求内容；子代理本就共享父会话句柄，隔离并不成立 |
| 子代理模型 | `subagent`、`background_subagent` 增加可选 `model` 参数 |
| preset 与本地标识 | 新增可选 `preset`（rig 中称 vendor）；`id` 只作本地选择键 |
| 凭证 | 认证与凭证缓存只在 provider crate；kernel 不持有、不接触凭证 |
| 依赖边界 | 只有 protocol、provider、extension 直接依赖 rig；其他 crate 经 protocol 白名单使用；由 pre-commit 检查保证（§9） |
| 兼容性 | 现有 `claw.toml` 无需修改；`id` 与 rig preset 同名的旧 provider 改用 preset 方言，见 §2.5 |

## 2. 配置

config crate 继续不依赖 rig；与 rig 相关的解析全部在 provider crate 完成。

### 2.1 Provider 字段

| 字段 | 变化 | 含义 |
|---|---|---|
| `id` | 由枚举改为字符串（`ProviderId` 新类型） | 本地唯一标识，模型选择键为 `id/model`；不得为空、首尾不得有空白、不得含 `/` |
| `preset` | 新增，可选，默认等于 `id` | rig 预设名：决定方言细节与目录元数据来源 |
| `provider_type` | 改为可选；新增 `gemini` | 协议：`responses`、`openai-completions`、`anthropic`、`gemini` |
| `base_url` | 改为可选 | 省略时使用 rig 预设地址 |
| `display_name`、`api_key`、`auth` | 不变 | — |

### 2.2 preset 与协议的解析规则

在 provider crate 中按下列顺序得到 rig `ProviderConfig`：

1. `preset = provider.preset` 或 `provider.id`。
2. 有 `provider_type` 时取 rig `ProviderId::new(preset, format)`；没有时取 `ProviderId::catalog(preset)`，即 rig 为该 preset 声明的主协议。不用 `ProviderId::resolve`：它对注册了两种协议的 preset（minimax、moonshot、zai、xiaomimimo、ollama、opencode）返回歧义错误。
3. 得到可配置的组合：用 `ProviderId::config(key)` 的预设（`config` 对 catalog-only 的 id 返回 None，按第 4 步处理）。
4. 未得到（preset 不被 rig 认识、该组合未注册或仅有目录条目）：`base_url` 必填，否则该 provider 的所有模型构建失败；`provider_type` 省略时按 `responses`（保持旧默认）。使用该协议的通用方言：OpenAI 系为 `openai::wire::OPENAI` 基线，Messages 为 `anthropic::wire::ANTHROPIC`，Gemini 为 `GeminiConfig`。若 rig 认识该 preset（只是没有这一协议的预设），构建时记录 warn，写明 provider 与协议。
5. 有 `base_url` 时覆盖；OpenAI 系上 `responses` → `Route::Responses`，`openai-completions` → `Route::Chat`，省略时保留方言与目录的默认路由。

示例：

| 场景 | 写法 |
|---|---|
| 官方端点 | `id = "deepseek"` |
| 已知 preset 的另一种协议 | `id = "moonshot"`, `provider_type = "anthropic"` |
| 已知 preset 的代理 | `id = "ds-proxy"`, `preset = "deepseek"`, `base_url = "…"` |
| 通用兼容端点 | `id = "corp"`, `provider_type = "openai-completions"`（省略则为 `responses`）, `base_url = "…"`，模型须写 limits |
| 网关以其他协议转发已知厂商模型 | `id = "litellm"`, `preset = "anthropic"`, `provider_type = "openai-completions"`, `base_url = "…"` |

ChatGPT 认证要求 `preset` 为 `chatgpt` 且协议为 Responses（省略即可）；原来基于 `id` 的检查改为基于 preset。

### 2.3 模型元数据

`context_tokens`、`max_output_tokens`、`input` 改为可选，并新增可选的 `request_max_tokens`（`input` 由带默认值的 `Vec` 改为 `Option`）。provider crate 用 `Catalog::builtin().get(ProviderId::catalog(preset), upstream_id)` 查目录，逐项合并，配置值优先：

| ModelProfile 字段 | 来源顺序 |
|---|---|
| `context_tokens` | 配置 → 目录 `input_limit()`（`max_input_tokens` 与 `context_window` 取小者） |
| `max_output_tokens` | 配置 → 目录 `max_output_tokens`；仅作能力元数据，不直接发送 |
| `request_max_tokens` | 配置 → `min(max_output_tokens, compaction.reserve_tokens)`；每个 Turn 作为 `max_tokens` 发送 |
| `input` | 配置 → 目录 `input`（文本，另含图片时加 Image）→ 仅文本 |
| `display_name` | 配置 → 目录 `display_name` → 模型 id |

两项 limits 任一缺失时该模型构建失败，原因写明缺少的字段、查询的 `preset/upstream_id` 及"配置与 rig 目录均未提供"。

`request_max_tokens` 与 compaction 预留绑定（用户决定）：compaction 保证输入不超过 `context_tokens − reserve_tokens`，预算不超过 `reserve_tokens`，两者之和就不会超出窗口（仅在 compaction 开启时成立）。需要更长输出时，同时调大 `reserve_tokens`（compaction 随之提前）与 `request_max_tokens`。输出被截断（`MaxTokens`）时，"窗口溢出"的判定改为与 `request_max_tokens` 比较，正常用完预算不再误判为溢出。删除 DeepSeek 仅文本输入的硬编码特判，由目录决定。

### 2.4 校验时机

配置加载时只校验已填写的值：limits 与 `request_max_tokens` 非零、`request_max_tokens` 不超过 `compaction.reserve_tokens`、`upstream_id` 非空、模型 id 不重复、`id` 与 `preset` 是不含 `/` 与首尾空白的非空名字、`active_model` 指向已配置条目、认证来源有效。provider 构建时另查 `request_max_tokens` 不超过 `max_output_tokens`（后者可能来自目录）。依赖目录的检查在 provider 构建时进行，失败沿用 `ProviderFactory` 现有的按模型记录、解析时报错机制。`KernelFactory::build` 启动时已调用 `create_catalog` 解析全部模型，因此任一模型构建失败仍会让应用启动失败，报错时机与今天"配置加载即校验全部模型"一致，无需新增机制。compaction 预留与当前模型上下文的比较，以及当前模型 `request_max_tokens` 不超过预留的检查，也在 `KernelFactory::build` 中进行，使用 kernel 实际采用的 `CompactionPolicy`，避免预算默认值（取自配置）与运行期预留不一致。恢复会话时若持久化的模型已不在配置中，保留之前选中的或当前活动模型并记录 warn，不再拒绝打开会话。

`ModelProfile` 改由 provider crate 组装（`ConfiguredModel::profile()`）；config 的 `LlmModel::to_profile` 与 `AppConfig::model_profile` 移除，config crate 中依赖它们的测试迁到 provider crate。app 的 UI bootstrap 改为先建 `ProviderModelFactory`，再从中取当前模型的 profile。

### 2.5 兼容性

- 现有字段与取值全部保留，现有 `claw.toml` 无需修改。
- 例外：每个 Turn 发送的 `max_tokens`。以前发送 `max_output_tokens`，现在发送 `request_max_tokens`，未配置时为 `min(max_output_tokens, reserve_tokens)`，通常更小（例如 384000 → 16384）。恢复更大输出需同时调大 `compaction.reserve_tokens` 与 `request_max_tokens`。README 写明这一升级差异。
- 省略 `provider_type` 的含义：以前一律按 `responses`，只有 openai、chatgpt 与自定义 id 能因此工作，其余 preset 直接报错。现在 rig 认识的 preset 用其主协议（openai、chatgpt 仍为 Responses），未知 preset 仍按 `responses`。例外：`id` 与 rig preset 同名、但以前不在内置列表中的 provider（如 openrouter、groq、mistral、xai、ollama），以前走通用 OpenAI 方言（省略协议时为 Responses），现在改用该 preset 的方言与默认路由；设 `preset` 为 rig 不认识的名字（如 `"custom"`）可恢复旧行为。README 说明新规则与这一升级差异。

## 3. provider crate

`ConfiguredModel` 持有 profile、upstream_id、rig `ProviderConfig`（已含 key、base_url、路由）、`extra_param`、共享 http、可选 Authenticator、idle timeout。删除 `Definition`、`ProviderCompletion`、`build_model` 与 `ProviderConnectError::MissingAuthentication`。

新增 `async fn connect(&self, http: DynHttpClient) -> Result<DynModel<Completion>, ProviderConnectError>`：

1. 有 Authenticator 时，用基础 transport（不经扩展 middleware）解析凭证。凭证缓存、刷新锁与持久化由该 provider 共享的 rig `Authenticator` 负责，provider crate 不另建缓存。
2. 克隆 `ProviderConfig`，有凭证时写入 key、account_id 与 API base。
3. 按 OpenAI、Messages、Gemini 三个协议族调用 `connect(http).completion(upstream_id)`，设置 idle timeout 后擦除。

`auth_context` 改为 provider crate 内部方法，`AuthContext` 不出现在 provider 的公开接口中。保持不变：`prepare`（参数合并）、`ProviderFactory::resolve`、每个 provider 共享一个 Authenticator。

## 4. kernel：每次请求构造模型

- `ProviderModel` 只保留 `Arc<ConfiguredModel>`，无锁、无缓存、无凭证状态，可被多个会话安全共享。
- `stream_request`：准备请求 → 取带 middleware 的 http → 可取消地 `connect`（认证在其中完成）→ `stream`。其余流处理、失败分类、日志不变。
- `ProviderModelFactory::create`、`create_catalog` 改用 `ConfiguredModel::profile()`。
- 删除只验证缓存归属的测试：`kernel/src/provider.rs` 中两个缓存单测、`kernel/tests/prepared_cache.rs`。

代价：rig 不再跨请求复用历史适配结果，每次请求重新适配一遍历史。项目本就每次全量重建消息，额外开销为本地 CPU，与模型调用延迟相比可忽略。

## 5. Thinking 级别

- `protocol::ThinkingLevel` 派生 `Default`（Medium，用户决定）；`ModelRequestOptions` 增加 `thinking_level: ThinkingLevel`。没有持久化级别的会话（新建或从未切换过）以 `ThinkingLevel::default()` 起步。
- 普通 Turn 改用 `SessionModelState::snapshot()`（`run.rs` 当前用 `active()`），使模型与级别取自同一快照；压缩与 MCP sampling 显式设为 Off，不随默认级别变化。
- `ThinkingLevel::generation_options(self) -> Option<rig_core::completion::options::GenerationOptions>`：Off → None；Minimal、Low、Medium、High、XHigh → 同名 `Effort`，并设 `on_unsupported = Ignore`。kernel 的 `ProviderRequest` 在有值时调用 `CompletionRequest::options`，不必引用 rig 的选项类型。
- rig 按方言与目录把推理强度翻译为各家参数。rig 对不支持的选项默认使请求失败，例如 DeepSeek 只接受 low、high、max；设为 Ignore 后，模型不支持所选级别时该参数不发送（rig 记录 warn），请求照常进行，效果等同 Off。
- 目录列出模型的档位（`ReasoningSupport::Listed`）时，provider 的 `prepare` 先把级别吸附到不高于它的最高档，没有则取最低档，保证级别越高、发送的推理不会越少：DeepSeek 上 Minimal、Medium 发 low，XHigh 发 high；Anthropic Opus 的 xhigh 保持不变。目录未列档位的模型仍由 Ignore 兜底。
- `extra_param` 优先于映射结果，覆盖按顶层键进行：例如 `extra_param` 中的 `thinking` 会整体替换 rig 生成的 `thinking`。

## 6. 子代理模型参数

- `subagent` 参数增加可选 `model`；`background_subagent` 的 `spawn` 动作同样增加可选 `model`。格式为 `provider/model`，必须是已配置模型；省略时继承父会话当前模型（现状）。
- 在父会话的模型目录中解析；不存在时返回工具错误，并列出可用的 `provider/model`。
- 解析结果经 `create_owned_session` 传入子会话，替换继承的模型；子会话的模型设置照常持久化。thinking 级别仍继承父会话。

## 7. 错误与日志

- 构建失败的原因包含 provider/model、缺失字段或无法解析的 preset/协议组合；不含密钥。
- 不新增日志点；沿用现有构建失败、请求失败与认证失败的日志。

## 8. 测试与验收

按测试先行实施，测试放在 crate 级 `tests/` 或 `#[cfg(test)] mod tests`。

| 编号 | 场景 | 必须证明的结果 |
|---|---|---|
| B01 | 最小配置 | 只写 `id = "deepseek"`、key 与模型 id 的配置可加载，limits 来自目录 |
| B02 | 配置覆盖目录 | 配置的 limits、input、display_name 优先于目录 |
| B03 | 缺元数据 | 目录与配置均无 limits 的模型构建失败，原因写明缺失字段 |
| B04 | preset 解析 | §2.2 五种示例都能构造；不写 `provider_type` 的 minimax、moonshot 走 rig 主协议；未知 preset 缺 `base_url` 时失败，缺 `provider_type` 时按 `responses` |
| B05 | 新协议族 | gemini 与 openrouter 可构造并对本地 HTTP 测试服务发出请求 |
| B06 | ChatGPT 凭证 | 凭证写入请求；preset 不是 chatgpt 时认证配置被拒绝 |
| B07 | Thinking | DeepSeek 上 High 时请求体含 `reasoning_effort: "high"`；Off 时不含推理参数；Medium（不受支持）时不含推理参数且请求成功；`extra_param` 优先 |
| B08 | 子代理模型 | 指定已配置模型时子会话使用该模型；未配置模型返回列出可用模型的工具错误；省略时继承 |
| B09 | 兼容性 | 仓库现有 `claw.toml` 与现有测试配置不经修改即可加载 |
| B10 | 依赖边界 | 边界检查在当前仓库通过；在 kernel 的 `[dependencies]` 加 rig-core，或在 kernel `src` 写 `rig_core::` 时检查失败 |
| B11 | 工程检查 | workspace 测试、fmt、Clippy、全部 pre-commit hooks 通过；README 更新配置说明 |
| B12 | 请求预算 | 未配置时预算为 `min(max_output_tokens, reserve_tokens)` 并作为 `max_tokens` 发送；配置值原样发送；超过 `reserve_tokens` 时配置加载失败，超过 `max_output_tokens` 时模型构建失败，超过 kernel 预留时启动失败；输出用完预算的 `MaxTokens` 不判为窗口溢出 |

## 9. 依赖边界（实施最后阶段）

目标：rig 不向其他 crate 扩散；需要 rig 类型的地方经 protocol 的白名单使用。

| crate | 规则 |
|---|---|
| protocol、provider、extension | 可在 `[dependencies]` 直接依赖 rig-core / rig-reqwest |
| 其他 crate（kernel、app、extensions 等） | `[dependencies]`、`[build-dependencies]` 及目标平台依赖中不得出现 rig；`src` 下不得出现 `rig_core::`、`rig_reqwest::` |
| 所有 crate 的 `[dev-dependencies]` | 不限制，测试可使用 rig 的 test-utils 等 |

- protocol 新增 `llm` 模块，保留 rig 的子模块结构（`llm::message`、`llm::completion`、`llm::streaming`），在各子模块中逐项 `pub use`，禁止整体导出 rig。保留子模块是为了避免与 protocol 自有类型重名，例如 rig 的 `message::StopReason` 与 `protocol::StopReason`。按当前用量，导出范围为 `message` 中的 `AssistantMessage`、`AssistantContent`、`Message`、`UserContent`、`ToolCall`、`ToolFunction`、`ToolName`、`CallId`、`ToolResult`、`ToolResultContent`、`Reasoning`、`ImageMediaType`、`MimeType`、`StopReason`，`completion` 中的 `CompletionRequest`、`ToolDefinition`、`FinishReason`，`streaming` 中的 `Item`、`StreamEvent`。实施时以 kernel 实际用量为准增减，新增条目即是一次显式审查。
- extension 为扩展作者导出 middleware 契约（`HttpMiddleware`、`Exchange`、`http_client` 模块及 middleware 实现所需的 future 类型）；`extensions` crate 改为经 `extension::` 使用，并从 `[dependencies]` 移除 rig-core。
- kernel 移除 rig-core 的 `[dependencies]` 条目，`src`（含 `#[cfg(test)] mod tests`）中的 `rig_core::` 全部改为 `protocol::llm::`。
- 检查：脚本 `scripts/check-rig-boundary.py`，在 `.pre-commit-config.yaml` 中作为 local hook 注册，`pass_filenames: false`，在 `Cargo.toml` 或 `.rs` 变动时运行。成员与依赖取自 `cargo metadata --no-deps`，因此缩进、表写法、目标平台表与 `package = "rig-core"` 改名都无法隐藏依赖；非白名单 crate 的 `src` 与 `build.rs` 中任何 `rig_core`、`rig_reqwest`、`rig_http` 名称都会报错。脚本每次运行先执行内置自测，再检查 workspace；违规时逐条输出位置并以非零退出。

## 10. 不在范围

- 显示配置之外的模型、在线拉取模型列表、模型选择器 UI 改动。
- Bedrock、Vertex 等需要额外 crate 的 catalog-only provider。
- 价格、计费展示；推理级别之外的可移植选项（cache、verbosity 等）。
