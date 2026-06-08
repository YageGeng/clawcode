# Hook Mechanism Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 clawcode 中实现用户可配置的 command hook 机制，覆盖工具执行、会话启动、用户输入、压缩、停止和审批生命周期。

**Architecture:** 配置 crate 只提供可反序列化的 hook 配置类型；新增 `hook` crate 负责 discovery、matcher、子进程执行、输出解析和事件 outcome 聚合；kernel 只在现有 `run_loop()`、`execute_turn()`、`dispatch_tool()`、`resolve_tool_approval()` 和 compaction 路径调用 `HookEngine`。协议层新增 hook 运行事件类型，供 TUI/ACP 后续展示，但一期 UI 不需要专门交互。

**Tech Stack:** Rust 2024, tokio, futures, serde/serde_json, regex, tempfile, typed-builder, existing kernel/tool/context/session recorder abstractions.

---

## 文件结构

- Create `crates/config/src/hook.rs`：定义 `HooksFile`、`HookEventsToml`、`MatcherGroup`、`HookHandlerConfig`。
- Modify `crates/config/src/lib.rs`：导出 hook 配置类型。
- Modify `crates/config/Cargo.toml`：确认 `serde_json` 测试依赖可用。
- Create `crates/hook/Cargo.toml`：新增 hook crate。
- Create `crates/hook/src/lib.rs`：导出 `HookEngine`、request/outcome 类型、通用常量。
- Create `crates/hook/src/engine/mod.rs`：`HookEngine`、`ConfiguredHandler`、`HookConfig`、preview/run 分发入口。
- Create `crates/hook/src/engine/discovery.rs`：读取 `.claw/hooks.json`、`~/.claw/hooks.json`，规范化 handler。
- Create `crates/hook/src/engine/dispatcher.rs`：matcher 选择、alias 去重、并发执行。
- Create `crates/hook/src/engine/command_runner.rs`：通过 `$SHELL -lc` 执行 command hook。
- Create `crates/hook/src/engine/output_parser.rs`：解析 stdout JSON 和 exit code 语义。
- Create `crates/hook/src/events/common.rs`：共享 request 字段、matcher 规则、additional context 聚合。
- Create `crates/hook/src/events/*.rs`：各 hook 的 request/outcome/run/preview。
- Modify `crates/protocol/src/hook.rs`：新增 hook event name、run summary、output entry、trust/status 枚举。
- Modify `crates/protocol/src/event.rs`：新增 `HookCompleted` 事件。
- Modify `crates/protocol/src/lib.rs`：导出 hook 协议类型。
- Modify `crates/kernel/Cargo.toml`、workspace `Cargo.toml`：加入 `hook` crate 依赖。
- Modify `crates/kernel/src/session.rs`：构建并持有 `Arc<HookEngine>`，接入 SessionStart。
- Modify `crates/kernel/src/turn.rs`：接入 UserPromptSubmit、PreToolUse、PostToolUse、Stop、PermissionRequest。
- Modify `crates/kernel/src/compaction.rs` 和 `crates/kernel/src/context.rs`：传入 hook engine 并接入 PreCompact/PostCompact。
- Modify tests under `crates/config`, `crates/hook`, `crates/kernel`：覆盖配置、engine 和 kernel 集成。

## Task 1: 配置类型与 JSON 解析

**Files:**
- Create: `crates/config/src/hook.rs`
- Modify: `crates/config/src/lib.rs`
- Test: `crates/config/src/hook.rs`

- [ ] **Step 1: 写失败测试**

在新文件 `crates/config/src/hook.rs` 中先写测试模块，验证 tagged command handler、unsupported handler、Windows 字段和默认 hooks 都能解析。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Parses command handlers with Codex-compatible tagged shape.
    #[test]
    fn parses_command_hook_config() {
        let parsed: HooksFile = serde_json::from_str(
            r#"{
              "hooks": {
                "PreToolUse": [{
                  "matcher": "shell",
                  "hooks": [{
                    "type": "command",
                    "command": "python3 .claw/hooks/pre_shell.py",
                    "commandWindows": "py .claw\\hooks\\pre_shell.py",
                    "timeout": 60,
                    "async": false,
                    "statusMessage": "checking shell"
                  }]
                }]
              }
            }"#,
        )
        .expect("parse hooks file");

        assert_eq!(parsed.hooks.pre_tool_use.len(), 1);
        let group = &parsed.hooks.pre_tool_use[0];
        assert_eq!(group.matcher.as_deref(), Some("shell"));
        assert_eq!(group.hooks.len(), 1);
        let HookHandlerConfig::Command {
            command,
            command_windows,
            timeout_sec,
            r#async,
            status_message,
        } = &group.hooks[0]
        else {
            panic!("expected command handler");
        };
        assert_eq!(command, "python3 .claw/hooks/pre_shell.py");
        assert_eq!(command_windows.as_deref(), Some("py .claw\\hooks\\pre_shell.py"));
        assert_eq!(*timeout_sec, Some(60));
        assert!(!*r#async);
        assert_eq!(status_message.as_deref(), Some("checking shell"));
    }

    /// Parses prompt and agent variants so discovery can warn and skip them.
    #[test]
    fn parses_unsupported_handler_variants() {
        let parsed: HooksFile = serde_json::from_str(
            r#"{
              "hooks": {
                "SessionStart": [{
                  "hooks": [{ "type": "prompt" }, { "type": "agent" }]
                }]
              }
            }"#,
        )
        .expect("parse hooks file");

        assert!(matches!(
            parsed.hooks.session_start[0].hooks[0],
            HookHandlerConfig::Prompt {}
        ));
        assert!(matches!(
            parsed.hooks.session_start[0].hooks[1],
            HookHandlerConfig::Agent {}
        ));
    }
}
```

- [ ] **Step 2: 运行失败测试**

Run: `rtk cargo test -p config hook::tests::parses_command_hook_config hook::tests::parses_unsupported_handler_variants`

Expected: FAIL，因为 `hook` module 和类型还不存在。

- [ ] **Step 3: 添加配置类型**

在 `crates/config/src/hook.rs` 写入配置类型。所有新增类型上方使用英文 doc comment。

```rust
//! Hook configuration types loaded from hooks.json files.

