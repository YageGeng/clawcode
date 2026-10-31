# 使用上游 rig-core 替换 provider vendor 的设计规格

日期：2026-10-30。状态：已获实施授权，迁移已落地；验收结果与未验证项见 [验收报告](../reports/2026-10-30-rig-core-provider-migration-verification.md)。

本文落实用户已确认的六项决策及后续追加的代码组织要求，并与 [实施计划](../plans/2026-10-30-rig-core-provider-migration-implementation.md) 配套。用户随后明确授权“开始按照计划实现”。实现直接在现有工作区完成，仍禁止 SubAgent、worktree 和未经授权的 commit。

## 1. 目标与决策

用未经修改的上游 rig-core 替代 `crates/provider` 中的 vendor 实现。保留薄的项目接入层，负责配置、模型选择和与 Kernel 的衔接；供应商编码、流解析、模型消息、usage、错误分类和认证算法归上游所有。

| 决策 | 本次采用的行为 |
|---|---|
| 允许 DeepSeek 图片能力回退 | 直接使用上游 DeepSeek dialect，不增加 vision 补丁或自定义替代 dialect；能力声明同步为上游实际能力 |
| 按最新上游接口适配 | 使用 `DynModel<Completion>`、`StreamEvent`、`CompletionStream::finish()`；移除本地 `Llm/ArcLlm` 和供应商响应泛型擦除 |
| 完整支持 HttpMiddleware | 原生注册上游 middleware，删除请求头白名单、凭证恢复及 JSON-only 请求体限制 |
| usage 跟随上游 | 输入包含缓存读写、输出包含 reasoning；计数保留 `Option`；项目消费方同步调整 |
| reasoning 跟随上游 | 成功响应保留原生 assistant 消息、Origin 和 Native，回放使用上游规则 |
| 认证使用上游能力 | 使用上游 Authenticator、FileTokenStore 和令牌刷新；不实现 Codex auth.json 兼容存储 |

完成标准是当前受支持的模型调用、工具循环、扩展、会话恢复、压缩和 WebUI 在新接入层上通过验收，并删除旧 vendor。编译通过或替换依赖名不构成完成。

## 2. 源码基线与依赖策略

- ClawCode：`e7ec2fec847abd234928039b44a6494f814f43a5`。
- 本地 Rig：`/home/isbest/Documents/WorkSpace/rig`，revision `fc556f982d501eea9d7167398a19386e4884e776`。
- Rig 工作区版本：`0.46.0-rc.1`；最低 Rust 版本声明为 `1.97.1`。本机调研时为 `1.99.1`，不是最低版本实测证明。
- 用户追加决策：不固定 Git revision，持续接收当前大版本的发布更新。`rig-core` 与 `rig-reqwest` 从 crates.io 声明 `version = ">=0.46.0-rc.1, <1"`；保留当前 API 所需的最低版本，允许后续 0.x 正式发布以及 0.46.0 的后续预发布，不自动跨入 1.x。
- `Cargo.lock` 保留已验证版本和校验和，确保普通构建可复现；通过 `cargo update -p rig-core -p rig-reqwest` 更新到范围内的新发布。0.x 的 minor 更新可能包含破坏性变更，更新后必须验证并适配，不宣称仅放宽版本范围就无需维护。
- 上述本地 revision 仅是首次调研/实现的历史依据。生产依赖不使用 Git revision、本机绝对 path、上游修改或 `[patch]` 补丁。
- 根 `[workspace.dependencies]` 唯一声明 `rig-core` 的来源和 `default-features = false`。子 crate 使用 `{ workspace = true }`；只有需要联网的接入层添加 `reqwest`、`rustls` features。协议 crate 只需要原生数据类型。
- 第一阶段不启用上游 `derive`、`audio`、`image` 生成、`websocket`、`tungstenite`、PDF/EPUB 等额外功能。图片输入由消息类型支持，不需要 image-generation feature。
- 不引入 rig facade、rig-agent、rig-ecs、rig-harness、rig-memory 来替换 ClawCode 的调度、工具系统或会话存储。
- 执行时若 revision 不可获取或上游 API 已变化，应先更新本文的基线和差异，不静默换版本。是否改用已发布版本属于后续依赖决策。

已核对的上游依据：

| 契约 | Rig 仓库相对位置 |
|---|---|
| 动态模型与延迟发送 | `crates/rig-core/src/driver/dyn_model.rs`、`src/driver.rs` |
| 流事件与 finish/partial | `crates/rig-core/src/streaming/event.rs`、`src/streaming/mod.rs` |
| 原生消息及历史回放 | `crates/rig-core/src/completion/message.rs`、`src/completion/history.rs` |
| usage 与累计 | `crates/rig-core/src/completion/request.rs` |
| 完整 HTTP middleware | `crates/rig-http/src/http_client/middleware.rs`、`erased.rs` |
| ChatGPT 认证与存储 | `crates/rig-core/src/providers/chatgpt/auth/`、`src/providers/internal/auth/store.rs` |
| DeepSeek 图片降级 | `crates/rig-core/src/providers/openai/wire/chat.rs` 的 `image_rule`、`encodes` |

## 3. 改动范围

### 3.1 必须修改

