# 使用上游 rig-core 替换 provider vendor 的实施计划

> 最新补验：已使用当前发布依赖和复审修复完成生产 app / WebUI / DeepSeek 基础 12 项及真实 MCP Sampling；见 [验收记录](../reports/2026-10-30-rig-core-live-e2e.md)。8.5 的 MessageEnd 删除 reasoning 扩展与自定义 middleware 组合 live 场景仍未覆盖，因此保持未整体勾选。

> **执行方式：** 用户已授权实施；使用 executing-plans 技能，由当前执行者在现有工作区完成。用户明确禁止 SubAgent、worktree 和未授权 commit；这些限制优先于技能中的默认提交和并行执行流程。所有步骤用 `- [ ]` 跟踪。实施结果与证据见 [验收报告](../reports/2026-10-30-rig-core-provider-migration-verification.md)。

**目标：** 删除 provider vendor，使用 crates.io 当前大版本范围的上游 rig-core 完成当前模型、工具、扩展、认证和会话链路，并通过协议回归与真实生产验收。

**架构：** 按 docparse 的上下文 struct 方式组织 Rust 行为：全局共享配置、HTTP 和认证，具体上游模型及 Prepared 由 SessionModelState 的本地 ProviderModel 按会话＋模型持有，请求时只克隆本会话模型并替换 transport。Kernel 在扩展和持久化前用最终响应校正增量累计；ACP 明确发送最终 thought 状态，包括清空。协议持有上游原生消息和 Usage，扩展原生接入 HttpMiddleware，WebUI 使用精度安全的显示投影。

**技术栈：** Rust 2024、rig-core / rig-reqwest `>=0.46.0-rc.1, <1`（crates.io）、Tokio、reqwest/rustls、项目现有 JSONL/ACP/React/TypeScript。

**规格：** [设计规格](../specs/2026-10-30-rig-core-provider-migration-design.md)。执行前完整阅读两份文档；本轮已收到明确实施授权。

## 全局约束

- 本轮实施 spec/plan 规定的代码、测试、依赖与文档变更，不创建 commit；docparse 仅作只读参考。
- 后续实施禁止 SubAgent 和 worktree，不包含任何自动 commit 步骤。
- 按用户追加决策，改用 crates.io 范围 `>=0.46.0-rc.1, <1`，不固定 Git revision；保留 Cargo.lock，通过定向 cargo update 获取后续 0.x 正式发布并验证。调研 revision 仅作为历史证据；不使用本地绝对 path、不修改上游或添加补丁。
- 根统一声明依赖来源；协议只依赖上游类型，provider 选择 `reqwest`、`rustls`；不默认开启额外生成/文档/WebSocket 功能。
- 所有新增函数写英文函数级注释，非平凡修改解释原因；遵守 typed-builder、Arc::clone、日志脱敏和依赖分组要求。
- 代码组织必须遵守 AGENTS.md 第 15 条与 spec 3.4：struct 引用/持有所需上下文，相关函数组织为方法；短期优先借用，跨返回流/任务的上下文按需拥有或 Arc 共享；复用已有类型和转换 trait，禁止上下文参数链、空壳工具 struct 和无意义 helper。
- Rust 非平凡逻辑先写行为测试并确认失败，再实现。测试只在 crate `tests/` 或 `#[cfg(test)] mod tests`；前端不要求 TDD/单元测试。
- 模拟 HTTP 与合成供应商数据只证明协议行为；WebUI 验收必须使用 production app 和真实配置 provider。
- 不读取、覆盖、搬移或删除真实 Codex 凭证；旧会话采用读取时转换，不批量重写源文件。
- 根目录执行本文 Cargo 命令；`npm --prefix web` 明确指定 WebUI。遵守当前 RTK.md，不额外套用旧文档的“一律加 rtk”规则。

## 审查重点

1. 同会话稳定配置/凭证下连续请求和重试复用 Prepared；A/B 交替不能清空对方缓存；middleware 每次重新执行且不串上下文：P2/P3/P4/P5 的构造、会话缓存、重试与并发测试。
2. 最终响应可能修正或清空 reasoning，先校正内容再执行扩展，最后显式发送完整 thought 快照；原生消息不能覆盖扩展修改：P4/P6/P7 的校正、清空、native 失效和重开测试。
3. 老 usage 中 0 与未知不可辨，输入口径混合且存在大于 JavaScript 安全整数的值：P1 的保守解码与 P7 的投影验收。
4. 收到工具调用后遇到 refusal、截断或取消不得执行工具，无效参数不得当作 `{}`：P4/P5 的执行计数断言。
5. 新 OAuth 凭证目录、并发刷新及设备码输出不能破坏用户文件或 ACP stdio：P2 的路径、输出边界与 P8 的真实认证验收。

上述行为测试之外，A16 单独审查上下文所有权、方法归属和抽象数量；不把源码 grep 或 struct 数量当作行为正确性测试。

## 上下文归属与接口约束

执行前按 spec 3.4 的 docparse 实例和职责表确定所有权。P1 的转换属于 ModelUsage/serde/标准 trait；P2 的共享配置、HTTP、认证和无缓存模型构造属于 ProviderFactory/ConfiguredModel；P3 的注册和附着属于 registrar/runtime/ProviderMiddleware；P4 的运行句柄归 SessionModelState，具体模型缓存与流归该会话 ProviderModel，结算归 AssistantAttempt；P5 复用 RetryState、ContextUsageEstimate 和 SessionStatistics；P6 复用协议读取与现有会话类型；P7 复用 UsageOf 借用投影并补齐最终 thought 清空。不得建立统一 ProviderContext/Utils 层或全局 SessionId→历史缓存表。

P4 增加一个具有实际共享职责的借用视图，供最终校正和扩展改写检测复用；放在现有 `crates/kernel/src/provider/content.rs`，只在 Kernel 内以最小可见性使用：

