# ACP Provider 登录实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal：** 切换到未登录的 OAuth 模型时，通过 ACP `auth/login` 在 WebUI 完成设备码登录，模型请求不再阻塞等待登录。

**Architecture：** Provider 是唯一判断方，给出 `SignInRequired` 和登录方式；Kernel 只转换错误、转发调用，在切换模型和消息入队前调用 `ensure_signed_in`；ACP 映射为 `authMethods`、`auth/login` 和 -32000，设备码通过绑定请求的 URL elicitation 下发；WebUI 收到 -32000 后发起登录。

**Tech Stack：** Rust（rig-core 0.46、agent-client-protocol 5、tokio）、React 19、zustand。

**Spec：** [ACP 登录设计规格](../specs/2026-11-05-acp-provider-login-design.md)

> 本计划是执行时的记录。实现后的 review 调整（去掉 `sign_in` 的取消参数、`SignInFailed`、`methodName`、`SignInDialog` 的位置等）以 spec 为准。

## Global Constraints

- 未经用户明确允许不执行 `git commit`，也不改动用户的暂存状态（不运行 `git add`、`git reset`、`git stash`）。本计划因此没有提交步骤。
- 不使用 worktree。前置改动（`auth_dir` 改名与 `~` 展开）已在工作区，直接在其上继续。
- 项目规则：注释用英文，每个新函数有函数级注释；超过 3 个字段的结构体用 `typed-builder`；克隆 `Arc` 写 `Arc::clone(&x)`；用 `tracing::info!` 全路径，不用字段语法；测试只放 crate 的 `tests/` 或 `#[cfg(test)] mod tests`；依赖经 root `[workspace.dependencies]` 声明并按语义分组。
- 只有 protocol、provider、extension 可以直接依赖 rig（`scripts/check-rig-boundary.py`），Kernel 和 ACP 不得 import rig。
- code 和 token 不进入 tracing 日志。
- 固定取值：`method_id` = provider ID；`name` = `display_name`；上游文案前缀 `ChatGPT sign-in required`；检查上限 10 秒；申请设备码上限 30 秒；需要登录响应 -32000，`data = {"methodId": id}`；取消响应 -32800；未知方法响应 -32602；只有 HTTP 401 打失效标记。
- WebUI 端到端测试必须使用生产 `app` 后端和配置的 provider，不引入 fixture 后端。

## Review Focus

1. **全新环境：** `auth_dir` 根本不存在时，应判定为需要登录，而不是报错。测试见 Task 1。
2. **`auth_dir` 无法使用：** 路径的上级是普通文件时，不应误导用户去登录，应在请求时报存储错误。测试见 Task 1。
3. **残缺的凭据记录：** 文件能解析但没有 token（`{"account_id":"a"}`），应判定为需要登录。测试见 Task 1。
4. **拿到设备码之前就失败或取消：** 申请设备码的请求失败时，`auth/login` 报错且不发 elicitation；此时用户取消也能立即结束。测试见 Task 2、Task 5。
5. **同时发起两次登录：** 两次 `auth/login` 各拿各的 code，elicitation 的 `requestId` 不串，互不影响。测试见 Task 5。

---

### Task 1: Provider 判定是否需要登录

**Files:**
- Create: `crates/protocol/src/login.rs`
- Modify: `crates/protocol/src/lib.rs`、`crates/provider/src/auth.rs`、`crates/provider/src/factory/mod.rs`、`crates/provider/Cargo.toml`
- Test: `crates/provider/tests/upstream_auth.rs`

**Interfaces:**
- Produces（`protocol`）：
  - `pub struct LoginMethod { pub id: String, pub name: String }`
  - `pub struct DeviceCode { pub verification_uri: String, pub user_code: String }`，手写 `Debug`，把 `user_code` 显示为 `<redacted>`
  - `pub type DeviceCodeSink = Arc<dyn Fn(DeviceCode) + Send + Sync>`
- Produces（`provider`）：
  - `ProviderConnectError::SignInRequired { method_id: String }`，`Display` 为 `sign-in required for provider '{method_id}'`
  - `ProviderFactory::with_http(config: ConfigHandle, http: DynHttpClient) -> Self`
  - `ConfiguredModel::login_method(&self) -> Option<&LoginMethod>`
  - `async fn ConfiguredModel::ensure_signed_in(&self) -> Result<(), ProviderConnectError>`，只会返回 `SignInRequired`
  - `ConfiguredModel::note_failure_status(&self, status: Option<u16>)`

**验收：** L01–L06。

- [ ] **Step 1：写失败测试**

