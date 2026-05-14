# System Prompt 构建系统设计

**日期**: 2026-05-14
**状态**: 待审核

参考规格: `docs/system-prompt-architecture_v4.md`（TypeScript OpenCode 原始设计）

---

## 1. 目标

为 clawcode 实现完整的 system prompt 构建管线，使 LLM 在每次请求中接收到结构化的分层系统指令。采用 `CompletionRequest.preamble` 注入方式，不污染 `ContextManager` 的对话存储。

## 2. 模块总览

```
crates/kernel/src/prompt/mod.rs          # 新增 - SystemPrompt 类型 + render()
crates/kernel/src/prompt/environment.rs  # 新增 - 环境信息采集
crates/kernel/src/prompt/instruction.rs  # 新增 - AGENTS.md 加载（可先 stub）
crates/kernel/src/lib.rs                 # 修改 - pub mod prompt
crates/kernel/src/turn.rs                # 修改 - TurnContext 加字段，注入 preamble
crates/kernel/src/session.rs             # 修改 - 传递 prompt 配置到 TurnContext
crates/kernel/src/agent/role.rs          # 修改 - AgentRole 加 prompt 字段
crates/protocol/src/op.rs                # 修改 - Op::Prompt 加 system 字段
```

**不改动**: `provider/` 全部、`context.rs`、`protocol/message.rs`

## 3. SystemPrompt 类型

### 3.1 核心结构

`kernel/src/prompt/mod.rs`:

```rust
/// 分层存储的 system prompt，render() 时按优先级拼接为最终字符串。
///
/// 拼接顺序（与 TS spec 一致）：
///   ① agent_prompt（非空时替换默认 prompt）
///   ② environment block
///   ② instructions（AGENTS.md + .agents/*.md）
///   ② skills XML（仅当 agent 有 skill 权限）
///   ③ user_prompt（最低优先级，临时注入）
#[derive(Clone, Debug, typed_builder::TypedBuilder)]
pub struct SystemPrompt {
    /// ① Agent 自定义 prompt — 非空时完全替换默认 system prompt
    #[builder(default, setter(strip_option))]
    pub agent_prompt: Option<String>,
    /// ②a 环境信息（model, cwd, git, platform, date）
    pub environment: EnvironmentInfo,
    /// ②b 指令文件内容（AGENTS.md + .agents/*.md），已格式化为最终文本
    #[builder(default, setter(strip_option))]
    pub instructions: Option<String>,
    /// ②c 技能注册表 XML，仅当 agent 拥有 skill 权限时填充
    #[builder(default, setter(strip_option))]
    pub skills_xml: Option<String>,
    /// ③ 用户临时注入的 system prompt
    #[builder(default, setter(strip_option))]
    pub user_prompt: Option<String>,
}
```

### 3.2 EnvironmentInfo

`kernel/src/prompt/environment.rs`:

```rust
/// 运行环境快照，注入到每个 LLM 请求中。
#[derive(Clone, Debug, typed_builder::TypedBuilder)]
pub struct EnvironmentInfo {
    /// 模型标识符（如 "deepseek-v4-pro"）
    pub model_id: String,
    /// 工作目录绝对路径
    pub cwd: PathBuf,
    /// 是否为 git 仓库
    pub is_git_repo: bool,
    /// 操作系统平台: "darwin" | "linux" | "win32"
    pub platform: String,
    /// 当前日期，格式 "YYYY-MM-DD"
    pub date: String,
}

impl EnvironmentInfo {
    /// 从当前环境采集信息
    pub fn capture(model_id: String, cwd: PathBuf) -> Self { ... }
}
```

### 3.3 默认 System Prompt

当 `agent_prompt` 为 `None` 时使用的默认 prompt，定义在 `prompt/mod.rs` 常量中。内容描述 clawcode 的基本能力和行为准则（参考 Claude Code 和 OpenCode 的默认 system prompt，结合本项目定位编写）。