- `AssistantProjection<'a>` 只保存 `&'a rig_core::message::AssistantMessage`，通过 `From<&'a AssistantMessage>` 构造，不复制消息。
- `AssistantProjection::blocks(&self) -> Result<Vec<ContentBlock>, ModelError>`。
- `AssistantProjection::matches(&self, blocks: &[ContentBlock], stop_reason: StopReason) -> Result<bool, ModelError>`，复用 blocks 投影和终态映射。

视图在方法调用结束后释放，不进入返回的 `'static` 流或持久化数据。返回流拥有所需模型/transport/取消句柄；不要以 Box::leak、全局状态或深拷贝回避生命周期。现有 ProviderRequest/ProviderMessage 的类型转换继续放在关联方法和 TryFrom 中；一次请求共享的调用身份关联只维护一份，不散落为自由函数参数。

## 文件与任务归属

以下新增路径是计划目标，尚未创建。其他修改路径见 spec 第 3 节。

| 任务 | 主要实现文件 | 新增测试文件 |
|---|---|---|
| P1 数据契约 | `crates/protocol/src/usage.rs`（新增）、`message.rs`、`model.rs`、`tool.rs`、`lib.rs`，根/协议 manifests | `crates/protocol/tests/provider_data.rs` |
| P2 接入及认证 | `crates/provider/src/{lib,auth}.rs`（auth 新增）、`factory/mod.rs`，config llm/config/retry、manifests | `crates/provider/tests/upstream_factory.rs`、`upstream_auth.rs` |
| P3 middleware | `crates/extension/src/middleware.rs`（新增）、registrar/registry/runtime/point，protocol 旧 hooks/events | `crates/extension/tests/http_middleware.rs` |
| P4 会话模型、流与消息转换 | `crates/kernel/src/{provider,model,runtime}.rs`、`provider/content.rs`、`runtime/{session,model_state,attempt,run}.rs`、`runtime/tools/{mod,batch}.rs` | `crates/kernel/tests/provider_upstream.rs`，复用 `runtime/model_state.rs` 的 tests 模块 |
| P5 调度和统计 | `crates/kernel/src/runtime/{retry,run,compaction,settlement,model_state}.rs`、compaction/context、session_stats，config/retry | 复用 `retry.rs`、`compaction.rs`、`provider_upstream.rs` |
| P6 持久化 | protocol 反序列化、kernel transcript/replay/compaction，Store 仅按必要性修改 | `crates/kernel/tests/provider_history.rs`，复用 `crates/store/tests/jsonl.rs` |
| P7 ACP/WebUI | acp mapping/projection 的 usage 与完整 thought 快照，web domain/updateDecoder/sessionState/sessionStaging/AgentUsage | 复用 `crates/acp/tests/{mapping,v2_alignment}.rs` |
| P8 删除与验收 | 旧 vendor/示例、workspace 依赖、README/NOTICE、现有集成测试和真实 app | `crates/app/tests/web/provider_migration.e2e.mjs`（真实后端浏览器脚本） |

P1→P2→P3→P4→P5→P6→P7→P8 顺序执行。P1–P7 是同一次跨 crate 接口迁移，中间状态不发布；各任务先验证所属 crate 和具体行为，不为保持临时旧 API 编译而新增兼容抽象。P4/P5 共同形成 Kernel 的可编译验收点，涉及同一 crate 的 usage/流类型变化不能虚报分阶段已通过。P8 必须完成全仓编译与回归。

## P1：固定依赖并建立原生数据契约

**覆盖：** A01、A05、A07、A10、A11。

**文件：** 修改根 `Cargo.toml`/`Cargo.lock`、`crates/protocol/Cargo.toml`、`src/{message,model,tool,lib}.rs`；新增 `src/usage.rs` 与 `tests/provider_data.rs`。同步协议 crate 现有测试的构造点。

**输出接口：**

- `protocol::ModelUsage`：私有字段拥有一个 `rig_core::completion::Usage`；实现 `From<Usage>`、`AsRef<Usage>`、`AddAssign<&ModelUsage>`、`Default`、版本化 Serialize/Deserialize。
- `ModelFinal.model_message: Option<rig_core::message::AssistantMessage>`；`MessageContent::Assistant.model_message` 同类型，反序列化默认 None。
- `ModelFailure.retry_after_ms: Option<u64>`、`context_overflow: bool`；`AssistantMetadata.context_overflow: bool`，旧记录默认 false。
- `ToolCall.invalid_arguments: Option<String>` 和 `ContentBlock::ToolCall.invalid_arguments`，默认 None；四字段结构体改用 typed-builder，迁移构造点。无效诊断必须随历史保存，以免重开后执行 `{}`。

- [x] **1.1 只读确认基线与引用范围。** 执行 `git status --short`、`git -C ../rig rev-parse HEAD`，核对 spec SHA。用 `rg -n 'ModelUsage|ModelFinal|ModelFailure|ToolCall \{|MessageContent::Assistant' crates extensions` 收集调用点；不恢复或覆盖用户改动。
- [x] **1.2 声明可更新的上游依赖。** 根添加 `rig-core` / `rig-reqwest` 的 crates.io version 范围和 default-features，协议继承，更新 lock；不引入 root rig facade。`cargo metadata --format-version 1 --no-deps` 必须能解析工作区；网络获取失败保留明确原因，不替换来源。
- [x] **1.3 编写 `provider_data` 失败测试。** 覆盖新格式、旧格式、未知版本、None/Some(0)、native 消息与无效工具参数往返；使用合成 token/signature，禁止真实凭证。关键断言如下：

  ```rust
  assert_eq!(usage.as_ref().input_tokens, Some(37));
  assert_eq!(usage.as_ref().cached_input_tokens, Some(10));
  assert_eq!(usage.as_ref().cache_creation_input_tokens, Some(20));
  assert_eq!(usage.as_ref().total_tokens, Some(40));
  assert_eq!(unknown.as_ref().input_tokens, None);
  assert_eq!(reported_zero.as_ref().input_tokens, Some(0));
  ```

  旧数据输入=7、read=10、write=20、output=3、total=40，读取为 inclusive input=37；旧全零为未知；旧 total<output 或缓存大于推导输入按 spec 7.2 置未知。新格式 `format` 必须为 `rig-core-v1`。
