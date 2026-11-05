# ACP 登录：在 WebUI 引导 OAuth provider 登录的设计规格

日期：2026-11-05。状态：设计已获口头确认，待审阅本文。前置：[rig-core 迁移设计](./2026-10-30-rig-core-provider-migration-design.md)，本文取代其 §9 中“首次请求触发设备登录、提示写入 stderr”的部分；[provider registry 设计](./2026-11-01-provider-registry-design.md)，沿用其“认证与凭证只在 provider crate”的边界。

## 1. 目标与已确认决策

目标：切换到尚未登录的 OAuth 模型时，用 ACP 的 auth 协议在 WebUI 里完成登录，不再依赖终端 stderr，模型请求也不再为等待登录而阻塞。

成功标准：用户在 WebUI 选择未登录的模型，弹窗显示验证链接和 code；完成授权后不用重启、不用刷新，切换自动完成，消息能正常发出。

| 决策 | 结论 |
|---|---|
| 切换时机 | 先拦下切换：不写入、不换模型；登录成功后自动完成切换，取消则留在原模型 |
| 登录方式 | 只用设备码；以后加浏览器登录只需在 `authMethods` 多列一个方法 |
| 拦截位置 | Provider 是唯一判断方；Kernel 只转换 provider 的错误、转发登录配置和调用 |
| 协议 | 标准 `authMethods`、`auth/login`、`auth_required`（-32000）；不新增私有扩展方法 |
| 覆盖对象 | 配了 `auth` 的 provider，目前只有 ChatGPT；API key 类 provider 行为不变 |
| 其他触发点 | 启动时已选中未登录模型、登录失效后发消息，走同一个引导 |
| 发消息被拦后 | 输入还回输入框，登录后由用户手动再发，不自动重发 |
| 配置 | 无新增配置；沿用 `auth = { type = "chatgpt", auth_dir = "..." }` |

## 2. 现状

- `crates/provider/src/auth.rs` 构造 `Authenticator` 时 `allow_device_flow = true`，第一次模型请求才触发登录，提示用 `eprintln!` 写到 stderr，请求最长阻塞 15 分钟。
- `Model::preflight` 已存在，Turn 开始、Compaction 和 MCP sampling 都会调用，`ProviderModel` 目前直接返回 `Ok(())`。Turn 层的调用在 `TurnStart` 之前：失败时不产生助手错误消息，已认领的排队消息退回队列（`crates/kernel/tests/protocol_alignment/queue.rs` 的现有测试断言了这一点），WebUI 的更新解码也不读 Run 的 `outcome`。因此本文不改变 `preflight` 的行为。
- `connect()` 失败走 `attempt.rs` 路径，Turn 以助手错误消息结束，错误显示在对话里。
- `KernelError::Model(#[from] ModelError)` 已存在，新的 `ModelError` 变体可以直接向上传递。
- `InsertionNotice::rejected` 经 `KernelError::for_insertion` 复制错误，未列出的类型会变成 `Protocol(String)`。
- 扩展的 `set_model` 经由 Kernel 的同一条切换路径，不经过 ACP。
- `session/prompt` 在消息入队前失败时，WebUI 已能把输入还回输入框（`returnInput`）。
- WebUI 已支持 URL 模式 elicitation，但当前实现要求 `sessionId`，且点击“打开并继续”后立即关闭弹窗。

## 3. 分层与职责

### 3.1 protocol

新增跨层共享的展示类型，不含凭据：

```rust
pub struct LoginMethod { pub id: String, pub name: String }
pub struct DeviceCode { pub verification_uri: String, pub user_code: String }
pub type DeviceCodeSink = Arc<dyn Fn(DeviceCode) + Send + Sync>;
```

### 3.2 provider：唯一判断方

`ConfiguredModel` 的 `auth: Option<Authenticator>` 改为 `auth: Option<Arc<ProviderAuth>>`，同一 provider 的模型共享一份：

