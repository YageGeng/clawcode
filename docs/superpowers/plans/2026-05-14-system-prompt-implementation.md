# System Prompt Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the layered system prompt assembly pipeline: `SystemPrompt` type with `render()`, environment info injection, instruction file loading, `AgentRole.prompt` extension, and `CompletionRequest.preamble` injection.

**Architecture:** A new `kernel::prompt` submodule holds the `SystemPrompt` struct. `TurnContext` gains `agent_prompt` + `user_system_prompt` fields. `execute_turn()` constructs a `SystemPrompt`, calls `render()`, and sets the result as `CompletionRequest.preamble`. The preamble is converted to `Message::System` at `build()` time by the existing provider infrastructure — no changes needed in the provider crate.

**Tech Stack:** Rust (edition 2024), tokio, typed-builder, chrono

---

### Task 1: EnvironmentInfo — 环境信息采集

**Files:**
- Create: `crates/kernel/src/prompt/mod.rs`
- Create: `crates/kernel/src/prompt/environment.rs`
- Modify: `crates/kernel/src/lib.rs`

- [ ] **Step 1: 创建 prompt 子模块根文件**

Write `crates/kernel/src/prompt/mod.rs`:

```rust
//! System prompt assembly pipeline.
//!
//! Constructs a layered system prompt string from agent-specific,
//! environment, instruction-file, skill, and user-provided sources.
//! The rendered result is injected into each LLM request via
//! [`CompletionRequest::preamble`].

pub(crate) mod environment;
pub(crate) mod instruction;

use environment::EnvironmentInfo;

/// Default system prompt used when no agent-specific prompt is configured.
pub(crate) const DEFAULT_SYSTEM_PROMPT: &str = "\
You are an interactive AI agent powered by clawcode. \
You help users with software engineering tasks by understanding \
their codebase, answering questions, and executing tools to \
read, write, and modify files.

Respond concisely and directly. Use tools when they are the best \
way to fulfill the user's request. Read files before editing them. \
Do not create files that the user did not ask for.";

/// Layered system prompt whose [`render`](SystemPrompt::render) method
/// produces the final string injected as the LLM request preamble.
///
/// Assembly order (matches the TypeScript OpenCode spec):
///   ① agent_prompt (replaces default when set)
///   ② environment block
///   ② instructions (AGENTS.md + .agents/*.md)
///   ② skills XML (only when agent has skill permission)
///   ③ user_prompt (lowest priority, temporary injection)
#[derive(Clone, Debug, typed_builder::TypedBuilder)]
pub(crate) struct SystemPrompt {
    /// ① Agent-specific prompt. When `Some`, completely replaces the
    ///    default system prompt. When `None`, `DEFAULT_SYSTEM_PROMPT` is used.
    #[builder(default, setter(strip_option))]
    pub agent_prompt: Option<String>,
    /// ②a Runtime environment snapshot.
    pub environment: EnvironmentInfo,
    /// ②b Formatted instruction file contents (AGENTS.md + .agents/*.md).
    #[builder(default, setter(strip_option))]
    pub instructions: Option<String>,
    /// ②c Skill registry XML block. Only filled when the agent has skill
    ///     permission the registry is populated.
    #[builder(default, setter(strip_option))]
    pub skills_xml: Option<String>,
    /// ③ Temporary user-provided system prompt. Lowest priority.
    #[builder(default, setter(strip_option))]
    pub user_prompt: Option<String>,
}

impl SystemPrompt {
    /// Render the complete system prompt string.
    ///
    /// Joins all non-empty layers in priority order with `\n`.
    pub fn render(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        // ① Agent prompt or default
        parts.push(
            self.agent_prompt
                .clone()
                .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string()),
        );

        // ②a Environment
        parts.push(self.environment.render_block());

        // ②b Instructions
        if let Some(ref ins) = self.instructions {
            parts.push(ins.clone());
        }

        // ②c Skills
        if let Some(ref skills) = self.skills_xml {
            parts.push(skills.clone());
        }

        // ③ User-provided
        if let Some(ref user) = self.user_prompt {
            parts.push(user.clone());
        }

        parts.into_iter().join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_env() -> EnvironmentInfo {
        EnvironmentInfo::builder()
            .model_id("test-model".to_string())
            .cwd(std::path::PathBuf::from("/tmp/test"))
            .is_git_repo(true)
            .platform("linux".to_string())
            .date("2026-01-01".to_string())
            .build()
    }

    #[test]
    fn render_with_default_prompt_includes_environment() {
        let sp = SystemPrompt::builder()
            .environment(test_env())
            .build();
        let result = sp.render();
        assert!(result.contains(DEFAULT_SYSTEM_PROMPT));
        assert!(result.contains("test-model"));
        assert!(result.contains("/tmp/test"));
    }

    #[test]
    fn agent_prompt_replaces_default() {
        let sp = SystemPrompt::builder()
            .environment(test_env())
            .agent_prompt("Custom agent prompt".to_string())
            .build();
        let result = sp.render();
        assert!(result.contains("Custom agent prompt"));
        assert!(!result.contains(DEFAULT_SYSTEM_PROMPT));
    }

    #[test]
    fn empty_layers_are_skipped() {
        let sp = SystemPrompt::builder()
            .environment(test_env())
            .build();
        let result = sp.render();
        // Should not contain empty lines for skipped layers
        assert!(!result.contains("\n\n\n"));
    }

    #[test]
    fn user_prompt_appended_last() {
        let sp = SystemPrompt::builder()
            .environment(test_env())
            .user_prompt("Extra instruction".to_string())
            .build();
        let result = sp.render();
        let user_pos = result.find("Extra instruction").unwrap();
        let env_pos = result.find("test-model").unwrap();
        assert!(user_pos > env_pos, "user prompt must appear after environment");
    }
}
```