- [x] **1.4 运行失败检查。** `cargo test -p protocol --test provider_data`。记录实际缺失 API/错误断言，不能把依赖下载或环境失败当作业务测试的 red。
- [x] **1.5 实现数据契约。** 将旧扁平 ModelUsage 转换集中在 `usage.rs`，解析/规范化归 ModelUsage 或其必要 serde 表示类型的关联方法与 TryFrom，读写和累计实现标准 trait；不增加 usage 工具函数集合。新格式内部直接序列化上游 Usage，累计委托上游。移除被上游非 Eq 类型打破的派生约束，只改确实依赖的类型。
- [x] **1.6 验证。** `cargo test -p protocol`。新增与原有协议测试全部通过；原生签名/加密内容只在测试断言和持久化值中存在，不出现在诊断输出。

## P2：组装上游配置模型和认证

**覆盖：** A01、A02、A12。

**文件：** provider manifest/lib/factory、新增 `auth.rs` 与两个 integration test；config `llm.rs`、`config.rs`、`lib.rs` 及 `tests/loading.rs`。`retry.rs` 的行为说明在 P5 修改。逐步移走旧 lib exports，最终目录删除归 P8。

**输入：** P1 的数据与根依赖。

**输出接口：**

- `ProviderFactory::new(ConfigHandle) -> Self`。
- `ProviderFactory::resolve(&self, &str, &str) -> Result<Arc<ConfiguredModel>, ProviderBuildError>`。
- `ConfiguredModel::http_client(&self) -> DynHttpClient`。
- `ConfiguredModel::auth_context(&self) -> impl Future<Output = Result<Option<rig_core::providers::chatgpt::auth::AuthContext>, ProviderConnectError>>`：使用共享上游认证，普通 API key 模型返回 None。
- `ConfiguredModel::build_model(&self, auth: Option<&rig_core::providers::chatgpt::auth::AuthContext>) -> Result<ProviderCompletion, ProviderConnectError>`：构造具有全新 Prepared 的具体模型，不缓存到 ConfiguredModel。
- `ProviderCompletion` 是可 Clone 的具体上游模型枚举；`with_transport(self, http: DynHttpClient) -> DynModel<Completion>` 替换所持克隆的 transport 后擦除类型，不在其中重建 Model。
- `ConfiguredModel::prepare(&self, request: CompletionRequest) -> CompletionRequest`，只合并模型附加参数与请求附加参数，不重写供应商编码。
- `ProviderAuthConfig::Chatgpt { token_store_dir: PathBuf }`，TOML tag 为 `chatgpt`；显式 access token 沿用现有配置渠道。

- [x] **2.1 写路由矩阵测试。** `upstream_factory` 覆盖现有七个供应商、适用的 Chat/Responses/Anthropic 路由、自定义兼容 base URL、静态扩展配置模型、未知内置 catalog 模型、缺少 key、重复/未配置模型。用本地 HTTP 捕获 `model=upstream_id` 和请求路径。
- [x] **2.2 写能力、合并和构造断言。** 模型默认 `{"a":{"x":1,"y":2}}` 与请求 `{"a":{"x":3}}` 得到 x=3/y=2；middleware 改写在 P3 验证。DeepSeek profile 不含图片输入，直接编码不要求旧 vision payload。新增 `model_builds_use_shared_transport_without_shared_history` 和 `request_transport_does_not_pollute_base_transport`：两次 build_model 使用同一基础连接池，但分别构造新模型；with_transport 只改克隆。长期模型/Prepared 的缓存身份和凭证变化断言归 P4 的会话所有者，不在 ConfiguredModel 中增加历史缓存。
- [x] **2.3 写认证边界测试。** 空/相对 token_store_dir、普通文件路径、旧 codex tag 明确报错；临时绝对目录由上游 FileTokenStore 读写 `chatgpt.json`。构造非过期合成凭证测试 auth_context；不访问个人目录、不联系真实 OAuth 服务。设备码输出使用回调可捕获，stdout 必须为空。
- [x] **2.4 运行失败检查。** `cargo test -p provider --test upstream_factory --test upstream_auth` 及 `cargo test -p config --test loading`。记录业务失败。
- [x] **2.5 实现共享定义和无缓存模型构造。** ProviderFactory 索引共享配置，ConfiguredModel 只持有不可变配置、基础 transport 和 Authenticator；准备/认证解析/build_model 是其方法，`auth.rs` 只组织所属类型的组装逻辑。build_model 用上游 config/dialect 新建具体模型并设置 idle timeout，返回必要的 ProviderCompletion 枚举；不添加 Llm trait，不在全局定义中保存 Prepared。with_transport 消费具体模型的克隆，不重新建立连接池或 Model。会话缓存复用、凭证变化替换由 P4 的 ProviderModel 实现；不手写刷新、历史适配或凭证持久化算法。
- [x] **2.6 配置认证交互。** 使用上游设备码回调写 stderr，实际请求允许登录，preflight 不登录；配置变更说明和完整 OAuth live 验收放 P8。提供明确配置错误而不接受旧 auth_file。
- [x] **2.7 验证。** 重跑 2.4，执行 `cargo test -p provider --lib` 和 `cargo check -p provider`，验证构造/transport 克隆行为；本地 HTTP 记录只保留脱敏结构。这里只证明共享依赖和独立模型构造，不能把它当作 P4 的会话缓存验收。认证锁复用注明“上游实现”，真实刷新仍待 P8。

## P3：用原生 HttpMiddleware 替换旧 provider hooks

**覆盖：** A08、A09、A13。