```rust
pub(crate) struct ProviderAuth {
    authenticator: Authenticator, // allow_device_flow = false
    method: LoginMethod,          // id = provider id, name = display_name
    invalidated: AtomicBool,      // the model endpoint answered 401
}

pub enum ProviderConnectError {
    SignInRequired { method_id: String },
    SignInFailed(&'static str), // our own text, so it is safe to show and log
    Authentication(Box<AuthError>),
}

impl ConfiguredModel {
    pub fn login_method(&self) -> Option<&LoginMethod>;
    pub async fn ensure_signed_in(&self) -> Result<(), ProviderConnectError>;
    pub async fn sign_in(&self, on_code: DeviceCodeSink) -> Result<(), ProviderConnectError>;
    pub fn note_failure_status(&self, status: Option<u16>);
}
```

规则：

- **判定“需要登录”：** `invalidated` 为真，或 `auth_context` 返回 rig 的“需要登录”错误。rig 用 `AuthError::Message` 表达这个错误，只能匹配文本。这里匹配完整前缀 `ChatGPT sign-in required`，因为刷新失败的错误会嵌入服务器返回的文字，只匹配短语可能误判。匹配只在一处，并用测试对照固定的 rig 版本。
- **`connect()` 与 `ensure_signed_in()` 用同一套判定。** 请求路径因此不会再进入设备登录。
- **`ensure_signed_in()` 只会返回 `SignInRequired`。** 其他错误（网络、刷新接口 5xx、文件损坏）记录日志后返回 `Ok(())`，留给真正的请求报告。
- **限时但不取消：** `ensure_signed_in()` 在独立任务里调用 `auth_context`，最多等待 10 秒；超时只放弃等待，刷新继续在后台完成。直接取消进行中的刷新可能丢失轮换后的 refresh token。provider crate 因此新增 `tokio` 依赖（经 workspace 声明）。
- **`note_failure_status`：** 配了 `auth` 的 provider 收到 401 时打上 `invalidated`。403 不算，它可能是套餐权限问题。
- **`sign_in`：** 用 `authenticator.clone().with_device_code_handler(...)` 调用 `sign_in_with_device_code`，成功后清除 `invalidated`。clone 共享存储、刷新锁和内存中的凭据，所有模型立即可用。
- **设备码上限：** 从开始登录到拿到设备码最多等 30 秒，超时返回 `SignInFailed`。这段时间弹窗还没出现，用户无法取消，不能无限等待。拿到设备码后由用户在弹窗里决定何时取消。
- **HTTP 注入：** 新增 `ProviderFactory::with_http(config, http)`，`new` 委托给它并传入 `rig_reqwest::shared()`。

### 3.3 kernel：只转换和转发

```rust
pub enum ModelError {
    // ...
    AuthRequired { method_id: String },
}

pub trait Model {
    // ...
    fn login_method(&self) -> Option<LoginMethod> { None }
    async fn ensure_signed_in(&self) -> Result<(), ModelError> { Ok(()) }
    // Dropping the future stops the sign-in. The default answers Unavailable.
    async fn sign_in(&self, on_code: DeviceCodeSink) -> Result<(), ModelError>;
}
```

- `preflight` 不变，Turn、Compaction 和 MCP sampling 的现有行为不受影响。
- `ProviderModel::ensure_signed_in` 调用 provider 的同名方法并转换错误；`stream_request` 把 `connect()` 的 `SignInRequired` 转成 `AuthRequired`，其他认证错误保持现状；把失败状态码交给 `note_failure_status`，不自行判断。
- `AuthRequired` 不可重试：`attempt.rs` 的匹配增加分支，按不可重试失败写入助手错误消息。`compaction.rs` 已有通配分支，不用修改。
- `for_insertion` 增加分支，保留 `Model(AuthRequired)` 的类型，否则 ACP 无法映射为 -32000。
- 新增两处 `ensure_signed_in` 调用，Kernel 不做任何判断，只传递结果：
  - `SessionSettingsUpdate::model`：确认模型已配置之后、获取 `settings_changes` 锁之前。
  - `admit_prompt`：在 `if !direct` 分支内、input hook 处理完之后、`enqueue` 前现有的 `request.ticket.cancellation.is_cancelled()` 检查之前，这样等待期间发生的取消仍然生效。直接命令、`!bash` 和被 hook 标记为 `Handled` 的输入不受影响。