use serde::Deserialize;
use serde::Serialize;

/// Top-level hooks.json document.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct HooksFile {
    /// Hook event groups keyed by lifecycle event name.
    #[serde(default)]
    pub hooks: HookEventsToml,
}

/// Configured hook matcher groups for every supported event.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct HookEventsToml {
    #[builder(default)]
    #[serde(rename = "PreToolUse", default)]
    pub pre_tool_use: Vec<MatcherGroup>,
    #[builder(default)]
    #[serde(rename = "PermissionRequest", default)]
    pub permission_request: Vec<MatcherGroup>,
    #[builder(default)]
    #[serde(rename = "PostToolUse", default)]
    pub post_tool_use: Vec<MatcherGroup>,
    #[builder(default)]
    #[serde(rename = "PreCompact", default)]
    pub pre_compact: Vec<MatcherGroup>,
    #[builder(default)]
    #[serde(rename = "PostCompact", default)]
    pub post_compact: Vec<MatcherGroup>,
    #[builder(default)]
    #[serde(rename = "SessionStart", default)]
    pub session_start: Vec<MatcherGroup>,
    #[builder(default)]
    #[serde(rename = "UserPromptSubmit", default)]
    pub user_prompt_submit: Vec<MatcherGroup>,
    #[builder(default)]
    #[serde(rename = "SubagentStart", default)]
    pub subagent_start: Vec<MatcherGroup>,
    #[builder(default)]
    #[serde(rename = "SubagentStop", default)]
    pub subagent_stop: Vec<MatcherGroup>,
    #[builder(default)]
    #[serde(rename = "Stop", default)]
    pub stop: Vec<MatcherGroup>,
}

impl HookEventsToml {
    /// Returns true when no event contains hook handlers.
    pub fn is_empty(&self) -> bool {
        self.pre_tool_use.is_empty()
            && self.permission_request.is_empty()
            && self.post_tool_use.is_empty()
            && self.pre_compact.is_empty()
            && self.post_compact.is_empty()
            && self.session_start.is_empty()
            && self.user_prompt_submit.is_empty()
            && self.subagent_start.is_empty()
            && self.subagent_stop.is_empty()
            && self.stop.is_empty()
    }
}

/// One matcher and the handlers it selects.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatcherGroup {
    /// Regex or exact matcher. None means all matcher inputs for that event.
    #[serde(default)]
    pub matcher: Option<String>,
    /// Handlers run when the matcher selects the event.
    #[serde(default)]
    pub hooks: Vec<HookHandlerConfig>,
}

/// Supported hook handler declarations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HookHandlerConfig {
    /// Command handler executed as a child process.
    #[serde(rename = "command")]
    Command {
        command: String,
        #[serde(default, rename = "commandWindows", alias = "command_windows")]
        command_windows: Option<String>,
        #[serde(default, rename = "timeout")]
        timeout_sec: Option<u64>,
        #[serde(default)]
        r#async: bool,
        #[serde(default, rename = "statusMessage")]
        status_message: Option<String>,
    },
    /// Placeholder for compatibility; discovery warns and skips it.
    #[serde(rename = "prompt")]
    Prompt {},
    /// Placeholder for compatibility; discovery warns and skips it.
    #[serde(rename = "agent")]
    Agent {},
}
```

- [ ] **Step 4: 导出配置类型**

修改 `crates/config/src/lib.rs`：

```rust
pub mod hook;

pub use hook::{
    HookEventsToml, HookHandlerConfig, HooksFile, MatcherGroup,
};
```

保留现有 module/export，不删除任何已有 export。

- [ ] **Step 5: 运行配置测试**

Run: `rtk cargo test -p config hook::tests`

Expected: PASS。

- [ ] **Step 6: 检查点**

Run: `rtk git diff -- crates/config/src/hook.rs crates/config/src/lib.rs`

Expected: diff 只包含 hook 配置类型与导出。不要运行 `git commit`，除非用户明确授权。

## Task 2: 协议层 hook 事件类型

**Files:**
- Create: `crates/protocol/src/hook.rs`
- Modify: `crates/protocol/src/event.rs`
- Modify: `crates/protocol/src/lib.rs`
- Test: `crates/protocol/src/hook.rs`

- [ ] **Step 1: 写序列化测试**

创建 `crates/protocol/src/hook.rs` 并先写测试，固定 wire shape。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Serializes hook completion events with stable snake_case event wrapper.
    #[test]
    fn hook_completed_event_serializes() {
        let event = HookCompletedEvent {
            turn_id: Some(crate::TurnId("turn-1".to_string())),
            run: HookRunSummary {
                id: "pre-tool-use:0:/repo/.claw/hooks.json:call-1".to_string(),
                event_name: HookEventName::PreToolUse,
                handler_type: HookHandlerType::Command,
                execution_mode: HookExecutionMode::Sync,
                scope: HookScope::Turn,
                source_path: "/repo/.claw/hooks.json".into(),
                source: HookSource::Project,
                display_order: 0,
                status: HookRunStatus::Blocked,
                status_message: Some("checking shell".to_string()),
                started_at: 1,
                completed_at: Some(2),
                duration_ms: Some(1000),
                entries: vec![HookOutputEntry {
                    kind: HookOutputEntryKind::Feedback,
                    text: "blocked".to_string(),
                }],
            },
        };

        assert_eq!(
            serde_json::to_value(event).expect("serialize hook event"),
            json!({
                "turn_id": "turn-1",
                "run": {
                    "id": "pre-tool-use:0:/repo/.claw/hooks.json:call-1",
                    "event_name": "pre_tool_use",
                    "handler_type": "command",
                    "execution_mode": "sync",
                    "scope": "turn",
                    "source_path": "/repo/.claw/hooks.json",
                    "source": "project",
                    "display_order": 0,
                    "status": "blocked",
                    "status_message": "checking shell",
                    "started_at": 1,
                    "completed_at": 2,
                    "duration_ms": 1000,
                    "entries": [{ "kind": "feedback", "text": "blocked" }]
                }
            })
        );
    }
}
```

- [ ] **Step 2: 运行失败测试**

Run: `rtk cargo test -p protocol hook::tests::hook_completed_event_serializes`