在 `upstream_auth.rs` 增加辅助函数 `chatgpt_factory(dir: &Path, http: DynHttpClient) -> ProviderFactory`（provider `chatgpt`，模型 `a`、`b`，`base_url = "https://provider.invalid"`，模型写明上限）和 `seed(dir, json)`（用 `FileTokenStore` 写入）。辅助断言 `assert_sign_in(error)`：`matches!(error, ProviderConnectError::SignInRequired { method_id } if method_id == "chatgpt")`。

| 测试名 | 准备 | 断言 |
|---|---|---|
| `missing_credential_requires_sign_in_without_device_flow` | 空目录，`RecordingHttpClient::new("")` | `ensure_signed_in` 和 `connect` 都是 `SignInRequired`；`http.requests().is_empty()` |
| `live_credential_passes_without_network` | `{"access_token":"t","account_id":"a","expires_at":i64::MAX}` | `ensure_signed_in` 为 `Ok`；没有 HTTP 请求 |
| `refresh_invalid_grant_requires_sign_in` | `{"refresh_token":"r","expires_at":0}`，`RecordingHttpClient::with_error(BAD_REQUEST, r#"{"error":"invalid_grant"}"#)` | `SignInRequired` |
| `refresh_server_error_does_not_intercept` | 同上记录，`with_error(SERVICE_UNAVAILABLE, "down")` | `ensure_signed_in` 为 `Ok`；`connect` 是 `Authentication` |
| `refresh_transport_error_does_not_intercept` | 同上记录，测试内的 `FailingHttpClient`，`send` 返回 `http_client::Error::Instance` | `ensure_signed_in` 为 `Ok` |
| `refresh_error_mentioning_the_phrase_is_not_sign_in` | `with_error(BAD_REQUEST, r#"{"error":"server_error","error_description":"sign-in required upstream"}"#)` | `ensure_signed_in` 为 `Ok` |
| `slow_refresh_survives_the_check_limit`（`#[tokio::test(start_paused = true)]`） | 测试内的 `GatedHttpClient`，`send` 先等 `Notify`，再返回 `{"access_token":"rotated","refresh_token":"rotated-refresh"}` | `ensure_signed_in` 为 `Ok`；放行后 `chatgpt.json` 最终包含 `rotated-refresh` |
| `unauthorized_response_requires_sign_in` | 有效凭据，调用 `note_failure_status(Some(401))` | 模型 `a` 和 `b` 的 `ensure_signed_in`、`connect` 都是 `SignInRequired` |
| `forbidden_response_keeps_the_credential` | 有效凭据，`note_failure_status(Some(403))` | `ensure_signed_in` 为 `Ok` |
| `api_key_provider_has_no_sign_in` | `api_key` provider，`note_failure_status(Some(401))` | `login_method()` 为 `None`；`ensure_signed_in` 为 `Ok` |
| `login_method_names_the_provider` | 同 `chatgpt_factory` | `login_method()` 的 `id == "chatgpt"`，`name == "ChatGPT"` |
| `absent_auth_dir_requires_sign_in` | `auth_dir` 指向不存在的子目录 | `SignInRequired`；没有 `chatgpt.json` |
| `unusable_auth_dir_does_not_intercept` | `auth_dir` 的上级是普通文件 | `ensure_signed_in` 为 `Ok`；`connect` 是 `Authentication` |
| `partial_record_requires_sign_in` | `{"account_id":"a"}` | `SignInRequired` |

`GatedHttpClient` 和 `FailingHttpClient` 实现 `rig_core::http_client::HttpClientExt`，`send_multipart` 和 `send_streaming` 委托给内层 `SequencedHttpClient`。trait 要求 `send` 返回 `'static` 的 future，所以先在 async 块外把请求体转成 `Bytes`。

- [ ] **Step 2：确认测试失败**

Run: `cargo test -p provider --test upstream_auth`
Expected: 编译失败，提示 `with_http`、`ensure_signed_in` 等不存在。

- [ ] **Step 3：实现**

- `protocol`：新增 `login` 模块并导出三个类型。
- `crates/provider/Cargo.toml`：`[dependencies]` 新增 `# async` 分组，内容为 `tokio = { workspace = true, features = ["rt", "time"] }`；dev-dependencies 的 tokio 增加 `"test-util"`。
- `crates/provider/src/auth.rs`：用下面的结构体替换 `impl ConfiguredModel { fn authenticator }`。

```rust
pub(crate) struct ProviderAuth {
    authenticator: Authenticator, // AuthSource::OAuth, allow_device_flow = false
    method: LoginMethod,
    invalidated: AtomicBool,
}
```

  方法：`new(provider: &LlmProvider, config: &ProviderAuthConfig) -> Self`；`context(&self, http: &DynHttpClient) -> Result<AuthContext, ProviderConnectError>`；`check(self: Arc<Self>, http: DynHttpClient) -> Result<(), ProviderConnectError>`；`note_failure_status(&self, status: Option<u16>)`。