- `Kernel::login_methods()` 和 `Kernel::sign_in(method_id, on_code)`：Kernel 保留 `KernelFactory::build` 已构建的 catalog，按 `id` 去重列出方法，并转发给该 provider 的任一模型。不在每次 `initialize` 时现建，否则握手要克隆并校验配置，还多出一个失败路径。模型共享 provider 定义，登录后 Session 立即可用。

### 3.4 acp：协议映射

- `initialize` 通过 `.auth_methods()` 返回 `AuthMethod::Agent` 列表；没有 OAuth provider 时为空。
- `KernelFailure` 与 `session/set_config_option` 把 `KernelError::Model(ModelError::AuthRequired { method_id })` 映射为 `Error::auth_required().data({"methodId": method_id})`。
- `session/prompt` 的分离任务把这个错误视为入队前的正常拒绝：记 info 日志，不发布 terminal fallback。
- 新模块 `crates/acp/src/login.rs` 用一个持有 Kernel、连接和 elicitation 能力的结构体实现 `auth/login`，详见 §4.3。

## 4. 流程

### 4.1 切换模型

1. WebUI 发 `session/set_config_option`；`ensure_signed_in` 返回 `AuthRequired`；没有写入，响应 -32000 和 `methodId`。
2. WebUI 发 `auth/login { methodId }`。
3. 完成 §4.3 的登录后，`auth/login` 成功响应。
4. WebUI 自动重试一次切换。如果仍然返回 -32000，显示切换失败，不再循环。

### 4.2 发消息

1. `admit_prompt` 被拦下，消息不进入历史，`session/prompt` 响应 -32000。
2. WebUI 把输入还回输入框并发起登录。
3. 登录后由用户手动再发一次。input hook 在这种情况下会再运行一次。

Run 进行中才失效时（refresh token 失效，或 token 被吊销），`connect()` 返回 `SignInRequired` 或模型接口返回 401，这个 Turn 以助手错误消息结束，错误显示在对话里；401 同时打上失效标记。下一次发消息进入上述流程。

Compaction 和 MCP sampling 同样走 `connect()`，直接报错，不等待登录。`/compact` 这类直接命令和子代理不经过这个拦截点，只显示错误，不弹窗。

`session/new` 与 `session/resume` 不要求登录，否则无法打开 Session，也就没法切换到其他模型。

### 4.3 `auth/login`

1. 校验 `methodId`，未知方法响应 -32602。
2. 用 `connection.spawn` 在独立任务中运行，连接继续处理其他请求。任务同时等待三件事：登录完成、elicitation 被拒绝或取消、`responder.cancellation()`。
3. 设备码回调触发时：
   - 客户端支持 URL elicitation：发送 `elicitation/create`，`mode = url`，范围是 `requestId`（这次 `auth/login` 的 JSON-RPC ID），`url` 为验证地址，`message` 是含 code 的英文说明，供没有专门样式的客户端显示；`_meta` 的 `ProductIdentity::ACP_NAMESPACE` 下放 `userCode` 和 `methodName`，WebUI 据此显示登录样式、标题和复制按钮。
   - 客户端不支持，或发送失败：把 code 打印到 stderr，继续等待。
4. 客户端回复 `accept` 时继续等待（只表示用户同意打开页面）；回复 `decline` 或 `cancel` 时放弃登录。
5. 登录结束后先收尾 elicitation，再响应 `auth/login`：客户端尚未回复的，用 `$/cancel_request` 撤销；已回复 `accept` 的，发送 `elicitation/complete`，因为对已回复的请求发取消不起作用。WebUI 不回复 `accept`，始终走前一种。

## 5. 错误处理