Expected: FAIL，因为类型未定义。

- [ ] **Step 3: 定义协议类型**

在 `crates/protocol/src/hook.rs` 添加：

```rust
//! Protocol types for hook execution previews and completion events.

use serde::Deserialize;
use serde::Serialize;

/// Supported lifecycle hook event names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEventName {
    PreToolUse,
    PermissionRequest,
    PostToolUse,
    PreCompact,
    PostCompact,
    SessionStart,
    UserPromptSubmit,
    SubagentStart,
    SubagentStop,
    Stop,
}

/// Hook handler implementation category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookHandlerType {
    Command,
}

/// Hook execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookExecutionMode {
    Sync,
}

/// Hook run lifecycle scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookScope {
    Thread,
    Turn,
}

/// Source layer that declared a hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookSource {
    User,
    Project,
}

/// Status of one hook command run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookRunStatus {
    Running,
    Completed,
    Failed,
    Blocked,
    Stopped,
}

/// One visible entry produced by a hook run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookOutputEntry {
    pub kind: HookOutputEntryKind,
    pub text: String,
}

/// Kind of hook output entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookOutputEntryKind {
    Context,
    Error,
    Feedback,
    Stop,
    Warning,
}

/// Stable summary for a hook run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct HookRunSummary {
    pub id: String,
    pub event_name: HookEventName,
    pub handler_type: HookHandlerType,
    pub execution_mode: HookExecutionMode,
    pub scope: HookScope,
    pub source_path: String,
    pub source: HookSource,
    pub display_order: i64,
    pub status: HookRunStatus,
    #[builder(default, setter(strip_option))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    pub started_at: i64,
    #[builder(default, setter(strip_option))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[builder(default, setter(strip_option))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[builder(default)]
    #[serde(default)]
    pub entries: Vec<HookOutputEntry>,
}

/// Completed hook run emitted by the kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct HookCompletedEvent {
    #[builder(default, setter(strip_option))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<crate::TurnId>,
    pub run: HookRunSummary,
}
```

- [ ] **Step 4: 导出并接入 Event**

在 `crates/protocol/src/lib.rs` 添加：

```rust
pub mod hook;
pub use hook::*;
```

在 `crates/protocol/src/event.rs` 的 `Event` enum 增加：

```rust
/// A lifecycle hook completed execution.
HookCompleted {
    session_id: SessionId,
    completed: crate::HookCompletedEvent,
},
```

并添加构造函数：

```rust
/// Create a hook completion event.
#[inline(always)]
pub fn hook_completed(
    session_id: impl Into<SessionId>,
    completed: crate::HookCompletedEvent,
) -> Self {
    Event::HookCompleted {
        session_id: session_id.into(),
        completed,
    }
}
```

- [ ] **Step 5: 运行协议测试**

Run: `rtk cargo test -p protocol hook::tests::hook_completed_event_serializes`

Expected: PASS。

## Task 3: hook crate 骨架、discovery 和 matcher

**Files:**
- Modify: root `Cargo.toml`
- Create: `crates/hook/Cargo.toml`
- Create: `crates/hook/src/lib.rs`
- Create: `crates/hook/src/engine/mod.rs`
- Create: `crates/hook/src/engine/discovery.rs`
- Create: `crates/hook/src/events/common.rs`
- Test: `crates/hook/src/engine/discovery.rs`
- Test: `crates/hook/src/events/common.rs`

- [ ] **Step 1: 创建 crate manifest**

新增 `crates/hook/Cargo.toml`：

```toml
[package]
name = "hook"
version = "0.1.0"
edition = "2024"

[dependencies]
chrono = { workspace = true }
config = { path = "../config" }
futures = { workspace = true }
protocol = { path = "../protocol" }
regex = { workspace = true }
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true, features = ["process", "time", "io-util", "macros"] }
typed-builder = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
pretty_assertions = { workspace = true }
```

在 workspace root `Cargo.toml` 的 members 添加 `"crates/hook"`。

- [ ] **Step 2: 写 matcher 失败测试**

在 `crates/hook/src/events/common.rs` 中先写测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Exact matcher only matches complete alternatives.
    #[test]
    fn exact_matcher_uses_complete_alternatives() {
        assert!(matches_matcher(Some("shell|apply_patch"), Some("shell")));
        assert!(matches_matcher(Some("shell|apply_patch"), Some("apply_patch")));
        assert!(!matches_matcher(Some("shell"), Some("shell_extra")));
    }

    /// Invalid regex is rejected during discovery validation.
    #[test]
    fn invalid_regex_is_rejected() {
        assert!(validate_matcher_pattern("[").is_err());
        assert!(validate_matcher_pattern("*").is_ok());
        assert!(validate_matcher_pattern("").is_ok());
    }
}
```

- [ ] **Step 3: 实现 matcher helpers**

在 `crates/hook/src/events/common.rs` 添加：

```rust
//! Shared helpers for hook event selection and output aggregation.

/// Validate a matcher pattern before it can enter the runtime registry.
pub(crate) fn validate_matcher_pattern(matcher: &str) -> Result<(), regex::Error> {
    if is_match_all_matcher(matcher) || is_exact_matcher(matcher) {
        return Ok(());
    }
    regex::Regex::new(matcher).map(|_| ())
}

/// Return whether a matcher selects the provided input.
pub(crate) fn matches_matcher(matcher: Option<&str>, input: Option<&str>) -> bool {
    match matcher {
        None => true,
        Some(matcher) if is_match_all_matcher(matcher) => true,
        Some(matcher) if is_exact_matcher(matcher) => input
            .map(|input| matcher.split('|').any(|candidate| candidate == input))
            .unwrap_or(false),
        Some(matcher) => input
            .and_then(|input| {
                regex::Regex::new(matcher)
                    .ok()
                    .map(|regex| regex.is_match(input))
            })
            .unwrap_or(false),
    }
}

/// Build matcher inputs with the canonical name first.
pub(crate) fn matcher_inputs<'a>(
    canonical: &'a str,
    aliases: &'a [String],
) -> Vec<&'a str> {
    std::iter::once(canonical)
        .chain(aliases.iter().map(String::as_str))
        .collect()
}