- `context`：`invalidated` 为真时直接返回 `SignInRequired`；否则调用 `auth_context`，并把 `AuthError::Message` 中以 `ChatGPT sign-in required` 开头的错误转成 `SignInRequired`，其他转成 `Authentication`。这是唯一匹配文案的地方。
- `check`：用 `tokio::spawn` 运行 `context`，再用 `tokio::time::timeout(SIGN_IN_CHECK_LIMIT, handle)` 等待（`const SIGN_IN_CHECK_LIMIT: Duration = Duration::from_secs(10)`）。超时不 abort 任务。只有 `SignInRequired` 向外返回；其他错误、超时和 `JoinError` 都记 `tracing::warn!` 后返回 `Ok(())`。
- `factory/mod.rs`：`auth` 字段改为 `Option<Arc<ProviderAuth>>`，一个 provider 共享一份；`auth_context` 改用 `ProviderAuth::context`；`ProviderConnectError` 的 `Display` 和 `source` 改成 `match`；`new` 委托给 `with_http(config, rig_reqwest::shared())`。

- [ ] **Step 4：确认测试通过**

Run: `cargo test -p provider && cargo test -p kernel --test provider_runtime provider_auth_failure_is_classified_and_logged`
Expected: 全部通过。

---

### Task 2: Provider 执行设备登录

**Files:**
- Modify: `crates/provider/src/auth.rs`、`crates/provider/src/factory/mod.rs`
- Test: `crates/provider/tests/upstream_auth.rs`

**Interfaces:**
- Consumes：Task 1 的 `ProviderAuth`、`DeviceCode`、`DeviceCodeSink`。
- Produces：`async fn ConfiguredModel::sign_in(&self, on_code: DeviceCodeSink) -> Result<(), ProviderConnectError>`。没有 `auth` 的 provider 返回 `Authentication(Box::new(AuthError::Message("provider has no sign-in".into())))`。

**验收：** L07。

- [ ] **Step 1：写失败测试**

设备流的模拟响应：

```rust
SequencedHttpClient::new([
    MockHttpResponse::success(r#"{"device_auth_id":"d1","user_code":"UC-1","interval":"1"}"#),
    MockHttpResponse::success(r#"{"authorization_code":"code","code_verifier":"verifier"}"#),
    MockHttpResponse::success(r#"{"access_token":"fresh","refresh_token":"fresh-refresh"}"#),
])
```

| 测试名 | 断言 |
|---|---|
| `device_sign_in_serves_every_model_of_the_provider` | 对模型 `a` 调用 `sign_in`：回调收到一次 `DeviceCode { verification_uri: "https://auth.openai.com/codex/device", user_code: "UC-1" }`；`chatgpt.json` 包含 `fresh-refresh`；模型 `b` 的 `ensure_signed_in` 为 `Ok` |
| `sign_in_clears_the_invalidated_mark` | 先 `note_failure_status(Some(401))`，`sign_in` 后 `ensure_signed_in` 为 `Ok` |
| `failed_device_request_reports_authentication` | `MockHttpResponse::error(SERVICE_UNAVAILABLE, "down")`：返回 `Authentication`；回调未被调用；没有 `chatgpt.json` |
| `stalled_device_request_times_out`（`#[tokio::test(start_paused = true)]`） | Task 1 的 `GatedHttpClient` 不放行：`sign_in` 返回 `Authentication`；回调未被调用 |
| `slow_authorization_is_not_limited_after_the_code` （`#[tokio::test(start_paused = true)]`） | 第一个响应返回设备码，之后的响应卡在 `Notify`：推进 60 秒后 `sign_in` 仍未结束，放行后成功 |
| `sign_in_without_auth_fails` | API key provider 返回 `Authentication` |
| `device_code_debug_hides_the_code` | `format!("{:?}", DeviceCode { .. })` 不含 code |

- [ ] **Step 2：确认测试失败**

Run: `cargo test -p provider --test upstream_auth`
Expected: 编译失败，提示 `sign_in` 不存在。

- [ ] **Step 3：实现 `ProviderAuth::sign_in(&self, http: &DynHttpClient, on_code: DeviceCodeSink) -> Result<(), ProviderConnectError>`**