- [ ] **Step 2: 创建 environment.rs**

Write `crates/kernel/src/prompt/environment.rs`:

```rust
//! Environment information captured once per turn and injected
//! into the system prompt for every LLM request.

use std::path::PathBuf;

/// Snapshot of the runtime environment injected into each LLM request.
#[derive(Clone, Debug, typed_builder::TypedBuilder)]
pub(crate) struct EnvironmentInfo {
    /// Model identifier (e.g. "deepseek-v4-pro").
    pub model_id: String,
    /// Absolute working directory path.
    pub cwd: PathBuf,
    /// Whether the working directory is inside a git repository.
    pub is_git_repo: bool,
    /// Operating system platform: "darwin" | "linux" | "win32".
    pub platform: String,
    /// Current date in "YYYY-MM-DD" format.
    pub date: String,
}

impl EnvironmentInfo {
    /// Capture environment info from the live system.
    ///
    /// Never fails — even if git detection fails, `is_git_repo` defaults
    /// to `false` rather than propagating an error.
    pub fn capture(model_id: String, cwd: PathBuf) -> Self {
        Self {
            is_git_repo: detect_git(&cwd),
            platform: std::env::consts::OS.to_string(),
            date: chrono::Local::now().format("%Y-%m-%d").to_string(),
            model_id,
            cwd,
        }
    }

    /// Format the environment info as a text block for the system prompt.
    pub(crate) fn render_block(&self) -> String {
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

/// Check whether `dir` is inside a git repository by running `git rev-parse --git-dir`.
fn detect_git(cwd: &std::path::Path) -> bool {
    std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_block_contains_all_fields() {
        let info = EnvironmentInfo {
            model_id: "deepseek-v4".to_string(),
            cwd: PathBuf::from("/home/user/project"),
            is_git_repo: true,
            platform: "linux".to_string(),
            date: "2026-05-14".to_string(),
        };
        let block = info.render_block();
        assert!(block.contains("deepseek-v4"));
        assert!(block.contains("/home/user/project"));
        assert!(block.contains("yes"));
        assert!(block.contains("linux"));
        assert!(block.contains("2026-05-14"));
    }

    #[test]
    fn capture_does_not_panic() {
        let info = EnvironmentInfo::capture(
            "test-model".to_string(),
            std::env::current_dir().unwrap(),
        );
        assert!(!info.model_id.is_empty());
        assert!(!info.platform.is_empty());
        assert!(!info.date.is_empty());
    }
}
```