### 3.4 render() 方法

```rust
impl SystemPrompt {
    /// 渲染最终 system prompt 字符串。
    /// 按 ① → ②a → ②b → ②c → ③ 顺序拼接，空段自动跳过。
    pub fn render(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        // ① Agent prompt 或默认 prompt
        parts.push(
            self.agent_prompt
                .clone()
                .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string())
        );

        // ②a 环境信息
        parts.push(self.environment.render_block());

        // ②b 指令文件
        if let Some(ref ins) = self.instructions {
            parts.push(ins.clone());
        }

        // ②c 技能注册表
        if let Some(ref skills) = self.skills_xml {
            parts.push(skills.clone());
        }

        // ③ 用户临时 prompt
        if let Some(ref user) = self.user_prompt {
            parts.push(user.clone());
        }

        parts.into_iter().join("\n")
    }
}
```

### 3.5 环境信息格式化

```rust
impl EnvironmentInfo {
    fn render_block(&self) -> String {
        format!(
            "You are powered by the model named {}.\n\
             Here is some useful information about the environment you are running in:\n\
             <env>\n  Working directory: {}\n  Is directory a git repo: {}\n  \
             Platform: {}\n  Today's date: {}\n</env>",
            self.model_id,
            self.cwd.display(),
            if self.is_git_repo { "yes" } else { "no" },
            self.platform,
            self.date,
        )
    }
}
```

## 4. 指令文件加载

`kernel/src/prompt/instruction.rs`:

### 4.1 加载策略

与 TS spec 一致：

1. **`AGENTS.md`** — 从工作目录向上查找至项目根，取第一个匹配
2. **`.agents/` 目录下的 `.md` 文件** — 仅加载当前工作目录下的 `.agents/` 中的所有 `.md` 文件

### 4.2 去重规则

- 以 `(文件名, 文件内容哈希)` 为 key 去重
- `.agents/` 中与 `AGENTS.md` 内容完全一致的文件 → 跳过
- `.agents/` 中多个文件内容相同 → 仅保留第一个
- 最终按 `AGENTS.md` 优先、`.agents/*.md` 随后的顺序拼接

### 4.3 格式化

```text
Instructions from: /absolute/path/to/AGENTS.md
<content>

Instructions from: /absolute/path/to/.agents/review.md
<content>
```

### 4.4 核心函数

```rust
/// 加载指令文件并返回格式化后的文本。
/// 返回 None 表示没有找到任何指令文件。
pub(crate) fn load_instructions(cwd: &Path) -> Option<String> { ... }
```

### 4.5 P1 简化

P1 阶段仅加载 `AGENTS.md`，后续阶段补充 `.agents/` 支持和去重逻辑。

## 5. 注入点与数据流

### 5.1 TurnContext 扩展

`kernel/src/turn.rs`:

```rust
pub(crate) struct TurnContext {
    // ... 现有字段 ...
    /// ① Agent 自定义 prompt — None 时使用默认 prompt
    #[builder(default, setter(strip_option))]
    pub agent_prompt: Option<String>,
    /// ③ 用户临时 system prompt
    #[builder(default, setter(strip_option))]
    pub user_system_prompt: Option<String>,
}
```

### 5.2 execute_turn() 注入逻辑

```
execute_turn()
  │
  ├─ 1. 构建 EnvironmentInfo（从 ctx 和环境采集）
  ├─ 2. 加载指令文件（如有）
  ├─ 3. 构造 SystemPrompt { agent_prompt, environment, instructions, skills_xml, user_prompt }
  ├─ 4. let rendered = system_prompt.render();
  ├─ 5. CompletionRequest::builder()
  │      .preamble(rendered)     // ← 注入点
  │      .chat_history(history)
  │      .tools(...)
  │      .build()
  │      // build() 内部自动将 preamble 转为 Message::system 插入 history[0]
  └─ 6. llm.stream(request)
```

### 5.3 注入方式选型结论