用 `self.authenticator.clone().with_device_code_handler(DeviceCodeHandler::new(...))` 把 rig 的 `DeviceCodePrompt` 转成 `DeviceCode` 交给 `on_code`，再调用 `sign_in_with_device_code(http)`。成功后清除 `invalidated`。记录开始、拿到设备码、完成和失败，只写 provider ID 和错误类别。

设备码上限（`const DEVICE_CODE_LIMIT: Duration = Duration::from_secs(30)`）：回调通过 `tokio::sync::Notify` 通知已拿到 code；用 `tokio::select!` 同时等待登录 future，以及“`sleep(DEVICE_CODE_LIMIT)` 且尚未拿到 code”。超时时丢弃登录 future，返回 `Authentication(Box::new(AuthError::Message("timed out requesting a device code".into())))`。provider 的 tokio 依赖相应增加 `"macros"` 和 `"sync"`。

- [ ] **Step 4：确认测试通过**

Run: `cargo test -p provider`
Expected: 全部通过。

---

### Task 3: Kernel 转换错误并在两处拦截

**Files:**
- Modify: `crates/kernel/src/model.rs`、`crates/kernel/src/provider.rs`、`crates/kernel/src/runtime.rs`、`crates/kernel/src/runtime/run/attempt.rs`、`crates/kernel/src/runtime/run/admission.rs`、`crates/kernel/src/runtime/session/settings.rs`
- Test: `crates/kernel/tests/subagent/fixture.rs`、`crates/kernel/tests/protocol_alignment/settings.rs`、`crates/kernel/tests/protocol_alignment/insertion.rs`

**Interfaces:**
- Consumes：Task 1、Task 2 的 `ConfiguredModel` 方法和 `protocol` 类型。
- Produces：
  - `ModelError::AuthRequired { method_id: String }`，`#[error("sign-in required for provider {method_id}")]`
  - `Model::login_method(&self) -> Option<LoginMethod>`（默认 `None`）
  - `async fn Model::ensure_signed_in(&self) -> Result<(), ModelError>`（默认 `Ok(())`）
  - `async fn Model::sign_in(&self, on_code: DeviceCodeSink, cancellation: CancellationToken) -> Result<(), ModelError>`（默认 `Unavailable`）
  - `ModelCatalog::login_methods(&self) -> Vec<LoginMethod>`，按 `id` 去重
  - `async fn ModelCatalog::sign_in(&self, method_id: &str, on_code: DeviceCodeSink, cancellation: CancellationToken) -> Result<(), ModelError>`，未知方法返回 `Unavailable`
  - `Kernel::login_methods(&self) -> Vec<LoginMethod>`
  - `async fn Kernel::sign_in(&self, method_id: &str, on_code: DeviceCodeSink, cancellation: CancellationToken) -> Result<(), KernelError>`

**验收：** L08–L11。

- [ ] **Step 1：写失败测试**

`fixture.rs` 的 `RoutedModel` 增加 `pub sign_in_required: Arc<Mutex<Option<String>>>`（`create_catalog` 里的 secondary 共享同一个 `Arc`）、`pub sign_in_wait: Mutex<Option<Arc<Notify>>>`，并实现 `ensure_signed_in`（先等 `sign_in_wait`，再按 `sign_in_required` 返回 `AuthRequired`）。`Route` 增加 `Fail(fn() -> ModelError)`，`stream` 遇到时返回该错误。这个夹具也被 `subagent` 测试目标共享，而那里不构造该变体，所以要加 `#[allow(dead_code)]` 和说明注释（与夹具现有写法一致），否则 clippy `-D warnings` 会失败。