| 范围 | 职责与边界 |
|---|---|
| 根 `Cargo.toml`、`Cargo.lock`，受影响 crate manifest | 固定上游来源、按语义分组继承依赖、清理仅 vendor 使用的依赖和 features |
| `crates/provider/src/lib.rs`、`factory/mod.rs` | 保留接入 crate；ProviderFactory/ConfiguredModel 共享配置、基础 transport 和认证，提供具体模型构造，不持有 Prepared 历史缓存；删除自建动态 LLM 协议 |
| `crates/provider/src/auth.rs`（新增） | 组装上游认证与项目指定存储目录；不实现 OAuth HTTP 协议或令牌文件格式 |
| `crates/kernel/src/provider.rs`、`provider/content.rs`、`model.rs` | 请求转换、原生流消费、完整消息结算、middleware 接入、工具身份关联 |
| `crates/protocol/src/message.rs`、`model.rs`、`tool.rs`、`lib.rs`，`usage.rs`（新增） | 原生 assistant 消息、usage 包装和版本化读写、无效工具参数、结构化失败信息 |
| `crates/kernel/src/runtime/{attempt,run,retry,settlement,model_state}.rs` | 终态结算、唯一重试调度、usage 累计及消息持久化 |
| `crates/kernel/src/runtime.rs`、`runtime/session.rs`、`model.rs`、`runtime/model_state.rs` | Session 创建/恢复/fork 时通过现有 ModelFactory 构建本会话模型句柄；SessionModelState 按模型持有，禁止复用全局有 Prepared 的运行句柄 |
| `crates/kernel/src/runtime/compaction.rs`、`compaction/{context,prompt}.rs`、`command/session_stats.rs` | 压缩和统计改用上游计数，不重复加缓存；保留模型上下文预算与项目压缩策略 |
| `crates/kernel/src/runtime/session/replay.rs`、`transcript.rs` | 新旧消息读取、原生消息重放；分支、fork 和恢复保留元数据 |
| `crates/kernel/src/runtime/tools/{mod,batch}.rs` | 无效工具 JSON 不执行；工具结果恢复名称与调用 ID，保留 `is_error` |
| `crates/extension/src/{registrar,registry,point,lib}.rs`、`runtime/provider.rs`、`middleware.rs`（新增） | 注册原生 HttpMiddleware 工厂，按请求构建上下文和 transport；删除旧三个 provider hook point |
| `crates/kernel/src/runtime/extension/provider.rs` | 将 Session/Run/Turn/Extension 上下文绑定到请求级 middleware，去除 once-per-logical-request 缓存 |
| `crates/protocol/src/hooks/`、`extension/events/provider.rs`、`extension/result.rs` 及 re-export | 删除旧 ModelRequestHooks、provider 事件、HeaderPatch/Mutation 的已无调用部分 |
| `extensions/src/available/hook_examples/provider.rs` 及注册入口 | 使用原生 middleware 示范完整头部和字节 body 操作 |
| `crates/config/src/{llm,retry,lib}.rs`、配置验证与 `claw.toml` | 认证配置迁移、重试语义明确、模型能力与上游对齐；不改当前活动模型选择或真实密钥 |
| `crates/acp/src/{mapping,projection}.rs` | 可选 usage 的精度安全投影；最终 assistant 始终发送完整 AgentThought（包括空内容）；原生模型历史不直接塞入浏览器事件 |
| `web/src/domain/model.ts`、`workspace/{updateDecoder,sessionState,sessionStaging}.ts`、`features/conversation/AgentUsage.tsx` | 接收未知计数并正确累计、显示，继续以十进制字符串保护整数精度 |
| `README.md`、`THIRD_PARTY_NOTICES.md`，相关测试和示例 | 描述破坏性配置/扩展变化、图片回退、usage 口径与验证边界；按实际剩余派生代码保留许可证声明 |

Store 的通用 JSONL 容器继续透传 payload；优先在协议反序列化边界完成新旧转换。`crates/store/src/model.rs`、`session.rs` 只有在实际读取验证要求时才调整，不改拓扑、序号、恢复或追加协议。

### 3.2 删除范围

移除 `crates/provider/src/{client,completion,http_client,providers,model,telemetry}` 中的 vendor，以及旧 `streaming.rs`、`one_or_many.rs`、`wasm_compat.rs`、`markers.rs`、`prelude.rs`、`json_utils.rs`、`factory/event.rs`。依赖确有剩余调用时先迁移调用再删除，不留下未编译的旧实现。

旧 provider 示例改为薄接入层或上游 API 示例；对本地旧协议类型的测试改为项目边界测试，不将上游整个测试集再次 vendor 进来。

### 3.3 不在范围内

- 新供应商、新模型自动发现、向量库、音视频生成、WebSocket 推理通道。
- Agent 调度、队列、工具审批、MCP、终端、会话树或 WebUI 的独立重构。
- 新的登录管理 UI、账号管理命令、Codex 凭证导入工具。
- 恢复旧会话中从未保存的签名、加密 reasoning 或原生供应商内容。
- 实际升级、删除或覆盖用户现有会话、凭证与配置文件。

### 3.4 代码组织：struct 引用上下文，行为归属到方法

本节落实 AGENTS.md 第 15 条。参考 docparse 当前快照 `fd41de7` 的具体做法，而不要求复制其业务类型：

| docparse 仓库内位置 | 已核对的组织方式 | 本次采用的原则 |
|---|---|---|
| `crates/core/src/table/assemble.rs`：`TableAssembler<'a>` | 借用配置、表格证据和公式切片，`reconstruct`、`locate`、`populate` 组织在同一 impl | 一组操作共享的上下文在构造时绑定，方法只接收本次操作变化的输入 |
| `crates/core/src/ocr/merge.rs`：`OcrMerger<'a>` | 借用 OCR 配置与缺失区域，把合并行为放到 `merge(&self, ...)` | 短期处理优先引用上下文，不为传参方便克隆大对象 |
| `crates/core/src/page.rs`：`PageAnalyzer` | 用 Arc 共享长期配置，`prepare`、`finish`、`compose` 等方法归属同一处理者 | 跨异步任务或返回流的生命周期确有需要时持有所有权/共享句柄 |
| `crates/core/src/ocr/index.rs`：`TextIndex<'a>` | 用 `FromIterator<&TextItem>` 建立借用索引，查询是实例方法 | 构造和转换优先使用标准 trait，避免同一职责散落为 free helper |