- [ ] **Step 3: 创建 instruction.rs (stub)**

Write `crates/kernel/src/prompt/instruction.rs`:

```rust
//! Instruction file loading: AGENTS.md and .agents/*.md.
//!
//! P1: loads AGENTS.md only.
//! Future: .agents/ directory scanning with content-hash deduplication.

use std::path::{Path, PathBuf};

/// Load instruction files and return formatted text.
///
/// Returns `None` when no instruction files are found.
///
/// ## Loading strategy
/// 1. Walk up from `cwd` to find the first `AGENTS.md`
/// 2. (Future) Load all `.md` files from `cwd/.agents/`
/// 3. (Future) Deduplicate by (filename, content hash)
/// 4. Format as `Instructions from: <path>\n<content>`
pub(crate) fn load_instructions(cwd: &Path) -> Option<String> {
    let agents_md = find_agents_md(cwd)?;
    let content = std::fs::read_to_string(&agents_md).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(format!(
        "Instructions from: {}\n{}",
        agents_md.display(),
        trimmed
    ))
}

/// Walk up from `start` to find the first `AGENTS.md`.
fn find_agents_md(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };

    loop {
        let candidate = current.join("AGENTS.md");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn finds_agents_md_in_working_dir() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("AGENTS.md");
        std::fs::write(&md, "# Test instructions").unwrap();

        let result = load_instructions(dir.path());
        assert!(result.is_some());
        assert!(result.unwrap().contains("# Test instructions"));
    }

    #[test]
    fn finds_agents_md_in_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("AGENTS.md");
        std::fs::write(&md, "# Parent instructions").unwrap();

        let child = dir.path().join("sub").join("deep");
        std::fs::create_dir_all(&child).unwrap();

        let result = load_instructions(&child);
        assert!(result.is_some());
        assert!(result.unwrap().contains("# Parent instructions"));
    }

    #[test]
    fn returns_none_when_no_agents_md() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_instructions(dir.path());
        assert!(result.is_none());
    }
}
```

- [ ] **Step 4: 检查 chrono 是否可用**

```bash
grep 'chrono' /Users/isbset/Documents/clawcode/Cargo.toml
```

If not present, add to workspace `Cargo.toml`:
```toml
chrono = "0.4"
```

And add to `crates/kernel/Cargo.toml`:
```toml
chrono = { workspace = true }
tempfile = { workspace = true }
```

- [ ] **Step 5: 注册模块**

Edit `crates/kernel/src/lib.rs`, add:
```rust
pub(crate) mod prompt;
```

- [ ] **Step 6: 编译验证**

```bash
cargo build -p kernel 2>&1 | tail -10
```

- [ ] **Step 7: 运行测试**

```bash
cargo test -p kernel -- prompt 2>&1 | tail -20
```

- [ ] **Step 8: Commit**

```bash
git add crates/kernel/src/prompt/ crates/kernel/src/lib.rs crates/Cargo.toml
git commit -m "feat(prompt): add SystemPrompt type with EnvironmentInfo and instruction loading"
```

---

### Task 2: TurnContext 扩展 + execute_turn 注入

**Files:**
- Modify: `crates/kernel/src/turn.rs`
- Modify: `crates/kernel/src/session.rs`

- [ ] **Step 1: TurnContext 增加 prompt 相关字段**

Edit `crates/kernel/src/turn.rs`, add to `TurnContext` struct:

```rust
    /// ① Agent-specific system prompt. `None` means use the default.
    #[builder(default, setter(strip_option))]
    pub(crate) agent_prompt: Option<String>,
    /// ③ Temporary user-provided system prompt.
    #[builder(default, setter(strip_option))]
    pub(crate) user_system_prompt: Option<String>,
```

Add import at top of file:
```rust
use crate::prompt::{SystemPrompt, environment::EnvironmentInfo, instruction};
```

- [ ] **Step 2: 在 execute_turn 中构建并注入 preamble**

In `execute_turn()`, after `context.push(Message::user(user_text))` and the `let tool_defs = ...` line, add:

```rust
    // ── Build and inject the system prompt preamble ──
    let env_info = EnvironmentInfo::capture(
        ctx.llm.model_id().to_string(),
        ctx.cwd.clone(),
    );
    let instructions = instruction::load_instructions(&ctx.cwd);

    let system_prompt = SystemPrompt::builder()
        .environment(env_info)
        .agent_prompt(ctx.agent_prompt.clone())
        .instructions(instructions)
        .user_prompt(ctx.user_system_prompt.clone())
        .build();

    let preamble = system_prompt.render();
    // ────────────────────────────────────────
```

Then in the loop, modify the `CompletionRequest::builder()` call to include `.preamble(preamble.clone())`:

```rust
        let request = CompletionRequest::builder()
            .model(Some(ctx.llm.model_id().to_string()))
            .preamble(preamble.clone())
            .chat_history(history)
            .tools(tool_defs.clone())
            .build();
```

- [ ] **Step 3: session.rs 传递字段**

Edit `crates/kernel/src/session.rs`:

In `run_loop()`, add to the `TurnContext::builder()` call for the `Op::InterAgentMessage` and `Op::Prompt` branches:

```rust
    .agent_prompt(None)
    .user_system_prompt(None)
```

Note: P1 passes `None` for both. The `agent_prompt` will be wired from `AgentRole` in Task 3. The `user_system_prompt` will be wired from `Op::Prompt` in Task 4.

- [ ] **Step 4: 编译验证**

```bash
cargo build -p kernel 2>&1 | tail -10
```

- [ ] **Step 5: 确保已有测试通过**

```bash
cargo test -p kernel 2>&1 | tail -20
```

- [ ] **Step 6: Commit**

```bash
git add crates/kernel/src/turn.rs crates/kernel/src/session.rs
git commit -m "feat(prompt): inject system prompt via CompletionRequest preamble in execute_turn"
```

---

### Task 3: AgentRole.prompt 扩展

**Files:**
- Modify: `crates/kernel/src/agent/role.rs`
- Create: `crates/kernel/src/prompts/explorer.txt`

- [ ] **Step 1: AgentRole 增加 prompt 字段**

Edit `crates/kernel/src/agent/role.rs`, add to `AgentRole` struct:

```rust
    /// Agent-specific system prompt. When `Some`, completely replaces
    /// the default system prompt. When `None`, the default is used.
    #[builder(default, setter(strip_option))]
    pub prompt: Option<String>,
```

Update `AgentRoleSet::with_builtins()` to set the explorer prompt:

```rust
    set.insert(AgentRole {
        name: "explorer".to_string(),
        description: "Lightweight agent for fast codebase exploration".to_string(),
        nickname_candidates: vec![],
        config_overrides: {
            let mut m = HashMap::new();
            m.insert("reasoning_effort".to_string(), "low".to_string());
            m
        },
        prompt: Some(include_str!("../prompts/explorer.txt").to_string()),
    });
```

- [ ] **Step 2: 创建 explorer.txt**

Write `crates/kernel/src/prompts/explorer.txt`:

```text
You are a file search specialist. You excel at thoroughly navigating
and exploring codebases.

- Use Glob for broad file pattern matching
- Use Grep for searching file contents with regex
- Use Read when you know the specific file path
- Use Bash for file operations like copying, moving, or listing
  directory contents
- Adapt your search approach based on the thoroughness level specified
  by the caller
- Do not create any files, or run bash commands that modify the
  user's system state
```

- [ ] **Step 3: 验证 role 测试**

```bash
cargo test -p kernel -- agent::role 2>&1 | tail -10
```

- [ ] **Step 4: Commit**

```bash
git add crates/kernel/src/agent/role.rs crates/kernel/src/prompts/
git commit -m "feat(role): add prompt field to AgentRole with explorer-specific system prompt"
```

---

### Task 4: Op::Prompt 扩展 + 端到端串联

**Files:**
- Modify: `crates/protocol/src/op.rs`
- Modify: `crates/kernel/src/session.rs`
- Modify: `crates/kernel/src/agent/control.rs`