| 测试名（文件） | 断言 |
|---|---|
| `model_change_requiring_sign_in_changes_nothing`（`settings.rs`） | `set_session_model(.., "fixture", "secondary")` 返回 `KernelError::Model(ModelError::AuthRequired { method_id })`，`method_id == "chatgpt"`；`session_settings` 的模型不变；没有 `SettingsChanged` 事件 |
| `prompt_requiring_sign_in_is_rejected_before_insertion`（`insertion.rs`） | 用 `admission_ticket` 和 `PromptRequest::builder()`（写法同 `crates/acp/src/server.rs` 的 `session/prompt`）：`InsertionNotice` 收到 `Model(AuthRequired)`；`user_texts` 为空；模型没有收到请求 |
| `direct_command_skips_the_sign_in_gate`（`insertion.rs`） | 设置 `sign_in_required`，发送 `/session`：不被拒绝 |
| `handled_input_skips_the_sign_in_gate`（`insertion.rs`） | input hook 返回 `Handled`：`PromptOutcome::Handled` |
| `cancel_during_the_sign_in_check_rejects_the_prompt`（`insertion.rs`） | `sign_in_wait` 卡住时 `cancel_session`，放行后收到 `KernelError::PromptCancelled` |
| `auth_required_during_a_run_shows_an_assistant_error`（`insertion.rs`） | `Route::Fail` 返回 `AuthRequired`：用户消息已入历史；有 `StopReason::Error` 的助手消息，错误文字含 `sign-in required`；模型只收到 1 次请求（不重试） |
| `login_methods_are_deduplicated`（`model.rs` 的 `mod tests`） | 同一方法的两个模型只列一项；没有方法的模型不列 |
| `catalog_sign_in_forwards_to_the_matching_model`（`model.rs` 的 `mod tests`） | 转发给对应模型；未知方法返回 `Unavailable` |
| `provider_model_reports_auth_required`（`provider_runtime.rs`） | 空 `auth_dir`：`login_method` 是 `Some`；`ensure_signed_in` 和 `stream` 都返回 `AuthRequired` |
| `unauthorized_provider_response_requires_sign_in`（`provider_runtime.rs`） | 有效凭据，本地 `Server` 返回 401：`stream` 失败后 `ensure_signed_in` 返回 `AuthRequired` |
| `provider_sign_in_honours_cancellation`（`provider_runtime.rs`） | 已取消的 token：`sign_in` 返回 `ModelError::Cancelled`，不发网络请求 |

- [ ] **Step 2：确认测试失败**

Run: `cargo test -p kernel --test protocol_alignment && cargo test -p kernel --test provider_runtime`
Expected: 编译失败，提示 `AuthRequired`、`ensure_signed_in` 不存在。

- [ ] **Step 3：实现**

- `model.rs`：按 Interfaces 增加变体、trait 方法和 catalog 方法。`preflight` 不变。
- `provider.rs`：`ProviderModel` 实现 `login_method`、`ensure_signed_in`、`sign_in`。`sign_in` 用 `tokio::select! { biased; ... }` 先检查 `cancellation`，再等待 `configured.sign_in(on_code)`，取消时返回 `ModelError::Cancelled`。把 `SignInRequired` 转成 `AuthRequired`，其余沿用 `connection_failure`。`stream_request` 的每个 `ModelFailure::from(error)` 之后调用 `configured.note_failure_status(failure.status)`；`try_stream!` 内用 `Arc::clone(&self.configured)`。
- `attempt.rs`：匹配增加 `AuthRequired` 分支，用 `error.to_string()` 作为 summary，`ModelRetryDisposition::NonRetryable`。
- `runtime.rs`：`for_insertion` 增加 `Self::Model(ModelError::AuthRequired { method_id }) => Self::Model(ModelError::AuthRequired { method_id: method_id.clone() })`。`Kernel` 增加字段 `login_catalog: Arc<ModelCatalog>`，`KernelFactory::build` 把已构建的 `catalog` 传入；新增两个转发方法。
- `settings.rs`：`SessionSettingsUpdate::model` 在确认模型已配置之后、`settings_changes` 加锁之前调用 `model.ensure_signed_in().await?`，返回前记 `tracing::info!`。
- `admission.rs`：在 `if !direct` 分支内、`if request.ticket.cancellation.is_cancelled()`（`enqueue` 前的那处）之前调用 `session.model.active()?.ensure_signed_in().await?`。

- [ ] **Step 4：确认测试通过**

Run: `cargo test -p kernel`
Expected: 全部通过，包括 `queue.rs` 和 `turn.rs` 中现有的 `preflight` 测试。

---

### Task 4: ACP 声明登录方式并映射 -32000

**Files:**
- Modify: `crates/acp/src/server.rs`
- Create: `crates/acp/tests/login.rs`

**Interfaces:**
- Consumes：Task 3 的 `Kernel::login_methods`、`ModelError::AuthRequired`。
- Produces：测试夹具 `LoginModel`、`LoginModelFactory`、`with_client`（Task 5 继续使用）。

**验收：** L12、L13。

- [ ] **Step 1：写失败测试**

`login.rs` 夹具：
- `with_client` 沿用 `crates/acp/tests/elicitation.rs` 的写法，客户端声明 URL elicitation，把收到的 `CreateElicitationRequest` 和 responder 交给测试。
- Kernel 构建沿用 `crates/acp/tests/v2_alignment.rs` 的 `alignment_kernel`。
- `LoginModel`：`login_method` 返回 `LoginMethod { id: "chatgpt", name: "ChatGPT" }`；`signed_in: AtomicBool` 为假时 `ensure_signed_in` 返回 `AuthRequired`；`sign_in` 的行为由 Task 5 补充。
- `LoginModelFactory` 的 catalog 包含 `plain/open`（无登录）和 `chatgpt/astra`（`LoginModel`），`active` 可配置。