迁移新增或修改的 Rust 逻辑必须先确定上下文所属的 struct，再组织其方法。禁止将 config、model、headers、history、取消令牌等同一组上下文在多个自由函数间反复传递；也禁止仅把自由函数搬入无字段工具 struct 作为形式上的满足。

本次职责和生命周期固定如下；已有类型优先复用，不为表中每个方法另建 helper struct：

| 承担职责的类型 | 持有/借用的上下文 | 方法或 trait 边界 |
|---|---|---|
| `ProviderFactory`、`ConfiguredModel` | 不可变配置模型、基础 transport 和上游认证句柄；不保存 Prepared | `resolve`、`http_client`、`auth_context`、`build_model`、`prepare`；只共享不依赖会话历史的数据 |
| 现有 `SessionModelState` | 本会话的模型目录、选择状态和各模型运行句柄 | 通过现有 ModelFactory 构造本会话目录；resolve/active/replace_model 只取本地句柄，切换回来复用本会话原模型 |
| `ProviderModel` | 本会话的 ModelProfile、共享 ConfiguredModel、当前凭证对应的具体模型/Prepared 缓存 | `connect`、`stream_request`/Model trait 方法协调会话内缓存、请求准备、middleware 和流转换；不再新增一层“总上下文” |
| 现有 `ProviderRequest`、`ProviderMessage` | 转换结果及转换期间的历史/调用身份信息 | 保留并更新 `TryFrom<ModelRequest>` 与关联转换方法；确需共享的请求级关联索引属于请求转换过程，不用模块全局表 |
| `AssistantProjection<'a>`（新增，位于 `kernel/src/provider/content.rs`） | 只借用 `&'a rig_core::message::AssistantMessage` | `From<&AssistantMessage>` 建立视图，`blocks` 与 `matches` 方法共用最终显示投影；同时服务结算和扩展改写检测 |
| 现有 `AssistantAttempt` | 单次生成的身份、增量和终态；不缓存 hook 前的可执行工具列表 | `apply`、`snapshot`、`settle` 完成状态变化和最终校正；不把其字段逐个传给新建自由函数 |
| `ProviderMiddleware` | 冻结的扩展注册和单次请求的 ExtensionContext | `apply` 附着原生 middleware；注册只保留上游 API 所需的薄闭包，复杂上下文操作归实例方法 |
| `ModelUsage`、现有 ACP `UsageOf<'a>` | 分别拥有上游 Usage、借用待投影的 ModelUsage | 标准转换/serde/累计 trait，以及 UsageOf 的 ACP/metadata 投影；不增加独立 usage 工具函数集合 |
| 现有 `RetryState`、`ContextUsageEstimate`、`SessionStatistics` | 对应的尝试预算、历史估算或累计状态 | 在现有方法和 trait 内更新上游语义，不创建平行的 retry/context/stats 管理层 |

`AssistantProjection::blocks(&self) -> Result<Vec<ContentBlock>, ModelError>` 负责唯一的原生到显示块转换；`matches(&self, blocks: &[ContentBlock], stop_reason: StopReason) -> Result<bool, ModelError>` 复用该转换并比较终态。它仅在调用栈内借用消息，不保存到持久化数据或返回流中，也不另缓存一份消息。工具调用从已校正的 blocks 在 AssistantAttempt 内收集，避免并行维护第二套投影逻辑。

上下文的成员只包含该职责所需的数据，优先 `&T`/`&mut T`；返回 `'static` 流时移动必要的拥有值或克隆已有 Arc，不能为满足借用形式引入泄漏、全局变量、额外深拷贝或长期持锁。超过 3 字段仍遵守 typed-builder，Option 默认和 Arc::clone 规则不变。

无共享上下文、无自然类型归属的叶子算法可保留自由函数；转换优先 `From`/`TryFrom`，借用集合构造优先适用的标准 trait。按内聚职责组织现有模块，不因“用了 struct”就新增文件、包装层或全仓改造。该结构约束通过代码审查验收，不编写仅匹配源码形状的测试。

## 4. 接入层与配置模型

`ProviderFactory` 只缓存共享的配置定义。`ConfiguredModel` 持有供应商配置、模型标识、模型额外参数、共享基础 transport 和上游 Authenticator，不持有具体模型或 Prepared；不能按全局 provider/model 键共享历史缓存。

具体模型缓存的作用域固定为“Session 实例＋配置模型”。SessionModelState 持有本会话独立的模型目录，其 ProviderModel 各自拥有未擦除的 `Model<W, DynHttpClient>` 及 Prepared；同一 Session 的多轮请求和重试复用，其他 Session 或子代理会话不能取得同一缓存。

约定接口（新增接口以实施计划为准，均为本项目 API）：

- `ProviderFactory::new(config: config::ConfigHandle) -> Self`。
- `ProviderFactory::resolve(&self, provider_id: &str, model_id: &str) -> Result<Arc<ConfiguredModel>, ProviderBuildError>`。
- `ConfiguredModel::http_client(&self) -> rig_core::http_client::DynHttpClient`：轻量克隆池化 transport。
- `ConfiguredModel::auth_context(&self) -> Future<Result<Option<rig_core::providers::chatgpt::auth::AuthContext>, ProviderConnectError>>`：经上游解析/刷新 ChatGPT 认证；普通 API key 模型返回 None，凭证由其不可变配置提供。
- `ConfiguredModel::build_model(&self, auth: Option<&rig_core::providers::chatgpt::auth::AuthContext>) -> Result<ProviderCompletion, ProviderConnectError>`：用基础 transport 构建新的具体模型和独立 Prepared，供会话首次绑定或凭证变化时使用；不做内部全局缓存。
- `ProviderCompletion` 是必要的具体上游模型枚举，不是新 Llm trait；实现 Clone，`with_transport(self, http: DynHttpClient) -> DynModel<Completion>` 只替换该克隆的 transport 再擦除类型。
- `ProviderModel::connect(&self, http: DynHttpClient) -> Future<Result<DynModel<Completion>, ModelError>>`：本会话解析认证、检查/更新自己的具体模型缓存，再克隆并调用 with_transport。