**文件：** extension manifest、`middleware.rs`、`registrar.rs`、`registry.rs`、`runtime/{mod,provider}.rs`、`point.rs`、`lib.rs`；protocol 旧 hooks/events/result/re-exports；`extensions/src/available/hook_examples/provider.rs` 及模块注册。新增 `crates/extension/tests/http_middleware.rs`，调整现有 extension 测试。

**输出接口：**

```rust
// 以下为计划中的本项目接口；内部调用上游 with_middleware。
ExtensionRegistrar::register_http_middleware<F, M>(
    &mut self, factory: F,
) -> Result<(), ExtensionError>
where
    F: Fn(ExtensionContext) -> M + Send + Sync + 'static,
    M: rig_core::http_client::middleware::HttpMiddleware + 'static;

ExtensionRuntime::provider_middleware(
    &self, context: ExtensionContext,
) -> ProviderMiddleware;

ProviderMiddleware::apply(&self, base: DynHttpClient) -> DynHttpClient;
```

`ProviderMiddleware` 可克隆，持有冻结注册和请求 context；注册阶段把附着动作擦除成内部闭包，保留原生 M 的泛型调用能力，不复制 trait 或添加逐供应商包装器。

- [x] **3.1 写顺序与字节能力测试。** 两个 middleware 得到 `headers-A, headers-B, body-A, body-B, response-A, response-B`；验证重复 header、不合法 UTF-8 的合法 HeaderValue、Authorization 替换、原始非 JSON Bytes、Exchange method/URI/extensions；所有测试用合成凭证。
- [x] **3.2 写隔离、事务与失败测试。** 两会话各 2 次请求共享 pool，tag 不串用；失败的 register_module 不留下 middleware；header/body 失败时底层发送次数为 0；200 响应 hook 失败可见，429 的 hook 失败仍保留原 429；multipart 不跑 body hook。
- [x] **3.3 运行失败检查。** `cargo test -p extension --test http_middleware`，确认失败来自新增契约。
- [x] **3.4 实现注册和附着。** 复用 ExtensionRegistrar 候选 registry 事务、注册顺序及 `context.for_extension`；ProviderMiddleware 持有本次请求所需上下文，在 apply 方法组织附着，每次请求实例化 factory 的结果；不缓存 header/body 结果。闭包只负责原生接口适配，复杂操作归所属 struct 的方法；不再创建携带同样上下文的平行包装层。失败按上游传播，不套用旧 `report 后继续` 策略。
- [x] **3.5 迁移示例并删除旧 API。** 更新 hook_examples，删除旧三个 provider point、事件、ModelRequestHooks 及无调用 HeaderPatch/Mutation；同时更新 `extension/tests/{runtime,registration,catalog}.rs`、`protocol/tests/extensions.rs` 的旧期望。其他扩展生命周期不变。
- [x] **3.6 验证。** `cargo test -p extension`、`cargo test -p protocol`、`cargo check -p extensions`。日志测试不打印合成 header/body 全文，以同样方式约束生产行为。

## P4：迁移会话模型、Kernel 流和消息转换

**覆盖：** A01、A03、A05、A06、A07、A09、A16。

**文件：** kernel manifest、`src/{provider,model,lib,runtime}.rs`、`provider/content.rs`、`runtime/{session,model_state,attempt,run}.rs`、`runtime/extension/provider.rs`、`runtime/tools/{mod,batch}.rs`；新增 `tests/provider_upstream.rs`，迁移 `tests/provider_runtime.rs` 的旧 Llm 测试，扩展 `runtime/model_state.rs` 的既有 tests 模块。

**输入：** P1 数据契约、P2 ConfiguredModel、P3 ProviderMiddleware。

**接口变化：** `Model::stream_with_middleware(&self, request: ModelRequest, cancellation: CancellationToken, middleware: Option<extension::ProviderMiddleware>) -> Result<ModelStream, ModelError>` 替代旧 hooks。每个会话的 ProviderModel 组合共享 ConfiguredModel、ModelProfile 和自身具体模型缓存；`ProviderModel::connect(&self, http: DynHttpClient) -> impl Future<Output = Result<DynModel<Completion>, ModelError>>` 先解析认证，再复用/替换本会话缓存，克隆后 with_transport。SessionModelState 的 new/resolve/active/replace_model 使用本会话 ModelCatalog，不能返回全局运行句柄；复用现有 ModelFactory::create_catalog 生成新的 ProviderModel，Factory 仍共享定义/HTTP/Authenticator。ProviderRequest/ProviderMessage 和 AssistantProjection 负责已有转换职责，不新增并行协调器。