| 测试名 | 断言 |
|---|---|
| `initialize_advertises_login_methods` | `InitializeResponse.auth_methods` 只有一项 `Agent`，`method_id == "chatgpt"`，`name == "ChatGPT"` |
| `initialize_omits_login_methods_without_oauth` | 只有 `plain/open` 时 `auth_methods` 为空 |
| `model_switch_requires_sign_in` | `set_config_option` 切到 `chatgpt/astra`：错误码 -32000，`data == {"methodId":"chatgpt"}`；`session_settings` 仍是 `plain/open` |
| `prompt_requires_sign_in` | active 为 `chatgpt/astra`：`session/prompt` 错误码 -32000，`data` 同上；没有用户消息更新；Session 不进入 running |
| `session_opens_without_sign_in` | active 为 `chatgpt/astra`：`session/new` 成功 |

- [ ] **Step 2：确认测试失败**

Run: `cargo test -p acp --test login`
Expected: `auth_methods` 为空、错误码为 -32603，测试失败。

- [ ] **Step 3：实现（`server.rs`）**

- `initialize` 增加 `.auth_methods(...)`，把 `kernel.login_methods()` 映射成 `wire::AuthMethod::Agent(wire::AuthMethodAgent::new(id, name))`。
- `From<KernelFailure>` 增加分支：`KernelError::Model(ModelError::AuthRequired { method_id })` → `Self::auth_required().data(serde_json::json!({ "methodId": method_id }))`。
- `SessionConfigUpdate::run`：Kernel 结果的 `map_err` 改用 `KernelFailure(error).into()`。
- `session/prompt` 的分离任务：`Err(KernelError::Model(ModelError::AuthRequired { .. }))` 与 `PromptCancelled` 同样按入队前拒绝处理，记 `tracing::info!`，不发布 terminal fallback。

- [ ] **Step 4：确认测试通过**

Run: `cargo test -p acp`
Expected: 全部通过。

---

### Task 5: ACP 实现 `auth/login`

**Files:**
- Create: `crates/acp/src/login.rs`
- Modify: `crates/acp/src/lib.rs`、`crates/acp/src/server.rs`、`crates/acp/src/elicitation.rs`
- Test: `crates/acp/tests/login.rs`

**Interfaces:**
- Consumes：Task 3 的 `Kernel::sign_in`、`Kernel::login_methods`；Task 4 的测试夹具。
- Produces：
  - `ElicitationBridge::supports_url(&self) -> bool`
  - `LoginTask`（`typed-builder`，字段 `kernel: Arc<Kernel>`、`connection: V2ConnectionTo<Client>`、`url_elicitation: bool`、`method: LoginMethod`、`responder: Responder<wire::LoginAuthResponse>`），方法 `run(self) -> Result<(), agent_client_protocol::Error>`
  - elicitation 线上格式：`mode = url`，`requestId` = 这次 `auth/login` 的 ID，`_meta[ACP_NAMESPACE] = {"userCode": code, "methodId": id}`，`message = "Open {uri} and enter code {code} to sign in to {name}."`

**验收：** L14–L16。

- [ ] **Step 1：写失败测试**

夹具 `LoginModel::sign_in`：可选地先等 `before_code: Notify`；调用 `on_code`；等 `release: Notify` 成功，或 `cancellation` 取消，或按配置返回 `Unavailable`。用一个 `Drop` 守卫记录登录 future 已结束。

| 测试名 | 断言 |
|---|---|
| `login_elicits_with_request_scope_then_withdraws` | elicitation 的范围是这次 `auth/login` 的 `requestId`，`mode = url`，`message` 和 `_meta.userCode` 含 code；放行后客户端的 elicitation 请求被取消，`auth/login` 成功；随后切换到 `chatgpt/astra` 成功 |
| `declined_login_reports_cancelled` | 回复 `decline` 和 `cancel`（参数化）：`auth/login` 响应 -32800，登录 future 结束，模型不变 |
| `accepted_elicitation_keeps_waiting_and_completes` | 回复 `accept`：`auth/login` 仍未结束；放行后客户端收到 `elicitation/complete`，`auth/login` 成功 |
| `cancel_request_stops_login` | 客户端取消 `auth/login`：响应 -32800，登录 future 结束 |
| `disconnect_stops_login` | 登录进行中断开连接：登录 future 结束 |
| `unknown_method_is_invalid_params` | `methodId = "missing"`：错误码 -32602 |
| `client_without_url_elicitation_still_signs_in` | 客户端不声明 URL：没有 elicitation，放行后 `auth/login` 成功 |
| `cancel_before_device_code_stops_login` | 卡在 `before_code` 时取消：响应 -32800，没有 elicitation |
| `failed_sign_in_reports_an_error_without_elicitation` | `sign_in` 在 `on_code` 前返回 `Unavailable`：错误码 -32603，`data` 不含上游文字，没有 elicitation |
| `concurrent_logins_keep_their_own_elicitation` | 同一连接两次 `auth/login`：两个 elicitation 的 `requestId` 分别对应各自的请求；取消其中一个不影响另一个 |
| `login_logs_omit_device_code` | 沿用 `crates/acp/tests/tracing.rs` 的捕获方式，过滤级别 `debug`：全流程日志不含 code。如果失败，就对出站参数日志中的 `userCode` 和 `message` 脱敏 |