生产 `ModelFactory::create_catalog` 的每次会话调用必须生成新的 ProviderModel 句柄并引用同一 ProviderFactory 配置定义。全局 catalog 只供配置/能力查询，不能把其中的 `Arc<dyn Model>` 直接安装为会话运行句柄。SessionModelState::new/resolve/active/replace_model、Session 恢复、fork 和子代理 Session 创建都使用本会话目录；单纯克隆全局 catalog 或上游 Model 不会获得独立 Prepared。

具体模型在本会话首次使用该配置模型或配置/有效凭证上下文变化时构建；凭证上下文包括 token、account 和 API base。稳定配置和凭证下，本会话后续请求及重试不得重新调用 completion 构造器或 `Model::new`。上游 Model 克隆只用于同一会话内复用 Prepared；新会话必须从配置/wire 构建新 Model，不能克隆其他会话的模型充当新绑定。缓存保持在具体模型层，不先只保存 DynModel。模型切换后再切回，复用该 Session 对应模型的缓存。

请求级 middleware 仅附着到本会话模型的 transport 克隆，不写回缓存。缓存不保存请求级 transport、middleware 或 ExtensionContext；上游历史匹配只校验本会话历史变化，不承担跨会话缓存分区。OAuth 凭证解析后检查本会话缓存，凭证变化时各会话在下次使用时替换自身缓存，不积累旧凭证版本；发布采用短临界区，不跨认证、网络请求或轮询持锁。Session 关闭/删除时释放它拥有的全部模型缓存；进行中的请求只在安全结束前持有必要句柄。重新打开持久化会话时新建缓存，fork 不复制父 Session 的 Prepared。不得以全局 SessionId→缓存表替代会话所有权。

缓存验收同时证明所有权与实际复用：A/B 两个 Session 共用基础 HTTP/认证，但运行模型和缓存句柄不同；同一 Session 的模型切换往返保持原句柄。使用上游公开 Wire/ReplayTarget 的计数探针，在每个 Session 有 10 个已完成工具调用、历史已经规范化的条件下按 A→A→B→A 准备请求，历史调用 ID 适配次数应为 10→1→10→1，而非共享 Prepared 时的 10→1→10→10。该探针只证明机制；还要通过真实 SessionModelState 路径验证句柄隔离和会话关闭后的释放，不能只证明共享 HTTP 池。

`ProviderConnectError` 只组合构建、认证和上游错误，不创造一套重复的 HTTP/供应商错误分类。

保留 `provider_id/model_id` 的项目选择键、`upstream_id`、自定义 base URL、配置与静态扩展贡献模型，以及额外参数的合并顺序：模型默认值 < 请求参数 < middleware 对最终字节的修改。合并仅作用于附加参数；不能额外复制上游的请求字段处理。

已配置的 OpenAI Chat、Responses、Anthropic、ChatGPT、DeepSeek、Moonshot、MiniMax、MiMo 与现有通用兼容端点均需继续可构造。不能因为模型未进入上游内置 Catalog 就拒绝用户显式配置的兼容模型。使用相应上游配置/dialect 构造 wire，并只把上游 Catalog 作为有来源的模型能力依据。

DeepSeek 的图片限制跟随上游；同步 `ModelProfile` 的输入能力，项目现有不支持媒体提示仍可使用。底层直接收到图片时遵从上游降级，不另写“尽量支持”的请求转换。

## 5. 流生命周期、错误、重试与工具

### 5.1 流

1. 配置和鉴权解析后构造上游模型；`stream()` 仅准备流，实际 HTTP 在轮询时开始。
2. 文本、reasoning 的实时增量从对应 delta 发给现有 UI 事件。`End` 携带完整块，不能作为 delta 再次追加；上游允许最终块修正此前 delta，因此增量累计不是最终消息的依据。
3. 工具调用只在完整块结束后转换；先保存上游原生 ID，再构造项目 ToolCallId，不能用调试字符串或内部局部 ID 替代供应商 ID。
4. 消费结束后必须调用 `finish()`。EOF 不是成功；截断、错误事件和缺失终态按上游错误失败。
5. 成功终态转换为 `ModelFinal`，同时携带 `Option<rig_core::message::AssistantMessage>`。有原生消息时，`AssistantAttempt::settle` 借用 `AssistantProjection` 按最终原生内容及其顺序重新投影 `blocks`，在自身方法中更新可执行工具调用，替换临时 delta 累计，再构造已结算消息、执行 MessageEnd hook、做原生一致性检查及持久化。最终 MessageEnd 快照更新 UI，不补发修正文本作为追加 delta。现有非-provider 测试 Model 返回 None 时才沿用其累计 blocks。
6. 取消后停止轮询并释放流。未成功结算的内容保留为项目失败/取消消息，下次请求按照上游 Error/Aborted 规则处理，不伪造成功的 Native 或签名。缺失 usage 保持未知。
7. refusal、未知结束原因、输出截断、未完成工具调用的判定遵守上游 response/stop/turn_failure 契约；只有可执行的成功调用进入项目工具执行器。