- [x] **4.1 写会话缓存与流契约测试。** 增加 `session_models_keep_independent_prepared`、`stable_credentials_reuse_session_model`、`credential_change_replaces_only_local_cache`、`session_close_releases_model_cache`：A/B 运行模型及缓存句柄不同、同会话稳定复用，切换模型再切回保留本会话原句柄，关闭并结束活动请求后释放，fork/重开取得新缓存。用上游公开 Wire/ReplayTarget 的规范化历史计数探针验证 A→A→B→A 为 10→1→10→1，并用真实 SessionModelState 路径检查缓存句柄归属；不添加生产测试计数字段、不复制 Prepared。流测试覆盖延迟发送、429、delta/End 不重复、工具 ID、finish usage、截断和 unknown event。`final_response_corrects_streamed_projection` 验证 Bye/Plan A 被最终 Hello/Plan B. 替换；最终不含 reasoning 时 blocks 不得残留旧 reasoning，但必须保留其他最终内容，ACP 清空验收归 P7。
- [x] **4.2 写原生回放和改写测试。** OpenAI Responses reasoning encrypted item、Anthropic signed thinking、DeepSeek reasoning_content 各构造两轮 wire 请求；比较原生字段是否保留。`provider_restatement_keeps_native_until_extension_edits` 验证无扩展时最终修正保留 Native、hook 输入已为 Hello/Plan B.，hook 改为 Hi 后显示/保存/下次请求一致且旧 Native 失效；再覆盖 hook 删除全部 reasoning 块，最终消息和下一请求不恢复旧 reasoning。相邻同类块使用统一投影，不因分块差异误判改写。
- [x] **4.3 写工具执行防线测试。** `invalid_arguments` 为不完整 JSON 时，真实工具调用计数为 0，返回 `is_error=true` 的结果；refusal、取消、未完成/截断调用不得执行；正常结果恢复 name、wire ID、is_error。保留空 MCP 结果及 bounded-output 分隔符原有测试。
- [x] **4.4 运行失败检查。** `cargo test -p kernel --test provider_upstream`，记录尚未支持的新生命周期/回放断言。
- [x] **4.5 实现本地模型缓存、流转换及最终校正。** ProviderModel::connect 经共享定义解析认证，在自己的缓存短临界区内检查 token/account/API base；首次或变化时调用 build_model，其余请求克隆本地具体模型后 with_transport，不跨 await 持锁。ProviderRequest/ProviderMessage 的 trait/方法使用上游类型，不增加自由函数上下文链。成功 finish 后保存完整 assistant；AssistantAttempt::settle 用借用投影校正 blocks 和工具调用，再运行 MessageEnd hook、检查 native 一致性并持久化。最终 UI 通过 MessageEnd 的完整快照更新，reasoning 清空必须由 P7 的空 AgentThought 实现；不追加修正 delta。None 原生消息的非-provider 路径沿用累计方式。
- [x] **4.6 实现工具关联。** 在已转换历史中按调用 ID 建立名称关联；无效参数转工具失败结果而不执行。继续使用项目工具权限、结果预算和 ProviderContentProjection；不把上游 Agent 工具运行时引入 Kernel。
- [x] **4.7 接通会话生命周期及所有调用方。** Kernel 保留可创建会话目录的 ModelFactory，静态 provider 注册在共享定义层冻结；每次 Session 创建/恢复/fork/子代理创建通过 create_catalog 取得新 ProviderModel，并以选中的配置键从本会话目录 resolve。全局 catalog 只用于查询；替换当前 SessionModelState::new 中直接 Arc::clone(self.models)/复用 settings.model 的路径，模型切换也只能 resolve 本地句柄。普通生成、压缩和分支摘要使用该 Session 的模型；关闭/删除释放本地目录，活动请求结束前只保留必要句柄。迁移测试 Model 和静态扩展贡献，并搜索旧 hooks/Llm/OneOrMany 引用。
- [x] **4.8 记录验证状态。** 执行 `cargo test -p kernel --test provider_upstream --test provider_runtime --test turn` 与 `cargo test -p kernel --lib runtime::model_state`；缓存内部身份断言只在合法 tests 模块读取真实缓存字段。若未迁移 usage 阻止编译，保留未完成，P5.7 一并验收，不加临时旧 API。要求同会话复用、跨会话独立及关闭释放均有证据；mock transport 不标记为 WebUI 验收。

## P5：统一重试、压缩和 usage 消费

**覆盖：** A04、A10、A13。

**文件：** config `retry.rs`/验证与测试；kernel `runtime/{retry,run,compaction,settlement,model_state}.rs`、`compaction/{context,prompt}.rs`、`command/session_stats.rs`；更新 `tests/{retry,compaction,provider_upstream}.rs` 和 `tests/protocol_alignment/usage.rs`。

**接口规则：** provider 内无 retry loop；Kernel 的现有 RetryState 消费上游映射后的 ModelFailure，按 spec 5.2 调度。ModelUsage.as_ref() 是读取计数入口；累计使用 `+= &ModelUsage`。压缩使用上游 context_tokens 和持久化 context_overflow。

- [x] **5.1 写精确次数测试。** `agent.enabled=true, agent.max_retries=5, provider.max_retries=Some(2)`：连续 429 最多发送 3 次；None 使用 agent 预算；Some(0) 只发送 1 次；enabled=false 只发送 1 次。每次实际尝试的 middleware 次数与 HTTP 次数一致，不能出现 18 次叠乘。
- [ ] **5.2 写时间和取消测试。** Retry-After=120000ms、上限=60000ms 时不重发；首帧/帧间各超过配置 idle timeout 会失败；等待退避、登录等待、读流时取消都能退出。Tokio 暂停时间或可控本地 HTTP 用于确定性测试，不靠长 sleep。
- [x] **5.3 写计数与压缩测试。** input=1000/read=600/write=100/output=200 时 context=1200；不得为1900。全未知使用原有估算；Some(0) 与 None 不混淆。上下文溢出来自结构化标记，而非新一套字符串包含判断。
- [x] **5.4 运行失败检查。** `cargo test -p kernel --test retry --test compaction --test provider_upstream`，确认预算、时间或计数行为失败。
- [x] **5.5 实现唯一重试所有者。** 删除旧 ProviderRequestOptions acquisition retry loop；保留 RetryState 的预算上下文和 schedule 方法，把 ErrorReport 转换放到所属错误类型的标准 trait/关联方法；不拆出一组重复传递 policy/attempt/context 的自由函数。统一普通生成与摘要路径的预算语义，失败部分输出持久化一次，新尝试有独立状态。
- [x] **5.6 更新 usage 消费。** 移除 cache 再加、默认零和旧字段构造；Session/Run/Compaction 都委托上游累计。中间 idle/未报告 snapshot 使用未知 Usage。CLI 统计将未知写成“未知”，不声称已知分项是完整账单。
- [x] **5.7 验证并关闭 Kernel 集成检查点。** 重跑 5.4、4.8，并执行 `cargo test -p kernel --test protocol_alignment`；该目标名已用当前 `cargo metadata --no-deps` 核实。所有命令通过后才能勾选 P4 的通过项；P8 做全量检查。

## P6：验证新旧会话和原生数据恢复

**覆盖：** A05、A06、A10、A11。

**文件：** kernel `runtime/{transcript,session/replay,compaction}.rs`，必要的协议 Deserialize；新增 `tests/provider_history.rs`；复用 `crates/store/tests/{jsonl,mcp_content}.rs`。Store 实现只有确实阻止 payload 往返时才修改。