fn is_match_all_matcher(matcher: &str) -> bool {
    matcher.is_empty() || matcher == "*"
}

fn is_exact_matcher(matcher: &str) -> bool {
    matcher
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '|')
}
```

- [ ] **Step 4: 写 discovery 测试**

在 `crates/hook/src/engine/discovery.rs` 写测试，固定“项目层先、用户层后、无效 matcher skip、async skip”。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Discovery appends project hooks before user hooks and skips invalid groups.
    #[test]
    fn discovers_hooks_in_precedence_order_and_skips_invalid_matcher() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let project = tmp.path().join("repo");
        let user = tmp.path().join("home");
        fs::create_dir_all(project.join(".claw")).expect("project hooks dir");
        fs::create_dir_all(user.join(".claw")).expect("user hooks dir");

        fs::write(
            project.join(".claw/hooks.json"),
            r#"{
              "hooks": {
                "PreToolUse": [
                  { "matcher": "[", "hooks": [{ "type": "command", "command": "bad" }] },
                  { "matcher": "shell", "hooks": [{ "type": "command", "command": "project" }] }
                ]
              }
            }"#,
        )
        .expect("write project hooks");
        fs::write(
            user.join(".claw/hooks.json"),
            r#"{
              "hooks": {
                "PreToolUse": [
                  { "matcher": "shell", "hooks": [{ "type": "command", "command": "user", "async": true }] },
                  { "matcher": "shell", "hooks": [{ "type": "command", "command": "user-sync" }] }
                ]
              }
            }"#,
        )
        .expect("write user hooks");

        let result = discover_handlers(DiscoveryConfig {
            project_cwd: project.clone(),
            user_home: Some(user.clone()),
        });

        assert_eq!(
            result.handlers.iter().map(|handler| handler.command.as_str()).collect::<Vec<_>>(),
            vec!["project", "user-sync"]
        );
        assert!(result.warnings.iter().any(|warning| warning.contains("invalid matcher")));
        assert!(result.warnings.iter().any(|warning| warning.contains("async hooks are not supported")));
    }
}
```

- [ ] **Step 5: 实现 engine 基础类型和 discovery**

`crates/hook/src/engine/mod.rs`：

```rust
pub(crate) mod discovery;
pub(crate) mod dispatcher;

use std::path::PathBuf;

use protocol::{HookEventName, HookSource};

/// Shell command selected from hook configuration.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub(crate) struct ConfiguredHandler {
    pub event_name: HookEventName,
    pub matcher: Option<String>,
    pub command: String,
    pub timeout_sec: u64,
    pub status_message: Option<String>,
    pub source_path: PathBuf,
    pub source: HookSource,
    pub display_order: i64,
}

/// File-system roots used for hook discovery.
#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    pub project_cwd: PathBuf,
    pub user_home: Option<PathBuf>,
}

/// Result of hook discovery.
#[derive(Debug, Default)]
pub struct DiscoveryResult {
    pub(crate) handlers: Vec<ConfiguredHandler>,
    pub warnings: Vec<String>,
}
```

`crates/hook/src/engine/discovery.rs` 实现 `discover_handlers(config: DiscoveryConfig) -> DiscoveryResult`，按顺序读取：

```rust
let project_path = config.project_cwd.join(".claw/hooks.json");
let user_path = config.user_home.map(|home| home.join(".claw/hooks.json"));
```

对每个 `HookHandlerConfig::Command`：
- `r#async == true`：warning + skip。
- `command.trim().is_empty()`：warning + skip。
- `timeout_sec.unwrap_or(600).max(1)`。
- 非 Windows 下忽略 `command_windows`。
- matcher invalid：warning + skip whole group。

- [ ] **Step 6: 运行 matcher/discovery 测试**

Run: `rtk cargo test -p hook events::common::tests engine::discovery::tests`

Expected: PASS。

## Task 4: command runner、dispatcher 和 output parser

**Files:**
- Create: `crates/hook/src/engine/command_runner.rs`
- Create: `crates/hook/src/engine/dispatcher.rs`
- Create: `crates/hook/src/engine/output_parser.rs`
- Modify: `crates/hook/src/engine/mod.rs`
- Test: same files

- [ ] **Step 1: 写 command runner 测试**

在 `command_runner.rs` 写测试，验证 stdin、stdout、exit code 2、timeout。

```rust
#[tokio::test]
async fn command_receives_json_on_stdin() {
    let handler = test_handler("python3 -c 'import sys; print(sys.stdin.read())'");
    let result = run_command(&handler, "{\"ok\":true}", std::env::temp_dir().as_path()).await;

    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout.trim(), "{\"ok\":true}");
    assert!(result.error.is_none());
}
```

- [ ] **Step 2: 实现 command runner**

`run_command()` 使用 `tokio::process::Command`：

```rust
/// Execute a hook command with JSON stdin in the provided working directory.
pub(crate) async fn run_command(
    handler: &ConfiguredHandler,
    input_json: &str,
    cwd: &std::path::Path,
) -> CommandRunResult {
    let started_at = chrono::Utc::now().timestamp();
    let started = std::time::Instant::now();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let mut command = tokio::process::Command::new(shell);
    command
        .arg("-lc")
        .arg(&handler.command)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    // Spawn, write stdin, wait with timeout, and return lossy UTF-8 output.
    // Keep process errors model-visible through HookRunStatus::Failed later.
}
```

完成 `CommandRunResult` 字段：`started_at`、`completed_at`、`duration_ms`、`exit_code`、`stdout`、`stderr`、`error`。

- [ ] **Step 3: 写 output parser 测试**

覆盖 PreToolUse deny、allow rewrite、invalid allow、PermissionRequest reserved fields、UserPromptSubmit block。

```rust
#[test]
fn pre_tool_use_rejects_updated_input_without_allow() {
    let parsed = parse_pre_tool_use(
        r#"{"continue":true,"hookSpecificOutput":{"hookEventName":"PreToolUse","updatedInput":{"command":"pwd"}}}"#,
    )
    .expect("parse pre tool use");

    assert_eq!(
        parsed.invalid_reason.as_deref(),
        Some("PreToolUse hook returned updatedInput without permissionDecision:allow")
    );
}
```