最终 assistant 的 ACP 投影必须先发送完整 AgentMessage，再发送同一 messageId 的完整 AgentThought；无 reasoning 时也显式发送 `content: []`，不省略 thought 更新。前端继续遵守“省略保留、空值清空、内容替换”，不能让最终 AgentMessage 默认清空 reasoning，否则会破坏其他独立 upsert。实时完成、扩展删去 reasoning 和历史重放均使用这一规则，确保旧的 thought delta 不残留。

### 5.2 只有 Kernel 调度重试

provider 接入层不另包重试循环。所有新建推理尝试由 Kernel 现有调度统一处理；保留项目的取消、回退等待、Turn 结算与重试提示。

本次明确统一旧的两层预算，配置含义如下：

- `retry.agent.enabled` 是总开关。
- 对模型请求，一次逻辑生成的最大附加尝试数为 `retry.provider.max_retries.unwrap_or(retry.agent.max_retries)`；`Some(0)` 表示不重试。每个实际尝试只消耗一次预算，不允许外层再次重试已耗尽的内层预算。
- 自有退避使用现有 `retry.agent.base_delay_ms` 和上限；供应商显式等待取上游 `ErrorReport::retry_after(now)`。超过 `retry.provider.max_retry_delay_ms`（非零）时结束请求，不把等待缩短后提前重发。
- `retry.provider.timeout_ms = None` 使用上游流 idle timeout 默认值；`0` 关闭；正值传给 `with_idle_timeout`，同时约束首帧和后续帧间静默。这是相对旧“仅获取流阶段超时”的明确语义变化。
- OAuth 的设备码等待遵从上游认证超时且可取消，不把用户登录等待计入流 idle timeout。
- 供应商失败按上游结构化分类，删除当前基于供应商错误字符串的重复重试/overflow 列表。`ModelFailure` 增加 `retry_after_ms: Option<u64>` 和 `context_overflow: bool`；`AssistantMetadata` 持久化 `context_overflow`（旧记录默认 false），供恢复后的压缩判断使用；错误正文、认证头不得进入普通日志。
- 普通生成、自动/手动压缩、分支摘要均使用同一分类和预算规则。失败的部分输出可以成为诊断历史，但不能执行其工具或在重试时复用它的成功状态。

### 5.3 工具结果

沿用项目工具注册、审批、执行和结果预算。上游 `ToolName`、`CallId` 和 `ToolResult.is_error` 必须保留；现有结果没有 name 时从同一次模型历史的工具调用关联，不从当前工具清单猜测。

上游 `ToolFunction.invalid_arguments` 必须传递到项目 `ToolCall` 和持久化 `ContentBlock::ToolCall` 的可选诊断字段。出现该字段时生成工具失败结果并允许模型修正，不调用真实工具、不把 `{}` 当作有效参数；恢复未完成运行后同样不得执行空参数。缺失/孤立工具结果和跨模型调用 ID 的回放使用上游历史适配规则。

## 6. 原生消息、扩展改写与持久化

`MessageContent::Assistant` 新增可选 `model_message: rig_core::message::AssistantMessage`，默认 None。它保留原生内容顺序、Origin、StopReason、Native、签名、加密 reasoning、原生工具身份；`blocks` 继续是项目显示/工具执行投影。

- 成功响应优先保存上游 `CompletionResponse::message()` 产生的 assistant 消息，而不是从 delta 重新拼一个带猜测签名的消息。先以该最终消息校正显示 blocks，再交给扩展；原生投影的分块/合并规则在结算和一致性检查中必须相同。
- 构造下一次模型请求时，未修改的 assistant 优先用 `model_message`，让上游决定同模型或跨模型回放。
- Context、MessageEnd 等扩展若修改 `blocks` 或终态，原生消息不能压过扩展修改。以已完成最终校正的消息为基线，调用 `AssistantProjection::matches` 比较原生内容的显示投影及终态；不一致则丢弃该条消息的原生缓存，从改写后的项目内容按 canonical 方式构造。不能把供应商对 delta 的最终修正当成扩展改写。此处整条失效即可，不实现块级缓存协调器。

具体验收：text delta 为 `Bye`、最终 text 为 `Hello`，reasoning delta 为 `Plan A`、最终 reasoning 为 `Plan B.` 时，MessageEnd hook 看到的、最终 UI 显示的、持久化和下一轮回放的内容均为最终值；没有扩展改写时保留 Native。若 MessageEnd hook 再将 `Hello` 改为 `Hi`，则显示/保存/下一轮发送 `Hi`，并按既定规则使原生缓存失效。若最终投影不含 reasoning 或 hook 删除全部 reasoning 块，ACP 必须发送空 AgentThought，实际 decoder/reducer 的 reasoning 变成空字符串，UI 和重开历史不能保留 Plan A/Plan B.。
- fork、重开、恢复、保留尾部历史必须保留原生消息；压缩摘要只写项目摘要，不能把 Native/raw JSON 或认证信息拼入摘要 prompt。
- 旧消息缺少 `model_message` 时按 canonical 读取；使用旧 metadata 标记失败/取消。不能把历史 reasoning 文本冒充可信原生 reasoning。
- 协议 crate 可以依赖关闭默认 feature 的 rig-core，以直接持有原生类型；不镜像维护 Native/Origin 结构。
- ACP/WebUI 使用显示投影，不将原生消息、完整响应 raw 或加密块通过通用 `_meta` 无意暴露；调试日志同样不输出这些正文。

Store 的文件版本、记录排序、父子关系和追加原子性保持原协议。新字段缺省兼容旧文件；不批量重写已有 JSONL。老版本应用不保证能读取新 payload，应在 README 明确升级边界。

## 7. usage 的唯一语义与旧数据

### 7.1 内部语义

`protocol::ModelUsage` 改为拥有一个上游 `rig_core::completion::Usage` 的小包装，提供 `From<Usage>`、`AsRef<Usage>` 和 `AddAssign<&ModelUsage>`。累加直接使用上游实现，不重新定义可选计数相加规则。