- [ ] **Step 2：确认测试失败**

Run: `cargo test -p acp --test login`
Expected: 错误为 method not found，测试失败。

- [ ] **Step 3：实现**

`server.rs` 注册 `wire::LoginAuthRequest` 处理函数：`context.traces.request` 建操作；`method_id` 不在 `kernel.login_methods()` 中时响应 `invalid_params`；否则构建 `LoginTask`（`url_elicitation` 取 `context.watchers.elicitations.supports_url()`），用 `connection.spawn` 运行。

`LoginTask::run` 的流程（下面的决定签名和测试无法替代）：

1. 先取 `responder.id().clone()` 和 `responder.cancellation()`；创建 `CancellationToken` 和 `mpsc::unbounded_channel::<DeviceCode>()`，`DeviceCodeSink` 把 code 发进通道。
2. 循环 `select!` 四路：`kernel.sign_in` 结束；请求被取消；通道收到 code；已发出的 elicitation 得到回复。
3. 收到 code：`url_elicitation` 为真时用 `connection.send_request` 发送 elicitation，保存 `cancellation_handle()` 和回复 future；否则 `eprintln!("{name} sign-in: {uri} (code: {code})")`。
4. elicitation 回复：`Accept` 记下已回复；`Decline`、`Cancel` 或未知动作时取消 token，响应 -32800；请求失败时回退到 stderr，继续等待。
5. 登录结束：未回复的 elicitation 用 `handle.cancel()` 撤销；已 `accept` 的发 `CompleteElicitationNotification`。
6. 响应：成功发 `LoginAuthResponse`；`KernelError::Model(ModelError::Cancelled)` 发 -32800；其他错误发 `internal_error().data("sign-in did not complete")`，不透出上游文字。

`elicitation_id` 用 `login-` 加 `requestId` 的 JSON 文本。记录开始、完成、取消和失败，只写方法 ID。

- [ ] **Step 4：确认测试通过**

Run: `cargo test -p acp`
Expected: 全部通过。

---

### Task 6: WebUI 登录引导

**Files:**
- Modify: `web/src/acp/connection.ts`、`web/src/acp/protocol.ts`、`web/src/domain/model.ts`、`web/src/workspace/controller.ts`、`web/src/features/mcp/ElicitationDialog.tsx`、`web/src/theme/workbench.css`

**Interfaces:**
- Consumes：Task 4 的 -32000 格式、Task 5 的 elicitation 格式。
- Produces：
  - `AcpRequestError.authMethodId: string | undefined`（getter，错误码 -32000 且 `data.methodId` 是字符串时返回）
  - `AcpProtocol.methods.authLogin = "auth/login"`
  - `InitializeResult.authMethods?: readonly { methodId: string; name: string }[]`
  - `ElicitationPrompt.sessionId` 改为可选；新增 `login?: Readonly<{ userCode: string; name: string }>`
  - `WorkspaceController.login(methodId: string): Promise<void>`

**验收：** L17。前端按项目规则不写单元测试，行为由 Task 7 的端到端脚本验证。

- [ ] **Step 1：实现控制器**

- `validateCapabilities` 保存 `authMethods`（`methodId → name`）。
- `login`：同一 `methodId` 进行中时返回同一个 Promise；发送 `auth/login`，不设超时。-32800 原样抛出；其他失败抛 `new Error("登录未完成，请重试")`。
- `setConfigOption`：捕获 `authMethodId`，等待 `login`，成功后再发一次，第二次的错误直接抛出。`login` 因用户取消抛出 -32800 时，`setConfigOption` 正常返回，不抛错。这样 `SessionConfigSelects` 不用改：取消不提示，其他失败沿用“切换失败”，`busy` 覆盖登录全程，下拉框受控显示原模型直到收到新配置。
- 发消息的 `.catch`：`authMethodId` 存在时，删除记录，调用 `returnInput(sessionId, submission, "需要先登录，登录后请重新发送")`，再调用 `login`；非取消的失败用 `notice/added` 显示标题 `登录未完成，请重试`。
- `elicit`：范围要求 `sessionId` 或 `requestId` 二选一，否则按现有方式报 -32602。`_meta` 中有字符串 `userCode` 时设置 `login`，`name` 取 `authMethods` 中 `methodId` 对应的值，查不到就用 `methodId`。