- [ ] **Step 4: 实现 output parser**

定义内部 wire structs，使用 `#[serde(rename_all = "camelCase")]` 和 `#[serde(deny_unknown_fields)]`。实现函数：

```rust
pub(crate) fn parse_pre_tool_use(stdout: &str) -> Option<PreToolUseOutput>;
pub(crate) fn parse_post_tool_use(stdout: &str) -> Option<PostToolUseOutput>;
pub(crate) fn parse_permission_request(stdout: &str) -> Option<PermissionRequestOutput>;
pub(crate) fn parse_user_prompt_submit(stdout: &str) -> Option<UserPromptSubmitOutput>;
pub(crate) fn parse_stop(stdout: &str) -> Option<StopOutput>;
pub(crate) fn parse_stateless(stdout: &str) -> Option<StatelessOutput>;
pub(crate) fn looks_like_json(stdout: &str) -> bool;
```

输出结构必须区分：
- `invalid_reason`
- `block_reason`
- `updated_input`
- `additional_context`
- `continue_processing`
- `stop_reason`
- `system_message`

- [ ] **Step 5: 写 dispatcher 测试**

测试 alias 去重和 declaration order。

```rust
#[test]
fn alias_matches_handler_once() {
    let handlers = vec![test_handler_with_matcher("apply_patch|write_file")];
    let selected = select_handlers_for_matcher_inputs(
        &handlers,
        HookEventName::PreToolUse,
        &["apply_patch", "write_file"],
    );

    assert_eq!(selected.len(), 1);
}
```

- [ ] **Step 6: 实现 dispatcher**

实现：

```rust
/// Select handlers for an event and optional matcher inputs.
pub(crate) fn select_handlers_for_matcher_inputs(
    handlers: &[ConfiguredHandler],
    event_name: HookEventName,
    matcher_inputs: &[&str],
) -> Vec<ConfiguredHandler>;

/// Execute selected handlers concurrently and return results in declaration order.
pub(crate) async fn execute_handlers<T>(
    handlers: Vec<ConfiguredHandler>,
    input_json: String,
    cwd: &std::path::Path,
    turn_id: Option<protocol::TurnId>,
    parse: fn(&ConfiguredHandler, CommandRunResult, Option<protocol::TurnId>) -> ParsedHandler<T>,
) -> Vec<ParsedHandler<T>>;
```

并发使用 `FuturesUnordered`，结果排序回 `configured_order`，同时记录 `completion_order`。

- [ ] **Step 7: 运行 hook engine 基础测试**

Run: `rtk cargo test -p hook engine::command_runner engine::output_parser engine::dispatcher`

Expected: PASS。

## Task 5: PreToolUse/PostToolUse 事件和 HookEngine P0 API

**Files:**
- Create: `crates/hook/src/events/pre_tool_use.rs`
- Create: `crates/hook/src/events/post_tool_use.rs`
- Modify: `crates/hook/src/events/mod.rs`
- Modify: `crates/hook/src/engine/mod.rs`
- Modify: `crates/hook/src/lib.rs`
- Test: `crates/hook/src/events/pre_tool_use.rs`
- Test: `crates/hook/src/events/post_tool_use.rs`

- [ ] **Step 1: 写 PreToolUse outcome 测试**

覆盖 “deny 丢弃 rewrite” 和 “最后完成 rewrite 胜出”。

```rust
#[test]
fn deny_discards_updated_input() {
    let results = vec![
        parsed_pre_tool_result(/*completion_order*/ 0, false, None, Some(serde_json::json!({"command":"pwd"}))),
        parsed_pre_tool_result(/*completion_order*/ 1, true, Some("blocked"), None),
    ];

    let outcome = fold_pre_tool_use_results(results, "call-1");

    assert!(outcome.should_block);
    assert_eq!(outcome.block_reason.as_deref(), Some("blocked"));
    assert!(outcome.updated_input.is_none());
}
```

- [ ] **Step 2: 实现 PreToolUse request/outcome**

`PreToolUseRequest` 字段：

```rust
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct PreToolUseRequest {
    pub session_id: protocol::SessionId,
    pub turn_id: protocol::TurnId,
    #[builder(default, setter(strip_option))]
    pub agent_id: Option<String>,
    #[builder(default, setter(strip_option))]
    pub agent_type: Option<String>,
    #[builder(default, setter(strip_option))]
    pub transcript_path: Option<std::path::PathBuf>,
    pub cwd: std::path::PathBuf,
    pub model: String,
    pub permission_mode: String,
    pub tool_name: String,
    #[builder(default)]
    pub matcher_aliases: Vec<String>,
    pub tool_use_id: String,
    pub tool_input: serde_json::Value,
}
```

`PreToolUseOutcome` 包含 `hook_events`、`should_block`、`block_reason`、`additional_contexts`、`updated_input`。

- [ ] **Step 3: 写 PostToolUse outcome 测试**

覆盖工具级失败仍执行 hook。

```rust
#[tokio::test]
async fn post_tool_use_runs_for_error_response() {
    let request = PostToolUseRequest {
        tool_response: serde_json::json!({"content":"failed","is_error":true}),
        ..post_tool_request("shell")
    };

    let outcome = run(&[handler_returning_context()], &request_shell(), request).await;

    assert_eq!(outcome.additional_contexts, vec!["context from hook".to_string()]);
}
```

- [ ] **Step 4: 实现 PostToolUse request/outcome**

`PostToolUseRequest` 同 PreToolUse，也使用 `#[derive(Debug, Clone, typed_builder::TypedBuilder)]`；`agent_id`、`agent_type`、`transcript_path` 标记 `#[builder(default, setter(strip_option))]`，`matcher_aliases` 标记 `#[builder(default)]`，并增加：

```rust
pub tool_response: serde_json::Value,
```

`PostToolUseOutcome` 包含 `should_stop`、`stop_reason`、`additional_contexts`、`feedback_message`。

- [ ] **Step 5: 实现 HookEngine P0 methods**

`crates/hook/src/engine/mod.rs` 增加：