输入已包含 `cached_input_tokens` 和 `cache_creation_input_tokens`；输出已包含 `reasoning_tokens`。供应商未提供的值保留 None，不能在持久化前 `unwrap_or(0)`。上游的 cost、tool_use_prompt_tokens 等字段随 Usage 保留，本次不增加定价、账单或新统计 UI。

示例：输入 1000、缓存读 600、缓存写 100、输出 200，总计为 1200。只有计数充分时才可推导未缓存输入 300。Anthropic 返回 uncached=7/read=10/write=20/output=3 时，上游输入=37、总计=40。

累计语义也跟随上游：两个未知计数相加仍未知，一个已报告计数与未知相加保留已报告部分；累计结果不能宣称是所有请求的完整账单。

### 7.2 序列化边界

为识别口径，ModelUsage 的新持久化形状固定为 `{"format":"rig-core-v1","usage":{...上游 Usage...}}`。内层使用上游序列化，不修改其字段含义。未知 format 必须报可定位的版本错误，不能当旧格式猜测。

ModelUsage 的反序列化集中兼容原有扁平形状（十进制字符串计数、`cache_read_tokens`、`cache_write_tokens`），所有消息、Usage record、compaction 和 replay 共享这个入口，不在各读取点重复迁移。

旧字段曾以 0 兼作“未报告”，且供应商输入口径不一致，因此采用保守转换：

1. 正数 total/output/cache 作为已报告值；旧 0 无法辨认来源，转为 None。旧 `reasoning_tokens` 已是 Option，保留 Some(0) 与 None。
2. 只有 total>0、output>0、total>=output 时，推导 inclusive input=total-output；不根据 provider 显示名称猜旧 input 是否含缓存。
3. total<output 时舍弃不一致的 total 与 input；已知缓存读写之和大于推导 input 时也将 input/total 置未知，其他可信分项保留。
4. 无法推导的 input 为 None，不重新解释旧 `input_tokens`。这些规则只改变内存视图，不覆盖源文件。

ACP 的项目 `_meta`/WebUI usage 继续以十进制字符串输出所有已知整数，未知项省略；保持目前的 cache_read/cache_write 展示键。必须通过显示 DTO 投影，不能把原生 Usage 直接交给 JavaScript 累计。`2^53+1` 经此投影往返必须精确。标准 ACP `Usage` 仍遵从其 schema：只有 total/input/output 必填值齐全时才发该对象，否则省略，不能补零；浏览器以项目精度安全的 metadata 为准。标准 `UsageUpdate` 的 used 必填值未知时，改用现有项目扩展更新传递未知计数与 context_window，不发送虚假的 used=0，也不能不发事件而让 UI 沿用旧数值。

### 7.3 消费方

- Context/overflow 判断使用 inclusive input 或上游 `Usage::context_tokens()`；不再加缓存读写。ACP `UsageUpdate.used` 和 WebUI Context 严格跟随 `context_tokens()`：优先 total；total 缺失且 input 已知时，用 input 加 output（output 缺失按 0），按 u64 上限饱和；input 也未知则仍未知。浏览器使用 BigInt，推导值只用于 Context，不写回已报告 total 或标准 ACP Usage。
- 没有足够计数时，沿用项目字符估算，而不是把未知作为 0 认定上下文很小。
- Run、Session、compaction 统计使用同一累计实现；WebUI 所有 usage 入口（包括 MessageView、ContextLens）共享格式化，未知显示“未知”，Some(0) 显示“0”。英文 CLI 使用 `unknown`，保持原有输出末尾格式。
- 已报告的零仍是有效压缩锚点，不能因为值为 0 就退回字符估算；本轮不改变这项既定语义。
- replay、重连和新增实时事件不能重复累计已持久化 usage；现有去重身份保持。

ACP 实时投影、命令快照和历史重放各构造一次借用型 `AcpEventMapper`，复用其 metadata 和 updates，不重复构造/序列化同一事件的 metadata；发布前释放不再需要的原生事件。

会话树的 `SessionTreeOf` 消费 Kernel 返回的独立快照，先按条目类型原位移除 assistant 的 `model_message` 和 compaction 的 `retainedTail`，再序列化显示结果；不能先完整复制隐藏历史再删除。Tree、Navigate、Branch 共用该入口。只转换明确的 usage 槽位，用户自定义 JSON 中的同名字段保持原样，持久化历史保持不变。分配验收只统计快照已构造后的投影阶段，隐藏内容增大不能导致投影分配量等比例增长。

### 7.4 失败诊断与脱敏

认证/模型构造失败的摘要和 WARN 保留 provider/model、失败阶段与安全类别；认证错误区分缺少认证上下文、I/O、JSON、HTTP 状态、token store 和认证流程失败。请求准备、流读取、最终校验失败保留上游 `report.kind.code()` 和可用 HTTP 状态。普通日志不得打印完整 report、源错误链或供应商正文。

终态是否失败继续由上游 `turn_failure()` 决定，但返回字符串可能含供应商原文，不能直接写入摘要或日志。摘要保留过滤/拒答、输出限制、异常终止、供应商报告错误、未知 finish reason 等安全分类；失败响应的 usage 继续保留，有工具调用时明确没有执行。`raw_stop_reason` 对未来枚举变体保留 Debug 表示，仅用于既有诊断字段，不加入普通日志。

## 8. 原生 HttpMiddleware 与扩展接口

废弃 `ModelRequestHooks` 和三个 provider hook point。新增 `ExtensionRegistrar::register_http_middleware(factory)`，factory 接收请求所属的 `ExtensionContext`，返回任意实现上游 HttpMiddleware 的对象。registry 保存 owner 和工厂，沿用模块注册的事务性与稳定顺序。