选用 `CompletionRequest.preamble`（方式 A），理由：

- `preamble` 在 `CompletionRequestBuilder::build()` 中已有完整链路
- 不需要修改 `ContextManager` trait
- system prompt 不存入对话历史，保持元指令与对话内容的语义边界
- 每次 `build()` 重新注入，天然支持 mode 切换后的 prompt 变更

## 6. AgentRole 扩展

`kernel/src/agent/role.rs` — 在现有 `config_overrides` 基础上增加 `prompt` 字段：

```rust
pub struct AgentRole {
    pub name: String,
    pub description: String,
    pub nickname_candidates: Vec<String>,
    pub config_overrides: HashMap<String, String>,  // "model", "reasoning_effort"
    pub prompt: Option<String>,  // 新增：agent 自定义 system prompt
}
```

### 6.1 内置角色

| 角色 | prompt | 说明 |
|------|--------|------|
| `default` | `None` | 使用默认 system prompt，全能力 |
| `explorer` | `Some(include_str!("prompts/explorer.txt"))` | 只读搜索特化 |
| `worker` | `None` | 使用默认 system prompt，高 reasoning |

### 6.2 Prompt 文件

新增 `crates/kernel/src/prompts/` 目录（与 `prompt/` 子模块区分，存放静态 txt）：

```
crates/kernel/src/prompts/
  explorer.txt    # explorer subagent 的专用 prompt
  worker.txt      # worker 专用（预留给未来）
  default.txt     # 默认 system prompt（可选外置）
```

## 7. 合成提醒（Synthetic Reminders）

合成提醒不放入 `system` 数组，而是作为文本片段注入到用户消息或 assistant 消息中，确保突破 AI SDK 的 system prompt 缓存而 100% 送达模型。

P1 阶段暂不实现，预期后续阶段实现：

| 触发条件 | 提醒内容 | 注入位置 |
|---|---|---|
| plan agent 模式 | "CRITICAL: Plan mode ACTIVE — READ-ONLY phase" | 用户消息前缀 |
| plan→build 切换 | "Mode changed from plan to build" | 用户消息前缀 |
| step > 1 | `<system-reminder>The user sent: ...</system-reminder>` | 包装用户消息 |
| step >= max_steps | 最大步数警告 | assistant 消息后缀 |

## 8. 依赖关系

```
prompt/mod          ──→ prompt/environment
prompt/environment  ──→ 无内部依赖（仅 std + chrono）
prompt/instruction  ──→ 无内部依赖（仅 std::fs + std::path）
kernel/turn         ──→ prompt/mod（构造 SystemPrompt + render）
kernel/session      ──→ protocol/op（传递 user_system_prompt）
kernel/agent/role   ──→ 无新增依赖
```

## 9. 不变式与错误语义

- **默认 prompt 回退**: `agent_prompt` 为 `None` 时，始终使用 `DEFAULT_SYSTEM_PROMPT`。不存在空 system prompt 的情况。
- **环境信息不失败**: `EnvironmentInfo::capture()` 始终成功——即使 git 检测失败也返回 `is_git_repo: false`。
- **指令文件无感知**: `load_instructions()` 失败时返回 `None`，不阻塞 turn 执行。
- **preamble 不持久化**: 每次 `build()` 时注入，不在 `ContextManager` 中存储。
- **用户 prompt 最低优先级**: 追加在末位，不覆盖 agent prompt 或指令文件。

## 10. 测试点

- `SystemPrompt::render()`: 各层组合输出正确；空层正确跳过；仅 agent_prompt 时输出正确
- `EnvironmentInfo::render_block()`: 格式化字符串包含所有字段
- `load_instructions()`: 找到 AGENTS.md 时返回非空；找不到时返回 None；去重逻辑正确
- `AgentRole`: 新字段默认值为 None；explorer 角色的 prompt 非空
- 集成: `execute_turn()` 的 `CompletionRequest` 包含 preamble，且 preamble 内容包含环境信息