```rust
/// Runtime engine for selecting and executing configured hooks.
pub struct HookEngine {
    handlers: Vec<ConfiguredHandler>,
    warnings: Vec<String>,
}

impl HookEngine {
    /// Discover hooks for a project and create an executable engine.
    pub fn discover(config: DiscoveryConfig) -> Self {
        let discovered = discovery::discover_handlers(config);
        Self {
            handlers: discovered.handlers,
            warnings: discovered.warnings,
        }
    }

    /// Return discovery warnings.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}
```

并导出 `preview_pre_tool_use`、`run_pre_tool_use`、`preview_post_tool_use`、`run_post_tool_use`。

- [ ] **Step 6: 运行 P0 hook tests**

Run: `rtk cargo test -p hook events::pre_tool_use events::post_tool_use`

Expected: PASS。

## Task 6: kernel 接入 P0 工具 hook

**Files:**
- Modify: root `Cargo.toml`
- Modify: `crates/kernel/Cargo.toml`
- Modify: `crates/kernel/src/session.rs`
- Modify: `crates/kernel/src/turn.rs`
- Test: `crates/kernel/src/turn.rs`

- [ ] **Step 1: 写 rewrite 后审批一致性测试**

在 `crates/kernel/src/turn.rs` tests 中新增测试：构造测试 tool，hook 把 `{"command":"unsafe"}` 改为 `{"command":"safe"}`，断言 tool 的 invocation 和 approval key 都看到 `safe`。

```rust
#[tokio::test]
async fn pre_tool_use_rewrite_rebuilds_invocation_and_approval() {
    let hook_engine = Arc::new(test_hook_engine_with_pre_tool_rewrite(
        "shell",
        serde_json::json!({"command":"safe"}),
    ));
    let tool = Arc::new(RecordingApprovalTool::new("shell"));
    let ctx = test_turn_context_with_tool_and_hooks(tool.clone(), hook_engine);
    let tool_ctx = test_tool_context(&ctx);
    let tool_call = provider::message::ToolCall::function(
        "call-1",
        "shell",
        serde_json::json!({"command":"unsafe"}),
    );

    let (output, succeeded) =
        dispatch_tool(&ctx, &test_event_tx(), "call-1", &tool_call, &tool_ctx)
            .await
            .expect("dispatch tool");

    assert!(succeeded);
    assert_eq!(output, "safe");
    assert_eq!(tool.last_invocation_arguments(), serde_json::json!({"command":"safe"}));
    assert_eq!(tool.last_approval_arguments(), serde_json::json!({"command":"safe"}));
}
```

- [ ] **Step 2: 运行失败测试**

Run: `rtk cargo test -p kernel pre_tool_use_rewrite_rebuilds_invocation_and_approval`

Expected: FAIL，因为 TurnContext 尚无 HookEngine，dispatch 未运行 hooks。

- [ ] **Step 3: 把 HookEngine 放入 Session/TurnContext**

`Session` 新增字段：

```rust
/// Lifecycle hook engine shared by this session.
pub hook_engine: Arc<hook::HookEngine>,
```

`TurnContext` 新增字段：

```rust
/// Lifecycle hook engine used by this turn.
pub hook_engine: Arc<hook::HookEngine>,
```

`spawn_thread()` 中创建：

```rust
let hook_engine = Arc::new(hook::HookEngine::discover(hook::DiscoveryConfig {
    project_cwd: cwd.clone(),
    user_home: dirs::home_dir(),
}));
```

将 `Arc::clone(&hook_engine)` 传入 runtime 和 handle。

- [ ] **Step 4: 修改 dispatch_tool 顺序**

在 `dispatch_tool()` 中，把 `let arguments = &tool_call.function.arguments;` 改成 mutable local：

```rust
let mut final_arguments = tool_call.function.arguments.clone();
let pre_outcome = ctx
    .hook_engine
    .run_pre_tool_use(build_pre_tool_use_request(ctx, call_id, tool_name, &final_arguments))
    .await;
emit_hook_events(ctx, tx_event, pre_outcome.hook_events);

if pre_outcome.should_block {
    let message = pre_outcome
        .block_reason
        .unwrap_or_else(|| "blocked by PreToolUse hook".to_string());
    let _ = tx_event.send(Event::tool_call_update(
        ctx.session_id.clone(),
        call_id,
        Some(message.clone()),
        Some(ToolCallStatus::Failed),
    ));
    return Ok((message, false));
}

if let Some(updated_input) = pre_outcome.updated_input {
    final_arguments = updated_input;
}
```

随后所有 `invocation()`、approval、tool call event arguments 都使用 `final_arguments.clone()`。

- [ ] **Step 5: 接入 PostToolUse**

在 stream 消费完成后、`Ok((output_text, succeeded))` 前调用：

```rust
let post_outcome = ctx
    .hook_engine
    .run_post_tool_use(build_post_tool_use_request(
        ctx,
        call_id,
        tool_name,
        &final_arguments,
        &output_text,
        !succeeded,
    ))
    .await;
emit_hook_events(ctx, tx_event, post_outcome.hook_events);
```

如果 `post_outcome.feedback_message` 非空，则把它追加到 `output_text`：

```rust
if let Some(feedback) = post_outcome.feedback_message {
    output_text = format!("{output_text}\n\n{feedback}");
}
```

`additional_contexts` 先以同样方式追加到模型反馈；后续 Task 8 再改为独立 context message，避免 P0 接入过大。

- [ ] **Step 6: 运行 P0 kernel 测试**

Run: `rtk cargo test -p kernel pre_tool_use_rewrite_rebuilds_invocation_and_approval`

Expected: PASS。

## Task 7: SessionStart、UserPromptSubmit 和 Stop

**Files:**
- Create: `crates/hook/src/events/session_start.rs`
- Create: `crates/hook/src/events/user_prompt_submit.rs`
- Create: `crates/hook/src/events/stop.rs`
- Modify: `crates/hook/src/engine/mod.rs`
- Modify: `crates/kernel/src/session.rs`
- Modify: `crates/kernel/src/turn.rs`
- Test: hook event files
- Test: `crates/kernel/src/session.rs`
- Test: `crates/kernel/src/turn.rs`

- [ ] **Step 1: 实现 hook crate 的三个事件**