**输入：** P1 的新旧 Usage 读取、P4 的 native 消息结算。

- [x] **6.1 写恢复矩阵。** 临时 JSONL 包含旧纯文本消息、旧 usage、新 native、新 usage、compaction 和 Usage record。关闭/重开/fork 后原生签名与 CallId 保持，但模型/Prepared 是新会话自己的绑定，不复用旧运行缓存；旧消息仍可 canonical 发送。修正最终值 Hello/Plan B. 及 hook 清空 reasoning 的结果在历史与下一请求中保持，UI 清空由 P7/P8 验证。无效工具参数恢复后仍返回失败结果，执行次数为 0。
- [x] **6.2 写文件保护和去重断言。** 只读取旧文件前后字节完全相等；混合文件新增记录只追加；未知 usage format 产生包含记录定位的错误；replay + live 更新不重复计数。新保存后不承诺旧版程序可读取。
- [x] **6.3 运行失败检查。** `cargo test -p kernel --test provider_history`、`cargo test -p store --test jsonl`。需要调整的边界必须有失败证据；若透传已正确则保留现有实现。
- [x] **6.4 修正必要的读取和投影。** 所有 ModelUsage 走协议类型的唯一 Deserialize，历史读取/恢复留在现有 Session/Transcript/Kernel 方法；不额外创建迁移工具函数集合或常驻 MigrationContext。native 随会话 payload 保存，摘要渲染仅用显示文本；旧失败/取消 assistant 按上游规则重建，不能把旧 reasoning 当作签名内容。
- [x] **6.5 验证。** 重跑 6.3，加 `cargo test -p store --test mcp_content`、`cargo test -p kernel --test compaction`。记录“兼容读取”而不是“旧文件已迁移完成”。

## P7：同步 ACP/WebUI 的最终 thought 快照和可选 usage

**覆盖：** A03、A06、A10、A11、A13。

**文件：** acp `src/{mapping,projection}.rs`、`tests/{mapping,v2_alignment}.rs`；web `src/domain/model.ts`、`workspace/{updateDecoder,sessionState,sessionStaging}.ts`、`features/conversation/AgentUsage.tsx`；必要时同步受影响的本地协议绑定，不修改上游 ACP schema。

**接口规则：** 复用 `UsageOf<'a>(&'a ModelUsage)` 借用视图，将 `to_acp_usage(&self) -> Option<wire::Usage>` 和 `metadata(&self) -> serde_json::Value` 组织为其方法，不另建无状态 usage 工具类。项目 metadata 键沿用当前命名，值为可选十进制字符串；标准 ACP Usage 的必填值不齐时返回 None，UsageUpdate 的 used 未知时使用现有扩展更新。浏览器以精度安全的 metadata 为准，原生 model_message 不进入通用浏览器 payload。

最终 assistant MessageEnd 的现有 ACP mapper 先发 AgentMessage，再无条件发同一 messageId 的 AgentThought：有 reasoning 发完整内容，无 reasoning 发 `content: []`。删除当前“reasoning 非空才发 thought”的条件。保留 decoder 独立 upsert 的省略/清空/替换语义，不让普通 AgentMessage 隐式擦掉 thought。

- [x] **7.1 写 ACP 映射失败测试。** 新增 `final_assistant_emits_empty_thought_snapshot`：先有 thought delta，最终 assistant 仅有文本，映射必须包含同 ID 的 AgentMessage 和 `AgentThought { content: [] }`，并覆盖 hook 删除 reasoning 与历史 replay。usage 覆盖 None/Some(0)/9007199254740993、标准必填缺失与扩展更新；含合成 native/encrypted 的消息不得把原生字段带到浏览器。
- [x] **7.2 运行失败检查。** `cargo test -p acp --test mapping --test v2_alignment`。
- [x] **7.3 实现最终 thought 和 metadata 投影。** 在现有 ACP mapper 的 MessageEnd 分支始终发完整 AgentThought，包括空数组；实时与 replay 走同一路径，不新增记录“是否发过 thinking”的全局状态。usage 转换归 UsageOf 方法/trait；不直接序列化原生 Usage/model_message，不为字段建 helper struct。更新 usage、idle、replay、compaction 路径，未知不能补零。
- [x] **7.4 修改 WebUI。** 保持已有 AgentThought 的 omitted/clear/replace 契约；通过实际 decoder/reducer 输入 thought chunk→最终 AgentMessage→空 AgentThought，断言 reasoning 为 `""`，包含真实 staging 路径；普通省略 thought 的 upsert 仍保留旧值。usage 接收可选字符串，未知显示“未知”、零显示“0”，BigInt/字符串累计且重连去重。不新增只镜像实现的单元测试，使用现有构建及 P8 浏览器契约检查。
- [x] **7.5 验证。** 重跑 7.2，执行 `npm --prefix web run check`、`npm --prefix web run build`、`npm --prefix web run test:elicitation`。在 P8 的真实页面上检查已知值、未知值与重连效果；大整数精度还需经过真实 decoder/渲染组件验证，不靠截图猜测。

## P8：清理 vendor，完成工程和生产验收

**覆盖：** A01–A16 的最终交付门槛。

**文件：** provider 旧目录/示例、根与子 crate manifests/lock、README、THIRD_PARTY_NOTICES；新增真实 app 浏览器脚本 `crates/app/tests/web/provider_migration.e2e.mjs`，可复用已有浏览器依赖或环境工具；新增最终报告 `docs/superpowers/reports/2026-10-30-rig-core-provider-migration-verification.md`。不要安装新的前端测试框架。

