# Clawcode ACP 与 Zed 集成对齐规格（范围 1-4）

## 动机

当前 `clawcode` 的 `crates/acp` 与 Zed 集成存在三类不一致：

1. 初始化能力与会话行为与目标客户端预期的边界不完全一致；
2. `session/set_config_option` 请求未在 ACP 侧接入；
3. Prompt 处理对非文本块的边界行为不明确（尤其会出现 `image` 类输入时的歧义）；
4. 鉴权能力在当前范围内明确不需要支持。

本规格只覆盖你已确认的范围（1-4），并明确排除 `image`、鉴权、slash command 相关功能。

## 设计目标

- 保持 ACP 会话能力可协商性，明确声明本期可用能力与不可用能力；
- 补齐 `set_session_config_option` 最小闭环，使 Zed 的配置修改调用不再失败；
- 规范文本 prompt 的输入转换逻辑，保证在无 image 支持时行为可预期；
- 不引入任何鉴权与 slash command 相关实现。

## 范围边界（本期）

- **包含**：规格 1）初始化能力声明；2）`SetSessionConfigOption` 请求链路；3）Prompt 非 image 输入处理；4）对应测试；
- **排除**：
  - image 输出/能力（`PromptCapabilities.image`、图片块输入、图片资源块处理）；
  - 鉴权（`Authenticate`、`auth_methods`）；
  - slash command（`/init`、`/compact`、`/review` 等）；
  - 与上述范围无关的 MCP/tool/events 深度改造。

## 核心设计（仅限范围 1-4）

### 1）Initialize 能力声明（去掉 image 与鉴权）

#### 1.1 责任文件
- `crates/acp/src/agent.rs`

#### 1.2 关键改动
- 在 `ClawcodeAgent::handle_initialize` 中保持 `embedded_context(true)`；
- 显式确认 `PromptCapabilities.image == false`（推荐通过 builder 显式写入 `image(false)`，避免默认语义变化）；
- 移除 `auth(...)` 能力声明；
- 保留并校验其他会话能力：`mcp_capabilities.http(true)`、`session_capabilities.close/list`。
- 流程约束：`serve()` 不再必须支持 `Authenticate`（如保留需明确禁用路径行为）。

#### 1.3 验收
- initialize 响应中 `prompt_capabilities.image` 为 `false`；
- `agent_capabilities.auth` 不对外暴露可用 auth 方法/能力入口；
- mcp/session 现有能力仍保持可用。

### 2）补齐 `SetSessionConfigOption` 请求

#### 2.1 责任文件
- `crates/acp/src/agent.rs`
- `crates/protocol/src/kernel.rs`
- `crates/kernel/src/lib.rs`

#### 2.2 关键改动
- 在 `serve()` 中新增 `on_receive_request(SetSessionConfigOptionRequest, ...)`；
- 新增 `ClawcodeAgent::handle_set_session_config_option`：
  - 参数校验：请求会话有效性、`config_id` 合法性、值类型合法性；
  - 成功：更新会话配置状态；
  - 返回：`SetSessionConfigOptionResponse::new(config_options)`；
  - 失败：返回可读错误码与错误原因。
- 将配置能力下沉路径明确：
  - 方案 A（本期最小闭环）：在 ACP 层维护会话级配置快照并回写；
  - 如你允许后续增强：将 `AgentKernel` 与 `Kernel` 增加配置设置接口，避免状态只在适配层停留。

### 3）Prompt 输入解析的非 image 化

#### 3.1 责任文件
- `crates/acp/src/agent.rs`

#### 3.2 关键改动
- `handle_prompt` 继续提取 `ContentBlock::Text`；
- 对非文本块采用固定策略：
  - `Image` / 资源类块：记录/返回“已忽略（unsupported）”；
  - 全部内容都不是文本时，返回明确错误而不是空请求；
- 在代码注释中增加英文说明：当前版本不支持 image/input resource block 的解析投递。

#### 3.3 验收
- 文本与非文本混合 prompt：仅文本入库并可稳定执行；
- 仅包含非文本的 prompt：返回可诊断错误；
- 不发生崩溃与未声明 panic。

### 4）测试与回归边界

#### 4.1 责任文件
- `crates/acp/src/agent.rs`

#### 4.2 关键用例
- `initialize_records_client_fs_capabilities_and_omits_image`：
  - 保留并增强对 `image=false`、auth 能力缺失的断言；
- `set_session_config_option_success`：
  - 请求有效配置后返回 config snapshot；
- `set_session_config_option_invalid_id`：
  - 未知配置项返回错误；
- `set_session_config_option_invalid_value`：
  - 无法解析/非法值返回错误；
- `prompt_with_non_text_only_block`：
  - 仅含非文本输入返回明确错误；
- `prompt_with_text_and_non_text`：
  - 文本有效，非文本可忽略，不影响执行。

## 修改清单（文件级）

| 文件 | 操作 | 说明 |
|------|------|------|
| `crates/acp/src/agent.rs` | 修改 | 初始化能力、serve 请求路由、`SetSessionConfigOption` 处理、prompt 文本抽取与非文本策略、相关测试 |
| `crates/protocol/src/kernel.rs` | 修改 | （可选）新增配置设置接口定义，供更长期一致性使用 |
| `crates/kernel/src/lib.rs` | 修改 | （可选）实现新增的配置设置接口 |
| `crates/kernel/src/lib.rs` | 修改 | `spawn_agent` 测试桩（若需）增加 config-option 对应桩方法 |

## 非目标（不做）

- 认证体系：
  - 不实现 `AuthenticateRequest`；
  - 不返回 `auth_methods` 与认证失败分支。
- 图片能力：
  - 不返回 `prompt_capabilities.image`；
  - 不实现 image block 输入转换；
  - 不实现 image tool/事件转发。
- 命令交互：
  - 不解析 slash command；
  - 不新增 `/compact` `/review` `/init` 等 command pipeline。

## 风险与约束

- `agent-client-protocol` 当前版本/feature 集合决定 `SetSessionConfigOptionRequest` 值载荷形式；实现时按本仓库实际依赖的 schema 进行匹配；
- 该范围内不强制要求 kernel 的配置持久化语义，即可先保证单会话会话内可用；
- 所有新增/修改代码需按 `AGENTS.md` 写明英文注释，函数级注释齐全。
