# Codex Auth Provider Design

## 背景

当前 ChatGPT provider 的认证主要依赖配置中的 `api_key`，这对已经在 `~/.codex/auth.json` 里维护 token 的用户不友好。用户希望通过 Codex 的订阅凭证直接驱动 clawcode 的 ChatGPT 调用，而不是重复配置 token。该需求必须保持向后兼容，且不依赖外部二进制。

## 目标

1. 支持 provider 配置写法 `auth = { type = "codex" }`。
2. 对 `[[providers]]` 中 `provider_type = "responses" && id = "chatgpt"`，在 `auth.type = "codex"` 时不再依赖 `api_key`。
3. 默认读取 `${CODEX_HOME}/auth.json`，若 `CODEX_HOME` 未配置则读取 `${HOME}/.codex/auth.json`。
4. 读取和写回 codex auth 文件时保留除 token 外的其他字段。
5. 现有 `api_key` 路径不变，默认仍按现有行为走。
6. 所有改动先通过 spec 与实现步骤明确，避免一次性大改。

## 非目标

1. 不新增独立的 Codex 登录命令。
2. 不改造外部 shell 登录流程；仍使用现有 OAuth/refresh 机制。
3. 不改变 ACP、kernel 或 tool 协议边界。
4. 不引入数据库、K/V 索引或全新 provider plugin 机制。

## 设计

### 1. 配置模型

在 `crates/config/src/llm.rs` 中扩展 provider schema：

1. `LlmProvider` 新增可选字段：
   - `api_key: Option<ApiKeyConfig>`（当前保持兼容）
   - `auth: Option<ProviderAuthConfig>`（新增）
2. 新增 `ProviderAuthConfig`：
   - 采用 serde tag：`type`
   - 支持 `type = "codex"`
   - 可选字段 `auth_file`（未配置时用默认路径）

示例：

```toml
[[providers]]
id = "chatgpt"
provider_type = "responses"
display_name = "ChatGPT"
base_url = "https://chatgpt.com/backend-api/codex"
auth = { type = "codex" }

[[providers.models]]
id = "gpt-5.4"
```

配置兼容性规则：

- 传统 ChatGPT 配置继续支持 `api_key`。
- `codex` auth 与 `api_key` 同时存在时，优先走 `auth` 路径。

### 2. ProviderFactory 路由

在 `crates/provider/src/factory/mod.rs` 的 `build_one` 中按 provider 分支调整：

1. `ProviderId::Chatgpt` 分支不再先统一 resolve `api_key`。
2. 当 provider 配置命中 `auth.type = "codex"`：
   - 使用 ChatGPT client 的 OAuth/token-file 分支构建客户端。
   - 不要求 `api_key`。
3. 当未配置 `auth` 时：
   - 继续要求 `api_key` 存在。
   - 解析失败返回明确错误（provider id + 缺失项）。
4. 错误信息要求英文，且能帮助快速判断是“缺 token 文件”还是“缺 api_key”。

### 3. Codex auth 文件读取与写回

在 `crates/provider/src/providers/chatgpt/auth/native.rs` 与相关 builder 流程增加兼容层。

#### 3.1 读取路径

默认 auth 文件路径决策：

1. 如果 provider 指定 `auth_file`，优先使用配置路径。
2. 否则按环境变量顺序决定：
   - `CODEX_HOME` -> `<CODEX_HOME>/auth.json`
   - fallback -> `${HOME}/.codex/auth.json`

#### 3.2 字段兼容

Codex 文件中 token 通常是嵌套结构：

```json
{ "tokens": { "access_token": "...", "refresh_token": "...", "id_token": "...", "account_id": "..." } }
```

旧的 chatgpt/native 兼容 flat 格式仍保留。

读取策略按优先级尝试：

1. 先读 `tokens` 嵌套；
2. 再回退到 top-level 的 `access_token/refresh_token/id_token/account_id`。

#### 3.3 写回策略

写回 token 时采用“保留字段”策略：

1. 先按 JSON value 读取整个 auth 文件。
2. 仅更新/新增 `tokens` 节点（包含 access/refresh/id/account）
3. 不覆盖其他 top-level 字段，例如：
   - `OPENAI_API_KEY`
   - `auth_mode`
   - `agent_identity`
   - `last_refresh`

### 4. 最小实现顺序（按步骤）

1. 更新 config schema 与反序列化测试。
2. 修改 factory 分支路由和错误路径。
3. 增加 codex auth 文件默认路径与文件读写兼容。
4. 补充 provider 构造与 auth 解析的单元测试。
5. 执行 review 与逐条修复。

## 验收标准

1. `[[providers]]` 配置仅含 `auth = { type = "codex" }`，且 chatgpt responses provider 能成功初始化。
2. 未配置 `auth` 且未配置 `api_key` 的 chatgpt provider 明确报错。
3. `~/.codex/auth.json` 可包含旧字段与 `tokens`，读写后字段不丢失。
4. 现有仅 `api_key` 的配置行为不变。
5. Codex auth 与 api_key 同时出现时行为稳定可控（codex auth 优先）。

## 风险与回滚点

1. 若 codex `auth.json` 结构继续变化，需及时补充兼容映射。
2. 若 chatgpt 客户端内部 auth 模式有更严格要求，可能需要在 builder 上增加 codex 专用开关。
3. 若发现 refresh 失败场景过多，应在错误中明确提示是否 token 过期或文件路径错误。