`ExtensionRuntime::provider_middleware(context) -> ProviderMiddleware` 返回请求级句柄；`ProviderMiddleware::apply(base: DynHttpClient) -> DynHttpClient` 按注册顺序将工厂产生的原生 middleware 附着到轻量克隆的 transport。工厂的泛型在注册处闭包擦除，不再复制一个 HttpMiddleware trait。

Kernel 的 Model 增加 `stream_with_middleware(request, cancellation, Option<extension::ProviderMiddleware>)`，替换旧 `stream_with_hooks`。基础 `stream` 仍供无扩展调用使用。

必须完整遵循上游：

- 完整 HeaderMap，包括多值和非 UTF-8 HeaderValue；不强制转成 BTreeMap<String,String>。
- 原始 Bytes 请求体；修改不强制为 JSON。
- Exchange 的 method、URI、extensions 可读；不承诺上游未提供的修改 method/URI 能力。
- 所有 header hooks 先按顺序运行，再运行所有 body hooks；后者看到最终 headers 和前一个 body 修改结果。
- 响应状态和完整 headers 可观察，响应 body 不可由此 hook 改写；multipart 跳过 body hook。
- 每次实际模型 HTTP 请求执行，包括重试；不缓存 payload/header replacement，也不以 response_seen 抑制后续尝试。
- 请求侧 hook 失败阻止发送；成功 HTTP 响应的 hook 失败使请求失败；非成功 HTTP 响应保留上游原错误优先级。不能继续使用旧 hook 的“诊断后忽略错误”行为。
- 同一共享连接池的不同 Session/Run/Turn 不共享请求级可变状态；丢弃请求即释放其 middleware。

应用扩展 middleware 作用于模型调用的 HTTP 交换。OAuth 认证/刷新使用上游 Authenticator 的基础 transport，不触发模型请求扩展；没有添加新的供应商请求头限制。

middleware 可以读取和替换认证头，这是用户明确选择的受信任扩展能力；宿主日志和 ACP 参数日志仍遵守 AGENTS.md 脱敏规则，不新增完整请求/响应日志。

## 9. ChatGPT 认证

配置改为 `auth = { type = "chatgpt", token_store_dir = "/明确的凭证目录" }`；`token_store_dir` 必填且为绝对路径，空值、相对路径或现有普通文件报配置错误，避免隐式依赖进程 cwd。旧 `type = "codex"`/`auth_file` 给出明确迁移错误，不静默把文件当目录。

直接使用 `FileTokenStore` 的 `<目录>/chatgpt.json`、权限、原子写和锁；共享上游 Authenticator 的刷新状态。保留显式 access token 配置能力。首次实际 OAuth 请求缺少有效凭证时允许上游设备码登录；取消必须能中断等待。

使用上游 DeviceCodeHandler 的回调把登录 URL 和临时用户码输出到 stderr，避免默认 stdout 输出污染 ACP stdio；不通过 tracing 记录 access/refresh token，也不新建 WebUI 登录管理界面。`preflight` 只检查构造条件，不发推理请求、不启动登录。

不用自定义 TokenStore 适配 Codex 文件；不读取、覆盖、搬移或删除 `CODEX_HOME/auth.json` 或 `~/.codex/auth.json`。已有登录需要在新的存储目录完成一次上游登录。

## 10. 验收矩阵

所有项在实施时记录结果和证据；本文件列出的是要求，不表示已经通过。

| 编号 | 场景 | 必须证明的结果 |
|---|---|---|
| A01 | 依赖及删除、模型复用 | crates.io 大版本范围，Cargo.lock 记录已验版本；生产仅有一份 rig-core；旧 vendor/Llm/OneOrMany/ModelRequestHooks 无生产引用；没有本地 path 或上游 patch；具体模型/Prepared 按 Session＋模型归属，同会话稳定凭证下复用，凭证变化替换，会话关闭释放 |
| A02 | 配置与路由 | 当前各供应商/协议组合可构造；upstream_id/base_url/附加参数顺序正确；未知模型仍支持显式配置；DeepSeek 不宣称图片能力 |
| A03 | 流完成 | text/reasoning 不重复、完整工具调用顺序正确；finish 取回 usage；最终内容覆盖不一致 delta；最终 reasoning 为空时空 AgentThought 清掉旧 delta，hook/UI/持久化/下一轮一致；EOF/截断/HTTP 429 不伪装成功 |
| A04 | 重试与取消 | 一套预算精确限制实际请求数；Retry-After、首帧/帧间超时、退避取消生效；失败/取消工具不执行 |
| A05 | 原生消息 | OpenAI Responses 加密 reasoning、Anthropic 签名、DeepSeek reasoning 与工具身份跨两轮及重开保持；跨模型按上游降级 |
| A06 | 扩展改写历史 | 供应商最终修正不触发 Native 失效；扩展在校正后修改文本/工具参数/终态时，下一请求发送新内容；删除 reasoning 在 ACP/UI 与重开历史生效，旧 Native 或 thought delta 不能覆盖修改 |
| A07 | 工具边界 | 无效 JSON 不执行空参数；结果携带调用名、正确 ID 和 is_error；空结果、图片/MCP 文本及预算投影不回退 |
| A08 | middleware 完整性 | 多值/非 UTF-8 header、认证头替换、非 JSON body、Exchange、顺序、每次重试、multipart、错误优先级均与上游一致 |
| A09 | 并发隔离与缓存作用域 | 两个会话并发共用 transport 时，headers/body/context 不串用；A/B 交替请求各自保留 Prepared；模型切换往返复用本会话缓存，关闭/重开/fork 生命周期正确；失败注册不泄漏 middleware |
| A10 | usage | 37/10/20/3/40 示例成立；None≠Some(0)；累计跟随上游；压缩不重复加缓存；2^53+1 通过 ACP/浏览器精确往返 |
| A11 | 历史与恢复 | 旧无 Native 消息、旧 usage、混合新旧 JSONL 可读取、fork、重开；源文件不被批量改写；未知格式报错；重连不重复累计 |
| A12 | OAuth | 新目录登录/读取/刷新与同目录并发锁由上游执行；取消可退出；旧 Codex 文件未访问；stdio 无设备码 stdout 污染 |
| A13 | 日志 | tracing 上下文保留；生命周期/错误可定位；密钥、认证头、cookie、原生消息及完整 prompt/response 不进入普通日志 |
| A14 | 工程检查 | workspace 测试、相关 hooks 对应的 fmt/Clippy、WebUI check/build 和现有前端检查全部通过 |
| A15 | 真实生产验收 | production app + production WebUI + 当前真实 provider，完成两轮工具调用、reasoning、取消、压缩、重开、usage、队列基本回归 |
| A16 | 上下文与方法组织 | 按 3.4 的职责表审查每个新增/修改入口：共享上下文由所属 struct 引用/持有，相关函数归其方法；配置/HTTP/认证全局共享，Prepared 由 SessionModelState 的本地 ProviderModel 持有；无重复传参链、空壳工具 struct、无意义 helper 或全局会话缓存；短期借用与返回流所有权正确，最终校正要求保持 |