- [x] **8.1 删除旧实现与无引用依赖。** 按 spec 3.2 删除 vendor；保留仍有派生代码所需许可证。确认生产不再引用 `provider::completion`、旧 Llm/OneOrMany、旧 provider hooks。重写仍有用途的示例，删除纯旧 API 展示。
- [x] **8.2 更新用户文档和配置样例。** 说明 DeepSeek 图片回退、原生 middleware 的可信权限、旧扩展 API 退役、usage 已含缓存/可未知、重试统一预算和 idle timeout、新 chatgpt/token_store_dir 及一次重新登录。不更改现有活动模型与真实凭据；claw.toml 只改相关非秘密声明。
- [x] **8.3 执行工程检查并检查退出码。** 以下全部成功；有 ignored/live-dependent 测试必须分类记录，不能把 ignored 算 passed：

  ```sh
  cargo fmt --all -- --check
  cargo check --workspace --all-targets --locked
  cargo test --workspace --all-targets --locked --no-fail-fast
  cargo test --workspace --doc --locked
  cargo clippy --workspace --all-targets --locked -- -D warnings
  npm --prefix web run check
  npm --prefix web run build
  npm --prefix web run test:elicitation
  git diff --check
  ```

  检查 `cargo tree -p app --edges normal` 中的 rig-core 来源、未使用的 transport 和重复版本。只有新失败、改动或未解疑点才扩大/重复测试。此处执行相关 fmt/Clippy 检查不意味着创建 commit；没有任何 git commit 步骤。
- [x] **8.4 构建生产程序。** `cargo build -p app --bin clawcode --locked`。在 `/tmp/clawcode-rig-core-e2e/` 建立当前配置副本，将 `session_persistence.data_home` 指向隔离数据目录，并隔离测试工作目录，保留真实 provider/model/base URL；只经原有密钥渠道传入凭证，不打印配置全文。使用 `CLAW_CONFIG=/tmp/clawcode-rig-core-e2e/claw.toml target/debug/clawcode serve --bind 127.0.0.1:3001 --web-root web/dist` 启动，确认 bootstrap 的活动模型与测试记录一致。配置文件含内联密钥时以 0600 保存副本，不纳入测试报告或版本控制。
- [ ] **8.5 在独立浏览器运行真实场景。** 脚本参数只传 app URL 和无秘密的隔离目录；浏览器使用独立 profile，不操作用户浏览器数据。每项记录请求身份、实际可见中间状态、结束状态及脱敏日志：

  | 场景 | 验收 |
  |---|---|
  | 普通生成及两轮工具循环 | 中间 text/reasoning 可见、不重复；工具调用/结果关联正确；第二轮真实请求成功 |
  | 最终 reasoning 清空 | 真实模型产生 reasoning 后，测试扩展在 MessageEnd 删除 reasoning；实际 ACP 含空 AgentThought，页面 thought 清空，重连/重开不恢复旧 delta；未产生 reasoning 时单列该 live 项未验 |
  | 当前 DeepSeek 图片输入 | UI/模型能力声明不宣称支持；显示项目既有不支持提示或上游降级，不能伪称视觉成功 |
  | 取消与再次请求 | 取消快速返回；不执行取消后的工具；随后请求成功且无遗留流 |
  | 自动或手动压缩 | 真实调用摘要；usage/缓存不重复计算；压缩后继续真实对话 |
  | 重开与 fork | 读取真实生成的历史并继续工具对话；原生消息持久化后仍可使用 |
  | usage 与重连 | 实时值与重开/重连一致；None/0 可区分；没有二次累计 |
  | 已有队列基本回归 | 忙时 follow-up/steer、消息入队和终态仍按原有协议工作 |
  | 原生 middleware | 注册测试扩展对真实模型请求添加无害诊断 header，显示其每次请求被调用；不改真实 Authorization、不记录 token |

- [x] **8.6 完成原生历史专项与 OAuth 验收分层。** 合成 OpenAI/Anthropic/DeepSeek wire 回放测试必须通过。具备相应真实凭证时，再执行 OpenAI Responses/Anthropic 两轮 reasoning 与 ChatGPT 新目录登录、使用和刷新；需要用户交互的 OAuth 由用户完成，不能伪造授权。缺凭据的 live 项单列未验，不能用当前 DeepSeek 成功代替。真实缓存写计数未返回时记录 None，不为了达标伪造计数。
- [x] **8.7 补充显示精度验证。** 使用生产构建中的真实 usage decoder/组件验证 `9007199254740993` 和 None/Some(0)。可以向组件级浏览器检查输入合成数据，但必须标明它是显示契约验证，不冒充生产 provider 的真实 usage。
- [x] **8.8 完成 A16 代码组织审查。** 对照职责表检查上下文、方法和生命周期；无重复传参链、空壳工具类、无意义 helper 或全局会话缓存。确认 ConfiguredModel 只共享定义/HTTP/认证，SessionModelState 本地 ProviderModel 持有并释放 Prepared，A/B 交替不互相清空；AssistantProjection 共用、UsageOf 借用，最终 thought 空快照完整。以代码和行为证据验收，不加源码形状测试或无关重构。
- [x] **8.9 编写验收报告并清理本轮进程。** 对 A01–A16 逐项标记通过/失败/未验，附实际命令、计数、供应商、脱敏证据、配置变化及 A16 的职责/生命周期审查结论；不宣称无凭据场景已通过。停止自行启动的测试服务/浏览器，保留可复查的测试产物，确认只存在授权范围的文件差异。

## 规格到任务的覆盖

| 规格章节/验收项 | 实施任务 |
|---|---|
| 版本、依赖、删除范围、会话级具体模型/Prepared 复用；A01 | P1、P2、P4、P8 |
| 配置路由与 DeepSeek 回退；A02 | P2、P8 |
| 流生命周期、最终校正与 thought 清空；A03 | P4、P6、P7、P8 |
| 重试、取消、超时；A04 | P4、P5 |
| 原生消息、改写、工具；A05–A07 | P1、P4、P6、P7 |
| 原生 middleware、并发与会话缓存隔离；A08–A09 | P2、P3、P4、P5、P6 |
| usage 与精度；A10 | P1、P5、P7、P8 |
| 新旧持久化与恢复；A11 | P1、P6、P7 |
| OAuth 与用户文件；A12 | P2、P8 |
| 日志；A13 | P2、P3、P5、P7、P8 |
| 工程检查、真实生产；A14–A15 | P8 |
| struct 上下文与方法组织；A16 | P1–P7 的类型职责落实，P8.8 审查 |