| 情况 | 结果 |
|---|---|
| 没有凭据、refresh token 失效、模型接口曾返回 401 | -32000，进入登录 |
| 网络不通、刷新接口 5xx、检查超过 10 秒 | 不拦截，切换照常成功，到请求时报原错误 |
| 凭据文件损坏 | 不拦截，请求时报 JSON 错误，删除文件后可重新登录 |
| 用户在弹窗点取消或按 Esc | 登录停止，`auth/login` 响应 -32800，模型不变，不显示错误 |
| 客户端发 `$/cancel_request` 或断开连接 | 登录停止 |
| 15 分钟未完成，或轮询时网络中断 | `auth/login` 报错，WebUI 统一提示“登录未完成，请重试”，模型不变。上游只用文本表达这些原因，这里不细分 |
| 申请设备码 30 秒没有响应 | `auth/login` 报错，WebUI 提示“登录未完成，请重试”，模型不变 |
| 客户端不支持 URL elicitation | code 回退到 stderr，其余流程相同 |
| 两个标签页同时登录 | 各拿一个 code，互不影响，先完成的生效；另一个需手动取消 |

## 6. WebUI

### 6.1 登录弹窗

基于 `ElicitationDialog` 增加登录样式：当 elicitation 带有 `userCode` 时使用。

```text
┌ 登录 ChatGPT ────────────────────────┐
│ 打开验证页面，输入下面的代码：       │
│                                      │
│        ABCD-EFGH   [复制]            │
│                                      │
│ 等待授权…                            │
│              [取消]  [打开验证页面]  │
└──────────────────────────────────────┘
```

- “打开验证页面”只打开新标签页，不回复 elicitation，弹窗保持显示。
- 弹窗在服务端撤销 elicitation 时关闭（现有的 `signal` abort 路径）。
- 取消和 Esc 回复 `cancel`。没有“拒绝”按钮。默认焦点落在安全按钮上，不落在打开外部页面的按钮上。
- “等待授权…”使用 `aria-live`，让屏幕阅读器能读到。
- 复制失败时，code 依然可以手动选中。

### 6.2 控制器

- `login(methodId)`：同一方式进行中时复用同一个请求。
- `setConfigOption`：捕获 -32000，等待登录，成功后重试一次。
- 发消息的失败分支：收到 -32000 时调用 `returnInput`，再发起登录；登录失败用现有的 notice 提示。
- `elicit` 接受 `requestId` 范围；`ElicitationPrompt.sessionId` 改为可选，新增可选的 `userCode`。没有 `userCode` 的 elicitation 继续使用现有样式。

### 6.3 模型下拉框

登录期间保持禁用并显示原模型，成功后才变成新模型。取消（-32800）不提示；其他失败沿用现有的“切换失败”，详情放在提示文字里。

## 7. 日志与安全

- 记录登录的开始、拿到设备码、完成、取消和失败，以及切换或发消息被拦。只写 provider ID 和错误类别。
- code 和 token 不写入 tracing。实现时确认 ACP DEBUG 参数日志是否覆盖出站 elicitation；如果覆盖，把 `userCode` 和含 code 的 `message` 脱敏，并加测试。
- `auth/login` 与现有 ACP 端点处于同一信任边界，本文不新增访问控制。
- Kernel 只接触 `LoginMethod` 和 `DeviceCode`，不接触 access token 或 refresh token。

## 8. 文档

更新 `README.md` 与 `README_zh.md` 的 ChatGPT 认证段落：登录改为在 WebUI 引导；不支持 URL elicitation 的客户端才回退到 stderr。历史 spec 和 plan 不修改。

## 9. 测试与验收

Rust 按 TDD 进行：先写失败测试。`ProviderFactory::with_http` 使刷新和设备登录能离线模拟（上游的认证 URL 是固定的，但 HTTP 客户端可替换）。