- [ ] **Step 1: Op::Prompt 增加 system 字段**

Edit `crates/protocol/src/op.rs`, update the `Op::Prompt` variant:

```rust
    /// A user prompt to process in the session.
    Prompt {
        /// User-provided text to process.
        text: String,
        /// Optional ad-hoc system prompt appended at lowest priority.
        /// Does NOT override agent or environment prompt layers.
        #[serde(default)]
        system: Option<String>,
    },
```

- [ ] **Step 2: session.rs 传递 user_system_prompt**

Edit `crates/kernel/src/session.rs`, in the `Op::Prompt` match arm:

```rust
            Some(Op::Prompt { text, system, .. }) => {
                let ctx = TurnContext::builder()
                    .session_id(rt.session_id.clone())
                    .llm(Arc::clone(&rt.llm))
                    .tools(Arc::clone(&rt.tools))
                    .cwd(rt.cwd.clone())
                    .pending_approvals(Arc::clone(&rt.pending_approvals))
                    .agent_path(rt.agent_path.clone())
                    .approval(Arc::clone(&rt.approval))
                    .user_system_prompt(system)
                    .build();
                // ... rest unchanged ...
```

- [ ] **Step 3: Sub-agent spawn 传递 agent_prompt**

In `crates/kernel/src/agent/control.rs`, when `spawn()` creates a sub-agent, look up the role's prompt and carry it through to the turn context. This will be fully wired in a follow-up task once sub-agent turn execution supports prompt injection.

For P1: add a `// TODO:` comment marking where role prompt will be passed.

- [ ] **Step 4: 全仓编译验证**

```bash
cargo build 2>&1 | tail -15
```

- [ ] **Step 5: 全仓测试**

```bash
cargo test 2>&1 | tail -20
```

- [ ] **Step 6: 手动端到端验证**

Start the claw client and check that the LLM receives system prompt content. Use tracing/logging to verify the preamble appears in the request:

```bash
# Add temporary tracing to print preamble, then:
cargo run -p acp --bin claw 2>&1 | head -50
```

Verify that the output shows the agent responding with awareness of:
- Its model name
- The working directory
- The git repository status
- Today's date

- [ ] **Step 7: Commit**

```bash
git add crates/protocol/src/op.rs crates/kernel/src/session.rs crates/kernel/src/agent/control.rs
git commit -m "feat(prompt): wire user system prompt through Op::Prompt and session pipeline"
```

---

### Task 5: 默认 System Prompt 优化 + 文档

**Files:**
- Modify: `crates/kernel/src/prompt/mod.rs`

- [ ] **Step 1: Review 并优化 DEFAULT_SYSTEM_PROMPT**

Review the default system prompt content against the project's actual tool set and behavior. Ensure it accurately reflects clawcode's capabilities and constraints (yolo approval mode in dev, file tools, etc.).

- [ ] **Step 2: Commit**

```bash
git add crates/kernel/src/prompt/mod.rs
git commit -m "docs(prompt): refine default system prompt for clawcode capabilities"
```

---

### Dependency Order

```
Task 1 (prompt 子模块 + EnvironmentInfo + instruction stub)
  └─ Task 2 (TurnContext 扩展 + execute_turn 注入)
       ├─ Task 3 (AgentRole.prompt)
       └─ Task 4 (Op::Prompt + 端到端串联)
            └─ Task 5 (默认 prompt 优化)
```

Tasks 3 and 4 can run in parallel after Task 2 completes.

---

## P1 范围外（后续阶段）

| 功能 | 说明 |
|---|---|
| `.agents/` 目录扫描 | 仅加载 AGENTS.md，`.agents/*.md` 后续 |
| 指令文件去重 | 内容哈希去重后续实现 |
| Skills XML 注入 | 等 skill 系统就绪后接入 |
| 合成提醒 | Plan mode / build-switch / max-steps 提醒后续 |
| Sub-agent prompt 传递 | `AgentRole.prompt` → sub-agent `TurnContext` 的完整链路 |
| CLAUDE.md 支持 | 目前仅加载 AGENTS.md，CLAUDE.md 按 spec 不加载 |