按 P0 模式新增：
- `SessionStartRequest { session_id, transcript_path, cwd, model, permission_mode, source }`
- `UserPromptSubmitRequest { session_id, turn_id, agent_id, agent_type, transcript_path, cwd, model, permission_mode, prompt }`
- `StopRequest { session_id, turn_id, transcript_path, cwd, model, permission_mode, stop_hook_active, last_assistant_message }`

每个文件必须有 event-specific fold 测试。

- [ ] **Step 2: 写 UserPromptSubmit block 测试**

在 kernel `execute_turn()` 测试中，hook 返回 `decision:block`，断言不会 `context.push(user_message)`，不会持久化 user prompt。

Run: `rtk cargo test -p kernel user_prompt_submit_block_prevents_prompt_persistence`

Expected: FAIL。

- [ ] **Step 3: 接入 UserPromptSubmit**

在 `execute_turn()` 构建 `user_message` 前调用：

```rust
let prompt_outcome = ctx
    .hook_engine
    .run_user_prompt_submit(build_user_prompt_submit_request(ctx, &user_text))
    .await;
emit_hook_events(ctx, tx_event, prompt_outcome.hook_events);
if prompt_outcome.should_stop {
    return Ok(Usage::default());
}
```

如果有 `additional_contexts`，在用户消息入 context 后逐条 `context.push(Message::user(text))` 并持久化。

- [ ] **Step 4: 写 SessionStart 注入测试**

构造 `run_loop` 首个 prompt 前 hook 返回 context，断言第一轮 request history 包含该 context 且 recorder 有对应 `MessageRecord`。

Run: `rtk cargo test -p kernel session_start_injects_context_before_first_prompt`

Expected: FAIL。

- [ ] **Step 5: 接入 SessionStart**

在 `run_loop()` 中增加：

```rust
let mut session_start_ran = false;
```

处理 prompt 前调用私有函数：

```rust
/// Runs SessionStart once and injects model-visible context messages.
async fn run_session_start_if_needed(rt: &mut Session, session_start_ran: &mut bool) {
    if *session_start_ran {
        return;
    }
    *session_start_ran = true;
    let outcome = rt.hook_engine.run_session_start(build_session_start_request(rt)).await;
    for completed in outcome.hook_events {
        let _ = rt
            .tx_event
            .lock()
            .await
            .send(Event::hook_completed(rt.session_id.clone(), completed));
    }
    for text in outcome.additional_contexts {
        let message = Message::user(text);
        rt.context.push(message.clone());
        let _ = rt.recorder.append(&[PersistedPayload::Message(
            MessageRecord::builder()
                .turn_id("session-start".to_string())
                .message(message)
                .usage(None)
                .build(),
        )]).await;
    }
}
```

- [ ] **Step 6: 写 Stop continuation 测试**

Hook 返回 `decision:block` + `reason:"run tests"`；LLM 第一次 final 后应继续一轮，第二轮收到 continuation。

Run: `rtk cargo test -p kernel stop_hook_block_creates_continuation_prompt`

Expected: FAIL。

- [ ] **Step 7: 接入 Stop**

在 `if tool_outputs.is_empty() && is_answer && final_received` 分支返回前调用 `run_stop()`。如果 `should_block`，push continuation `Message::user(block_reason)` 并 `continue` loop；否则 return。

- [ ] **Step 8: 运行 lifecycle 测试**

Run: `rtk cargo test -p hook events::session_start events::user_prompt_submit events::stop`

Expected: PASS。

Run: `rtk cargo test -p kernel user_prompt_submit_block_prevents_prompt_persistence session_start_injects_context_before_first_prompt stop_hook_block_creates_continuation_prompt`

Expected: PASS。

## Task 8: PermissionRequest 和审批流

**Files:**
- Create: `crates/hook/src/events/permission_request.rs`
- Modify: `crates/hook/src/engine/mod.rs`
- Modify: `crates/kernel/src/turn.rs`
- Test: `crates/hook/src/events/permission_request.rs`
- Test: `crates/kernel/src/turn.rs`

- [ ] **Step 1: 写 hook fold 测试**

```rust
#[test]
fn deny_overrides_allow() {
    let decision = resolve_permission_request_decision([
        PermissionRequestDecision::Allow,
        PermissionRequestDecision::Deny {
            message: "blocked".to_string(),
        },
    ]);

    assert_eq!(
        decision,
        Some(PermissionRequestDecision::Deny {
            message: "blocked".to_string()
        })
    );
}
```

- [ ] **Step 2: 实现 PermissionRequest event**

`PermissionRequestRequest` 包含最终 tool arguments，不含 `tool_use_id`。Outcome：

```rust
pub struct PermissionRequestOutcome {
    pub hook_events: Vec<protocol::HookCompletedEvent>,
    pub decision: Option<PermissionRequestDecision>,
}
```

- [ ] **Step 3: 写 kernel 审批测试**

Hook deny 时不发送 `ExecApprovalRequested` event，返回 model-facing rejection。

Run: `rtk cargo test -p kernel permission_request_deny_skips_user_approval_event`

Expected: FAIL。

- [ ] **Step 4: 接入 resolve_tool_approval**

在 `NeedsApproval` 分支中，发送 UI approval event 前调用：

```rust
let hook_outcome = self
    .hook_engine
    .run_permission_request(build_permission_request(self, invocation))
    .await;
emit_hook_events(self, tx_event, hook_outcome.hook_events);
match hook_outcome.decision {
    Some(hook::PermissionRequestDecision::Allow) => {
        return Ok(ToolApprovalResolution::Approved {
            decision: ReviewDecision::Approved,
            prompted: false,
        });
    }
    Some(hook::PermissionRequestDecision::Deny { message }) => {
        return Ok(ToolApprovalResolution::Forbidden { reason: message });
    }
    None => {}
}
```

- [ ] **Step 5: 运行审批测试**

Run: `rtk cargo test -p hook events::permission_request`

Expected: PASS。

Run: `rtk cargo test -p kernel permission_request_deny_skips_user_approval_event`

Expected: PASS。

## Task 9: PreCompact/PostCompact