| 编号 | 场景 | 必须证明的结果 |
|---|---|---|
| L01 | Provider 无凭据 | `ensure_signed_in` 与 `connect` 都返回 `SignInRequired`，且没有发出设备登录请求 |
| L02 | Provider 有效凭据 | 放行，请求携带该凭据 |
| L03 | 刷新失败分类 | `invalid_grant` 返回 `SignInRequired`；5xx 与网络错误时 `ensure_signed_in` 返回 `Ok` |
| L04 | 刷新超时 | 超过上限后返回 `Ok`，后台刷新完成并写入存储 |
| L05 | 401 | 打上失效标记后 `ensure_signed_in` 返回 `SignInRequired`；登录成功后清除；403 不打标记 |
| L06 | 上游文案 | 固定版本 rig 的“需要登录”错误被正确分类；服务器错误文字里含 `sign-in required` 的刷新失败不被误判 |
| L07 | 设备登录 | 模拟完整流程：回调收到 code，凭据写入 `chatgpt.json`，同 provider 的其他模型立即可用；申请设备码挂起时 30 秒后报错 |
| L08 | Kernel 切换 | 假 `Model` 的 `ensure_signed_in` 返回 `AuthRequired`：Session 文件没有新记录，当前模型不变 |
| L09 | Kernel 发消息 | 消息不进入历史；直接命令和 `Handled` 输入不受影响；`InsertionNotice` 收到的错误仍是 `Model(AuthRequired)`；等待检查期间的取消生效 |
| L10 | Kernel 运行中失效 | 通过拦截点后，模型请求返回 `AuthRequired`：对话里有助手错误消息，Run 以 `Failed` 结束，不重试 |
| L11 | Kernel 登录转发 | 方法按 `id` 去重；未知方法报错 |
| L12 | ACP 初始化 | 有 OAuth provider 时返回 `authMethods`，没有时为空 |
| L13 | ACP 错误映射 | 切换和发消息都响应 -32000，`data.methodId` 正确 |
| L14 | ACP 登录 | elicitation 使用 `requestId` 范围；成功后撤销并响应；`decline`/`cancel` 停止登录并响应 -32800；`accept` 不中断，且结束时收到 `elicitation/complete` |
| L15 | ACP 其他路径 | `$/cancel_request` 和断开连接都能中断；未知方法响应 -32602；不支持 URL 的客户端回退 stderr 且 stdout 无污染 |
| L16 | 日志 | 全流程日志不含 code 和 token |
| L17 | WebUI 静态检查 | `npm run check`、`npm run build`、`test:elicitation` 通过 |
| L18 | WebUI 取消（自动） | 真实 `app` 后端和 ChatGPT provider，空的隔离 `auth_dir`：切换 → 弹窗显示 code → 取消 → 模型不变、下拉框恢复可用。只向 OpenAI 申请一个设备码 |
| L19 | WebUI 成功（需用户配合） | 同样环境，用真实账号输入 code：弹窗自动关闭，切换自动完成，消息发出且有回复 |
| L20 | WebUI 发消息被拦 | 未登录时发消息：不进入历史，输入还回输入框，弹窗出现 |
| L21 | 工程检查 | `cargo fmt`、clippy（`-D warnings`）、workspace 测试和相关 pre-commit hook 通过 |

## 10. 风险与已知限制

- rig 的“需要登录”错误是文本。依赖固定在 `0.46`，升级时 L06 会失败，不会悄悄失效。
- 真实 refresh 失效无法按需触发，L03 只有模拟响应。
- 不联网时无法发现被吊销但未过期的 token，要等模型接口返回 401 后才知道，所以这种情况下第一条消息会失败。
- 凭据文件损坏不会进入登录引导，需要手动删除文件。
- 标签页之间不同步：一个标签页登录成功后，另一个标签页的弹窗要手动取消。
- `/compact` 这类直接命令和子代理在未登录时只显示错误，不弹窗。
- 失效标记只存在内存里。重启后，被吊销但未过期的 token 会让第一条消息再失败一次。
- HTTP/SSE 传输下 `requestId` 范围的 elicitation 能否送达，尚未确认；WebUI 使用 WebSocket，不受影响。
- 第三方 ACP 客户端的兼容性未实测，这里只验证协议行为和 WebUI。

## 11. 不在范围

登出（`auth/logout`）、模型列表的“未登录”标记、`auth/status`（unstable）、浏览器回调登录、MCP OAuth 流程、`prompt_cache` 默认值。