- [ ] **Step 2：实现弹窗**

`ElicitationDialog`：`prompt.login` 存在时渲染登录样式，其余逻辑不变。固定文案：

- 标题 `登录 {name}`；说明 `打开验证页面，输入下面的代码：`
- code 用 `<code>` 显示且可选中；旁边的按钮 `复制`，成功后短暂显示 `已复制`，`navigator.clipboard` 失败时不报错
- 状态 `等待授权…`，放在 `aria-live="polite"` 元素里
- 按钮 `取消`（`data-safe-focus`，回复 `cancel`）和 `打开验证页面`（`window.open(url, "_blank", "noopener,noreferrer")`，不回复）

样式写在 `workbench.css`，沿用 `.elicitation-url` 附近的变量。`session` 查询在 `sessionId` 缺失时不显示会话行。

- [ ] **Step 3：验证**

Run: `cd web && npm run check && npm run build && npm run test:elicitation`
Expected: 全部通过，`test:elicitation` 仍是 3 个通过。

---

### Task 7: 文档与端到端验收

**Files:**
- Modify: `README.md`、`README_zh.md`
- Create: `crates/app/tests/web/provider_login.e2e.mjs`

**Interfaces:**
- Consumes：完整的后端和 WebUI。

- [ ] **Step 1：更新 README**

中英文 ChatGPT 认证段落，把“首次请求需要完成一次设备登录，提示写入 stderr”改为：切换到未登录的模型或未登录时发消息，WebUI 会弹出登录引导；不支持 URL elicitation 的 ACP 客户端，登录码打印到 stderr。

- [ ] **Step 2：写端到端脚本**

沿用 `provider_migration.e2e.mjs` 的 CDP 写法；`CHROME_BIN` 未设置时默认 `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`。脚本从仓库 `claw.toml` 生成隔离配置：只替换 `auth_dir`（临时空目录）和 `active_model`（`--start`，默认 `deepseek/deepseek-flash`），不增加任何 fixture provider；`claw.toml` 没有 `auth_dir` 时直接报错退出。登录目标用 `--login-model` 指定，默认 `chatgpt/gpt-6-astra`。用 `CLAW_CONFIG` 和 `CLAW_SESSION_PERSISTENCE__DATA_HOME` 启动 `target/debug/clawcode`。模式：

| 模式 | 验收 | 步骤与断言 |
|---|---|---|
| `cancel` | L18 | 从非 OAuth 模型切到 `chatgpt/gpt-6-astra` → 弹窗显示 code → 点取消 → 下拉框恢复可用且仍是原模型；没有 `chatgpt.json` |
| `blocked-prompt` | L20 | `--start chatgpt/gpt-6-astra` → 发消息 → 弹窗出现、输入回到输入框、对话没有这条消息 → 取消 |
| `login` | L19 | 切换 → 在终端打印 URL 和 code → 最长等 15 分钟至弹窗关闭 → 下拉框变成新模型 → 发一条短消息并等到回复 |
| `stdio` | L15 | 启动 `clawcode stdio`，`initialize` 不声明 elicitation，发 `auth/login` → stderr 出现 `sign-in:` 和 code → stdout 每一行都是合法 JSON，且没有 `elicitation/create` → 发 `$/cancel_request`，收到 -32800 |

- [ ] **Step 3：运行自动模式**

Run: `cargo build -p app --bin clawcode && cd web && npm run build && cd .. && for mode in cancel blocked-prompt stdio; do node crates/app/tests/web/provider_login.e2e.mjs "$mode" || exit 1; done`
Expected: 三个模式都通过。每个模式只向 OpenAI 申请一个设备码。

- [ ] **Step 4：全量检查（L21）**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --no-fail-fast && pre-commit run --all-files`
Expected: 全部通过。这里必须带 `--all-files`：不带时 pre-commit 会临时 stash 未暂存的改动，违反“不改动暂存状态”的约束。

- [ ] **Step 5：请用户配合真实登录（L19）**

请用户运行 `node crates/app/tests/web/provider_login.e2e.mjs login`，用真实账号输入 code。这一步不能由代理代替。把结果连同 L01–L21 的状态报告给用户；未运行的项如实标注。