**Files:**
- Create: `crates/hook/src/events/compact.rs`
- Modify: `crates/hook/src/engine/mod.rs`
- Modify: `crates/kernel/src/context.rs`
- Modify: `crates/kernel/src/compaction.rs`
- Modify: `crates/kernel/src/turn.rs`
- Test: `crates/hook/src/events/compact.rs`
- Test: `crates/kernel/src/compaction.rs`

- [ ] **Step 1: 写 compact matcher 测试**

Hook matcher `manual` 只在 manual compact 时执行，`auto` 只在 auto compact 时执行。

Run: `rtk cargo test -p hook compact_hooks_match_trigger`

Expected: FAIL。

- [ ] **Step 2: 实现 compact event**

`PreCompactRequest` / `PostCompactRequest` 字段包含 `session_id`、`turn_id`、`agent_id`、`agent_type`、`transcript_path`、`cwd`、`model`、`trigger`。

输出只支持 universal 层：`should_stop`、`stop_reason`、`hook_events`。

- [ ] **Step 3: 调整 ContextCompactor API**

将 `ContextCompactor::compact_history()` 增加可选 hook 参数：

```rust
pub(crate) async fn compact_history(
    &self,
    llm: &dyn Llm,
    history: &[Message],
    hooks: Option<CompactionHookContext<'_>>,
) -> anyhow::Result<Option<CompactionOutput>>
```

新增结构：

```rust
/// Runtime data needed to run compaction hooks.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct CompactionHookContext<'a> {
    pub hook_engine: &'a hook::HookEngine,
    pub session_id: &'a SessionId,
    pub turn_id: &'a TurnId,
    pub cwd: &'a std::path::Path,
    pub model: &'a str,
    pub trigger: &'a str,
}
```

- [ ] **Step 4: 接入 auto compact**

在 `TurnContext::auto_compact_if_needed()` 调用 `context.compact()` 前后传入 `trigger: "auto"`。如果 PreCompact `should_stop`，返回 `AutoCompactionOutcome::SuspendForTurn`。

- [ ] **Step 5: 运行 compact 测试**

Run: `rtk cargo test -p hook events::compact`

Expected: PASS。

Run: `rtk cargo test -p kernel compaction`

Expected: PASS。

## Task 10: HookCompleted 事件、warnings 和端到端覆盖

**Files:**
- Modify: `crates/kernel/src/turn.rs`
- Modify: `crates/kernel/src/session.rs`
- Modify: `crates/tui/src/acp/client.rs`
- Test: `crates/kernel/src/turn.rs`
- Test: `crates/tui/src/acp/client.rs`

- [ ] **Step 1: 增加统一事件发送 helper**

在 `turn.rs` 增加私有函数：

```rust
/// Emits completed hook events to the frontend event stream.
fn emit_hook_events(
    session_id: &SessionId,
    tx_event: &mpsc::UnboundedSender<Event>,
    hook_events: Vec<protocol::HookCompletedEvent>,
) {
    for completed in hook_events {
        let _ = tx_event.send(Event::hook_completed(session_id.clone(), completed));
    }
}
```

- [ ] **Step 2: 写端到端 hook event 测试**

PreToolUse block 后，event stream 中应包含 `HookCompleted` 且 status 为 `blocked`。

Run: `rtk cargo test -p kernel pre_tool_use_block_emits_hook_completed_event`

Expected: FAIL。

- [ ] **Step 3: 接入所有 run_* 调用点**

每个 hook outcome 返回后立即调用 `emit_hook_events(...)`：
- SessionStart
- UserPromptSubmit
- PreToolUse
- PermissionRequest
- PostToolUse
- PreCompact
- PostCompact
- Stop

- [ ] **Step 4: ACP/TUI 忽略未知 hook event 或透传**

如果 `crates/tui/src/acp/client.rs` 的 event match 需要 exhaustive handling，加入 `Event::HookCompleted { .. } => None` 或转换为内部日志事件。不要让 hook event 破坏现有 UI。

- [ ] **Step 5: 运行端到端测试**

Run: `rtk cargo test -p kernel pre_tool_use_block_emits_hook_completed_event`

Expected: PASS。

Run: `rtk cargo test -p tui`

Expected: PASS。

## Task 11: 全量验证

**Files:**
- Verify only.

- [ ] **Step 1: 格式化**

Run: `rtk cargo fmt`

Expected: exit 0。

- [ ] **Step 2: hook/config/protocol targeted tests**

Run: `rtk cargo test -p config hook::tests`

Expected: PASS。

Run: `rtk cargo test -p protocol hook::tests`

Expected: PASS。

Run: `rtk cargo test -p hook`

Expected: PASS。

- [ ] **Step 3: kernel targeted tests**

Run: `rtk cargo test -p kernel pre_tool_use_rewrite_rebuilds_invocation_and_approval permission_request_deny_skips_user_approval_event stop_hook_block_creates_continuation_prompt`

Expected: PASS。

Run: `rtk cargo test -p kernel compaction`

Expected: PASS。

- [ ] **Step 4: UI compatibility tests**

Run: `rtk cargo test -p tui`

Expected: PASS。

- [ ] **Step 5: Diff review**

Run: `rtk git diff -- crates/config crates/hook crates/protocol crates/kernel crates/tui docs/superpowers/specs/2026-06-04-hook-mechanism-design.md docs/superpowers/plans/2026-06-04-hook-mechanism.md`

Expected:
- 所有新增/修改代码的非平凡逻辑有英文注释。
- 所有新增函数有英文函数级注释。
- `Arc<T>` 字段 clone 使用 `Arc::clone(&field)`。
- 超过 3 个字段的新 struct 使用 `typed-builder`，`Option` 字段必须配置 `#[builder(default)]` 或 `#[builder(default, setter(strip_option))]`。
- 未经用户明确许可不运行 `git commit`。

## 自检结果

- 覆盖 spec：P0/P1/P2/P3 hook、matcher、command runner、输出协议、kernel 集成、测试策略均有对应任务。
- 范围控制：未加入 trust 模型、Windows runtime、async runtime、prompt/agent handler 执行。
- 风险点：`PostToolUse.additionalContext` 在 Task 6 先用模型反馈承载，若执行时要求严格独立 context message，应在 Task 10 前补一个专门的 context-injection helper。