## 实施记录和未覆盖项

用户已授权实施。代码、测试、依赖、README 和第三方归属说明均已同步；未创建 SubAgent、worktree 或 commit。最终工程与真实运行证据见 [验收报告](../reports/2026-10-30-rig-core-provider-migration-verification.md)。

- 5.2：首帧/帧间超时、读流取消、退避取消和 Retry-After 均已测试；交互式 OAuth 登录等待取消未做 live 验证，保持未勾选。
- 8.5：production app + 当前 DeepSeek 的 12 个场景通过；未在该生产构建中启用可删除 reasoning 的测试扩展或原生 middleware 扩展。最终 thought 清空已分层经过 Kernel、ACP、真实浏览器 decoder/reducer/staging 验证，中间件已覆盖原生 HTTP 及真实 Kernel 尝试次数，不能冒充两项组合 live 证据。
- 2.3/2.6/8.6：上游 token-store 格式、旧配置拒绝和合成凭证读取已通过；设备码 stderr 路径经源码核对，真实 ChatGPT 登录/刷新以及 OpenAI/Anthropic 真实服务调用未验证。勾选表示已完成分层记录，不表示这些 live 项通过。
- 4.1：仓库内 `prepared_cache.rs` 验证 10→1→10→1；实际会话目录隔离、模型切换、关闭释放与凭证变化由 Kernel unit 验证。合成旧凭证只验证本地缓存替换，不代替真实 OAuth 刷新。
- 工程检查后的补充修复：未知 Anthropic 模型的默认输出上限、所有 assistant 角色判断、压缩 retainedTail 的浏览器隐藏、等价块分割、最终 usage 后取消和覆盖重试预算校验。均有对应回归或真实场景证据。


### 追加决策：持续接收发布更新

- 用户明确取消固定 revision；改为 `>=0.46.0-rc.1, <1`，最低版本不低于当前适配 API，后续 0.x 正式版本可以通过 `cargo update -p rig-core -p rig-reqwest` 获取。Cargo.lock 仍保留每次已验证的具体版本。
- 同步 README、第三方说明和 spec；历史验收报告保留原始 revision 的证据，另记 crates.io 发布版验证结果，不能把之前的测试结果直接套给不同来源的包。
- 发布版可能与同版本号的本地 Git main 不同；须重新编译并验证迁移相关回归。若上游 0.x minor 更新不兼容，继续在现有薄适配层调整，不引入 vendor 补丁。


### 复审问题修复

- [x] **R1** 将 MessageEnd 参数改写、删除调用、终态修改和失败不可复活加入真实 Kernel + 本地 HTTP 回归；删除 AssistantAttempt 的旧调用列表，在 hook 后一次确定执行内容。
- [x] **R2/R3** MCP Sampling 增加真实 Session Host 回归，覆盖无 delta 的最终内容、最终修正、reasoning 过滤和增量/最终无效 JSON 标记保留；复用 AssistantProjection。
- [x] **R4** 用同一 Anthropic refusal 帧分别验证上游 Usage 和项目保存结果；保留 input=10/output=2/total=12，并断言失败终态、Run 汇总与持久历史不丢计数。
- [x] **R5** 将原生一致性检查改为借用切片比较；增加 UTF-8 分块、隐藏 reasoning、工具参数、图片、空文本及终态差异回归。独立分配探针对相同 4MiB 文本从 4,194,944 字节额外分配降到 0，探针不进入生产源码。
- [x] **R6** 补齐 AssistantAttempt、ContextUsageEstimate 的 typed-builder，清理重复状态与一次不必要的工具列表克隆。
- [x] **R7** 工作区 all-targets 测试、Clippy、fmt 与 diff 检查；结果和未验证项见报告“复审修复验收”。本轮不重复声明原有 live 场景已验证当前修复。

### 2026-10-31 诊断与 usage 补充修复

- [x] **D1** 在 ProviderConnectError 的 Display、ProviderModel 和 ModelFailure 所属方法中保留安全错误分类、HTTP 状态与模型身份；补认证、HTTP、拒答的脱敏回归，原始供应商文字不得进入摘要或普通日志。
- [x] **D2** MessageView、ContextLens、AgentUsage 复用 ModelUsages 的格式化；Context 计算按上游 total/input/output 回退与 u64 饱和规则实现，精度与未知语义通过实际组件和 decoder 检查。
- [x] **D3** ACP 实时、命令和重放复用同一个借用 mapper；保留原有 map 便捷入口，避免无关调用方改造。UsageUpdated 的 used 与 Kernel 一致，精确 metadata 不补造 total。
- [x] **D4** 英文 CLI 未知值改为 unknown 并恢复无尾随换行；解释 LegacyUsage 的 serde-only 构造与旧 input 字段；未来 FinishReason 兜底保留 Debug 信息。
- [x] **D5** 第 4 项建议不采纳：保留已确认的 Some(0) 压缩语义及既有回归，不把零等同未报告。
- [x] **D6** 完成 workspace 回归（787 通过、2 忽略）、Clippy、fmt、WebUI check/build 和 23 项实际组件/decoder 契约检查。证据与边界见 `docs/superpowers/reports/2026-10-31-rig-review-fixes.md`；未创建 commit。
- [x] **D7** 会话树投影消费独立快照，先剥离隐藏历史再序列化；同步 Tree、Navigate、Branch 调用方。隐藏内容 0/8/32 MiB 的投影分配均为 3,153 字节，输出与原实现逐字节相同；ACP 165 项测试通过。测量方法及验证边界见 `docs/superpowers/reports/2026-10-31-session-tree-allocation.md`。