A03–A13 可用 crate 级集成测试、合成 wire payload 和本地 HTTP 测试服务验证确定性协议行为；这些不能冒充 A15。真实 OAuth、其他供应商或真实缓存写计数若无可用凭证/场景，单列未验项，不标记为 live 已通过。

## 11. 实施约束与交付

- 所有新增函数有英文函数级注释，非平凡修改解释变动原因；遵守 typed-builder、Arc::clone、依赖集中与日志规则。代码组织必须满足 AGENTS.md 第 15 条及本规格 3.4，并完成 A16 审查。
- 测试只放 crate 级 `tests/`，或精确声明 `#[cfg(test)] mod tests` 的模块；不得为测试在生产源引入独立 cfg(test) 支持代码。
- Rust 行为修改按测试先行。前端不要求 TDD/单元测试，使用现有 check/build 与真实 WebUI 验收。
- 不创建 SubAgent、worktree 或 commit。实施授权已收到；本轮不创建 commit。
- 文档日期按用户时区为 2026-10-30。最终实现交付应报告实际 diff、测试命令与数量、未验项和配置迁移说明。
- 本轮仅修订本 spec、配套 plan 和本项目 AGENTS.md 的组织规则，不修改实现或 docparse。此前 `/tmp/rig-provider-audit` 的离线探针是调研证据，不是未来完成验收的替代品。


## 12. 实施时核实的边界

- `rig-reqwest` 与 `rig-core` 使用相同的 crates.io 版本范围并一起更新，使用公开 `shared()` 共享连接池，不自建 transport wrapper。
- 显式模型不依赖内置 Catalog；请求没有指定输出限制时使用配置的 `max_output_tokens`，避免未知 Anthropic 模型无法编码。模型附加参数保留递归合并，上游负责最终协议字段编码。
- 原生 assistant payload 使用 `Box` 持有，不改变 JSONL 格式；最终展示投影合并相邻同类块，并用借用前缀比较忽略无意义分块差异。晚于最终 usage 的取消保留已报告计数。
- 浏览器安全边界覆盖 ACP notifications、tree/navigate/branch 和直接 compact 回复。会话树移除 assistant Native 与仅用于恢复模型上下文的 `retainedTail`；只在已知类型的 usage 位置做精度投影，保持用户自定义 JSON 不变。
- 工具终态完全遵守上游 `turn_failure`：结构化失败/取消不执行调用；不完整参数返回工具错误。上游可能把普通 refusal 文本作为回答，并允许 token limit 处已经完整的调用；不重新引入供应商字符串判断。
- live 验收采用当前真实 DeepSeek；OpenAI/Anthropic 原生回放使用合成协议测试，ChatGPT 登录/刷新与真实 MessageEnd 删除 reasoning 的组合场景不标记为 live 已验证。


## 13. 复审修复后的边界

- **MessageEnd 权威性：** `AssistantAttempt` 只负责形成完整消息；Kernel 在 hook 完成后从最终 blocks 一次生成执行列表，并读取最终终态。移除调用、改写参数或改为结束/错误/拒绝/取消均控制实际执行。已经失败或取消的供应商尝试不能被 hook 恢复成可执行调用。删除重复工具列表，避免旧参数与新历史同时存在。
- **MCP Sampling：** 没有原生最终消息时保留增量累计；存在最终 `model_message` 时使用同一 `AssistantProjection::blocks` 校正，再过滤 MCP 不支持的 reasoning。增量和最终路径均保留 `invalid_arguments`，交给既有 MCP 编码边界拒绝错误 JSON。
- **失败计数：** `ModelFailure` 增加 `usage: Option<Box<ModelUsage>>`，承接上游已经解码的失败终态 usage。`AssistantAttempt` 将这些计数写入失败 assistant；Run 累计和持久化沿用同一计数。没有上游已报告计数时保持未知，不解析错误正文来猜测 usage。
- **比较开销：** `AssistantProjection::visible` 统一可见内容范围，`matches` 借用比较工具参数、图片和文本。文本按连续字节切片比较，允许相邻块重新分割，不拼接或构造临时完整投影；`blocks` 只在确实需要拥有最终展示内容时分配。
- **组织规则：** `AssistantAttempt` 与 `ContextUsageEstimate` 使用 typed-builder，Option 字段有默认值；此次没有新增全局上下文或平行消息转换器。
