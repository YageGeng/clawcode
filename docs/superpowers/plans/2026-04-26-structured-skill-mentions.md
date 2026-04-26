# Structured Skill Mentions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace text-only skill mention handling with structured user inputs and Codex-compatible explicit skill selection rules.

**Architecture:** `skills` owns Codex-style mention selection over a crate-local `SkillInput` model. `kernel` owns runtime-facing `UserInput`, converts it to `skills::SkillInput` at the integration boundary, and converts only text inputs into normal model messages. Existing text constructors remain ergonomic but now store structured inputs internally.

**Tech Stack:** Rust 2024, SNAFU, Tokio tests, existing `llm::completion::Message`, existing `kernel` runtime and CLI tests.

---

## File Structure

- Modify `crates/skills/src/model.rs`: add `SkillInput` and `SkillMentionOptions`.
- Modify `crates/skills/src/mentions.rs`: replace text-only collector with structured-input collector and Codex selection rules.
- Modify `crates/skills/tests/skills.rs`: add Codex-rule tests and update existing collector test.
- Create `crates/kernel/src/input.rs`: kernel-owned `UserInput` plus display/message/conversion helpers.
- Modify `crates/kernel/src/lib.rs`: export `UserInput` and helper functions where needed.
- Modify `crates/kernel/src/runtime/task/api.rs`: store `Vec<UserInput>` in `ThreadRunRequest` and `RunRequest`.
- Modify `crates/kernel/src/runtime/task/runner.rs`: emit display text for `RunStarted`.
- Modify `crates/kernel/src/runtime/turn/runner.rs`: collect skills from structured inputs, inject skill bodies, and append text input messages.
- Modify `crates/kernel/src/runtime/continuation/decider.rs`: wrap continuation text into `RunRequest::new`.
- Modify `crates/kernel/tests/agent_loop.rs`: add structured skill input tests and adjust helpers if needed.
- Modify `crates/cli/src/runtime.rs`: continue using string constructors, which now wrap text input.

## Task 1: Skills Structured Mention API

**Files:**
- Modify: `crates/skills/src/model.rs`
- Modify: `crates/skills/src/mentions.rs`
- Modify: `crates/skills/tests/skills.rs`

- [ ] **Step 1: Write failing tests for full selection rules**

Append these tests to `crates/skills/tests/skills.rs`:

```rust
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use skills::{SkillInput, SkillMentionOptions};

fn mention_options() -> SkillMentionOptions {
    SkillMentionOptions::default()
}

fn skill(name: &str, path: &str) -> SkillMetadata {
    SkillMetadata {
        name: name.to_string(),
        description: format!("{name} skill"),
        path: PathBuf::from(path),
    }
}

/// Verifies structured skill input selects by exact path without requiring text mention.
#[test]
fn structured_skill_input_selects_by_path() {
    let alpha = skill("alpha-skill", "/tmp/alpha/SKILL.md");
    let inputs = vec![SkillInput::skill("alpha-skill", "/tmp/alpha/SKILL.md")];

    let selected = collect_explicit_skill_mentions(&inputs, &[alpha.clone()], &mention_options());

    assert_eq!(selected, vec![alpha]);
}

/// Verifies missing structured path blocks same-name plain mention fallback.
#[test]
fn structured_missing_path_blocks_plain_name_fallback() {
    let alpha = skill("alpha-skill", "/tmp/alpha/SKILL.md");
    let inputs = vec![
        SkillInput::skill("alpha-skill", "/tmp/missing/SKILL.md"),
        SkillInput::text("use $alpha-skill"),
    ];

    let selected = collect_explicit_skill_mentions(&inputs, &[alpha], &mention_options());

    assert!(selected.is_empty());
}

/// Verifies disabled structured path blocks same-name plain mention fallback.
#[test]
fn structured_disabled_path_blocks_plain_name_fallback() {
    let alpha = skill("alpha-skill", "/tmp/alpha/SKILL.md");
    let inputs = vec![
        SkillInput::skill("alpha-skill", "/tmp/alpha/SKILL.md"),
        SkillInput::text("use $alpha-skill"),
    ];
    let options = SkillMentionOptions {
        disabled_paths: HashSet::from([PathBuf::from("/tmp/alpha/SKILL.md")]),
        connector_slug_counts: HashMap::new(),
    };

    let selected = collect_explicit_skill_mentions(&inputs, &[alpha], &options);

    assert!(selected.is_empty());
}

/// Verifies linked paths resolve ambiguous skill names by exact path.
#[test]
fn linked_path_selects_when_plain_name_is_ambiguous() {
    let alpha = skill("demo-skill", "/tmp/alpha/SKILL.md");
    let beta = skill("demo-skill", "/tmp/beta/SKILL.md");
    let inputs = vec![SkillInput::text(
        "use $demo-skill and [$demo-skill](skill:///tmp/beta/SKILL.md)",
    )];

    let selected = collect_explicit_skill_mentions(&inputs, &[alpha, beta.clone()], &mention_options());

    assert_eq!(selected, vec![beta]);
}

/// Verifies plain ambiguous names select nothing.
#[test]
fn plain_ambiguous_name_selects_nothing() {
    let alpha = skill("demo-skill", "/tmp/alpha/SKILL.md");
    let beta = skill("demo-skill", "/tmp/beta/SKILL.md");
    let inputs = vec![SkillInput::text("use $demo-skill")];

    let selected = collect_explicit_skill_mentions(&inputs, &[alpha, beta], &mention_options());

    assert!(selected.is_empty());
}

/// Verifies connector slug conflicts suppress plain-name skill matching.
#[test]
fn connector_slug_conflict_suppresses_plain_name() {
    let alpha = skill("alpha-skill", "/tmp/alpha/SKILL.md");
    let inputs = vec![SkillInput::text("use $alpha-skill")];
    let options = SkillMentionOptions {
        disabled_paths: HashSet::new(),
        connector_slug_counts: HashMap::from([("alpha-skill".to_string(), 1)]),
    };

    let selected = collect_explicit_skill_mentions(&inputs, &[alpha], &options);

    assert!(selected.is_empty());
}

/// Verifies linked path wins even when connector slug conflicts with the skill name.
#[test]
fn linked_path_ignores_connector_slug_conflict() {
    let alpha = skill("alpha-skill", "/tmp/alpha/SKILL.md");
    let inputs = vec![SkillInput::text("use [$alpha-skill](/tmp/alpha/SKILL.md)")];
    let options = SkillMentionOptions {
        disabled_paths: HashSet::new(),
        connector_slug_counts: HashMap::from([("alpha-skill".to_string(), 1)]),
    };

    let selected = collect_explicit_skill_mentions(&inputs, &[alpha.clone()], &options);

    assert_eq!(selected, vec![alpha]);
}

/// Verifies common shell environment variables are not treated as skill mentions.
#[test]
fn common_env_vars_are_ignored() {
    let path_skill = skill("PATH", "/tmp/path/SKILL.md");
    let alpha = skill("alpha-skill", "/tmp/alpha/SKILL.md");
    let inputs = vec![SkillInput::text("use $PATH and $XDG_CONFIG_HOME and $alpha-skill")];

    let selected = collect_explicit_skill_mentions(&inputs, &[path_skill, alpha.clone()], &mention_options());

    assert_eq!(selected, vec![alpha]);
}
```

- [ ] **Step 2: Run tests to verify RED**

Run: `rtk cargo test -p skills structured_skill_input_selects_by_path`

Expected: FAIL because `SkillInput` and `SkillMentionOptions` do not exist and the collector has the old signature.

- [ ] **Step 3: Implement `SkillInput` and `SkillMentionOptions`**

Add to `crates/skills/src/model.rs`:

```rust
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillInput {
    Text { text: String },
    Skill { name: String, path: PathBuf },
}

impl SkillInput {
    /// Builds a text input used for plain `$skill` mention scanning.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// Builds a structured skill selection that resolves by exact path.
    pub fn skill(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self::Skill { name: name.into(), path: path.into() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillMentionOptions {
    pub disabled_paths: HashSet<PathBuf>,
    pub connector_slug_counts: HashMap<String, usize>,
}
```

Export both from `crates/skills/src/lib.rs`.

- [ ] **Step 4: Replace mention collector implementation**

Implement `collect_explicit_skill_mentions(inputs, skills, options)` in `crates/skills/src/mentions.rs` with:

- structured skill inputs first, path exact match
- blocked plain names for every structured skill input name
- linked path matching before plain name matching
- disabled path filtering
- duplicate path filtering
- ambiguous plain name skip
- connector slug conflict skip
- common env var skip
- `skill://` prefix normalization and direct `SKILL.md` path support

- [ ] **Step 5: Run skills tests to verify GREEN**

Run: `rtk cargo test -p skills`

Expected: PASS for all skills tests.

## Task 2: Kernel Structured UserInput Model

**Files:**
- Create: `crates/kernel/src/input.rs`
- Modify: `crates/kernel/src/lib.rs`
- Modify: `crates/kernel/src/runtime/task/api.rs`
- Modify: `crates/kernel/tests/agent_loop.rs`

- [ ] **Step 1: Write failing kernel input tests**

Add tests to `crates/kernel/tests/agent_loop.rs`:

```rust
use kernel::{UserInput, user_inputs_display_text, user_inputs_to_messages};

/// Verifies skill inputs are display metadata but not normal model messages.
#[test]
fn user_input_helpers_skip_skill_inputs_for_model_messages() {
    let inputs = vec![
        UserInput::skill("alpha-skill", "/tmp/alpha/SKILL.md"),
        UserInput::text("hello"),
    ];

    let messages = user_inputs_to_messages(&inputs);

    assert_eq!(user_inputs_display_text(&inputs), "[skill:alpha-skill](/tmp/alpha/SKILL.md)\nhello");
    assert_eq!(messages, vec![Message::user("hello")]);
}
```

- [ ] **Step 2: Run test to verify RED**

Run: `rtk cargo test -p kernel user_input_helpers_skip_skill_inputs_for_model_messages`

Expected: FAIL because `UserInput` and helpers do not exist.

- [ ] **Step 3: Implement kernel input module**

Create `crates/kernel/src/input.rs`:

```rust
use std::path::PathBuf;
use llm::completion::Message;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserInput {
    Text { text: String },
    Skill { name: String, path: PathBuf },
}

impl UserInput {
    /// Builds a plain text input that becomes a prompt-visible user message.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// Builds a structured skill selection that does not become a user message.
    pub fn skill(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self::Skill { name: name.into(), path: path.into() }
    }
}

/// Renders inputs into a durable display string for events and turn history.
pub fn user_inputs_display_text(inputs: &[UserInput]) -> String {
    inputs.iter().map(display_one_input).collect::<Vec<_>>().join("\n")
}

/// Converts only text inputs into prompt-visible messages.
pub fn user_inputs_to_messages(inputs: &[UserInput]) -> Vec<Message> {
    inputs.iter().filter_map(|input| match input {
        UserInput::Text { text } => Some(Message::user(text.clone())),
        UserInput::Skill { .. } => None,
    }).collect()
}

/// Converts kernel user inputs into skills-crate inputs for mention collection.
pub fn user_inputs_to_skill_inputs(inputs: &[UserInput]) -> Vec<skills::SkillInput> {
    inputs.iter().map(|input| match input {
        UserInput::Text { text } => skills::SkillInput::text(text.clone()),
        UserInput::Skill { name, path } => skills::SkillInput::skill(name.clone(), path.clone()),
    }).collect()
}

/// Renders one input for durable event/history display.
fn display_one_input(input: &UserInput) -> String {
    match input {
        UserInput::Text { text } => text.clone(),
        UserInput::Skill { name, path } => format!("[skill:{name}]({})", path.display()),
    }
}
```

Export it from `crates/kernel/src/lib.rs`.

- [ ] **Step 4: Update request structs**

Change `ThreadRunRequest` and `RunRequest` in `crates/kernel/src/runtime/task/api.rs`:

```rust
pub struct ThreadRunRequest {
    pub inputs: Vec<UserInput>,
    pub system_prompt_override: Option<String>,
}

impl ThreadRunRequest {
    pub fn new(input: impl Into<String>) -> Self {
        Self::from_inputs(vec![UserInput::text(input)])
    }

    pub fn from_inputs(inputs: Vec<UserInput>) -> Self {
        Self { inputs, system_prompt_override: None }
    }
}

pub struct RunRequest {
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    pub inputs: Vec<UserInput>,
    pub display_input: String,
}

impl RunRequest {
    pub fn new(session_id: SessionId, thread_id: ThreadId, input: impl Into<String>) -> Self {
        Self::from_inputs(session_id, thread_id, vec![UserInput::text(input)])
    }

    pub fn from_inputs(session_id: SessionId, thread_id: ThreadId, inputs: Vec<UserInput>) -> Self {
        let display_input = user_inputs_display_text(&inputs);
        Self { session_id, thread_id, inputs, display_input }
    }
}
```

Update `ThreadRuntime::run_outcome` to pass `request.inputs`.

- [ ] **Step 5: Run kernel input test to verify GREEN**

Run: `rtk cargo test -p kernel user_input_helpers_skip_skill_inputs_for_model_messages`

Expected: PASS.

## Task 3: Runtime Skill Collection And Message Conversion

**Files:**
- Modify: `crates/kernel/src/runtime/task/runner.rs`
- Modify: `crates/kernel/src/runtime/turn/runner.rs`
- Modify: `crates/kernel/src/runtime/continuation/decider.rs`
- Modify: `crates/kernel/tests/agent_loop.rs`

- [ ] **Step 1: Write failing runtime structured skill tests**

Add tests to `crates/kernel/tests/agent_loop.rs`:

```rust
/// Verifies structured skill input injects a skill without requiring `$skill-name` text.
#[tokio::test]
async fn runner_injects_structured_skill_input_without_text_mention() {
    let skill_root = write_test_skill_root();
    let model = Arc::new(RecordingModel::new(vec![ModelResponse::text("done", usage(4))]));
    let store = Arc::new(InMemorySessionStore::default());
    let router = Arc::new(ToolRouter::new(Arc::new(kernel::tools::ToolRegistry::default()), Vec::new()));
    let sink = Arc::new(RecordingEventSink::default());
    let skill_path = skill_root.path().join("rust-error-snafu/SKILL.md");
    let runtime = ThreadRuntime::new(Arc::clone(&model), store, router, sink).with_config(AgentLoopConfig {
        skills: skills::SkillConfig { roots: vec![skill_root.path().to_path_buf()], cwd: None, enabled: true },
        ..AgentLoopConfig::default()
    });

    runtime
        .run_request(RunRequest::from_inputs(
            SessionId::new(),
            ThreadId::new(),
            vec![
                UserInput::skill("rust-error-snafu", skill_path),
                UserInput::text("create an error enum"),
            ],
        ))
        .await
        .unwrap();

    let requests = model.requests().await;
    let request = requests.first().expect("model should receive one request");

    assert!(request.messages.iter().any(|message| first_user_text(message).contains("<skill_instructions")));
    assert!(request.messages.iter().any(|message| first_user_text(message).contains("Use SNAFU context.")));
    assert_eq!(request.messages.last(), Some(&Message::user("create an error enum")));
    assert!(!request.messages.iter().any(|message| first_user_text(message).contains("[skill:rust-error-snafu]")));
}
```

- [ ] **Step 2: Run test to verify RED**

Run: `rtk cargo test -p kernel runner_injects_structured_skill_input_without_text_mention`

Expected: FAIL until runtime uses structured inputs for skill collection and message conversion.

- [ ] **Step 3: Update task runner display text**

In `crates/kernel/src/runtime/task/runner.rs`, change `RunStarted` input from `request.input.clone()` to `request.display_input.clone()`.

- [ ] **Step 4: Update turn runner**

In `crates/kernel/src/runtime/turn/runner.rs`:

```rust
let user_messages = user_inputs_to_messages(&request.inputs);
store.begin_turn_state(
    request.session_id.clone(),
    request.thread_id.clone(),
    request.display_input.clone(),
    user_messages.last().cloned().unwrap_or_else(|| Message::user(request.display_input.clone())),
).await?;

let skill_inputs = user_inputs_to_skill_inputs(&request.inputs);
let options = skills::SkillMentionOptions::default();
let selected_skills = skills::collect_explicit_skill_mentions(&skill_inputs, &skill_outcome.skills, &options);
history.extend(skill_injections);
history.extend(user_messages);
```

Keep the fallback user message only for persistence APIs that require one active user message; normal model messages should still only include text inputs.

- [ ] **Step 5: Update continuation decider if needed**

`SessionContinuationRequest` still carries strings. Keep `RunRequest::new(session_id, thread_id, input)` in `TaskContinuation::into_run_request`; it now wraps text into structured input.

- [ ] **Step 6: Run kernel test to verify GREEN**

Run: `rtk cargo test -p kernel runner_injects_structured_skill_input_without_text_mention`

Expected: PASS.

## Task 4: Update Existing Call Sites And Tests

**Files:**
- Modify: `crates/kernel/tests/agent_loop.rs`
- Modify: `crates/cli/src/runtime.rs` only if compile errors require it.

- [ ] **Step 1: Run kernel tests and fix compile errors**

Run: `rtk cargo test -p kernel`

Expected before fixes: FAIL on stale `request.input` field usage or old skill collector signature if any remain.

- [ ] **Step 2: Replace stale field access**

Use these replacements:

- `request.input` -> `request.display_input` for events/history display.
- `request.input.clone()` -> `request.display_input.clone()` when a display string is required.
- Keep constructor calls `RunRequest::new(..., "text")` unchanged.

- [ ] **Step 3: Run kernel tests to verify GREEN**

Run: `rtk cargo test -p kernel`

Expected: PASS.

- [ ] **Step 4: Run CLI tests to catch constructor regressions**

Run: `rtk cargo test -p cli`

Expected: PASS. If the crate has no focused runtime failures, existing `ThreadRunRequest::new(prompt)` behavior is preserved.

## Task 5: Final Verification And Commit

**Files:**
- All modified files from prior tasks.

- [ ] **Step 1: Run skills tests**

Run: `rtk cargo test -p skills`

Expected: PASS.

- [ ] **Step 2: Run kernel tests**

Run: `rtk cargo test -p kernel`

Expected: PASS.

- [ ] **Step 3: Run CLI tests**

Run: `rtk cargo test -p cli`

Expected: PASS.

- [ ] **Step 4: Run workspace check**

Run: `rtk cargo check --workspace`

Expected: PASS.

- [ ] **Step 5: Commit implementation**

```bash
rtk git add crates/skills/src/model.rs crates/skills/src/mentions.rs crates/skills/tests/skills.rs crates/kernel/src/input.rs crates/kernel/src/lib.rs crates/kernel/src/runtime/task/api.rs crates/kernel/src/runtime/task/runner.rs crates/kernel/src/runtime/turn/runner.rs crates/kernel/src/runtime/continuation/decider.rs crates/kernel/tests/agent_loop.rs crates/cli/src/runtime.rs
rtk git commit -m "refactor: add structured skill mentions"
```

Expected: commit succeeds after fmt/clippy hooks. If fmt modifies files, re-add, rerun affected tests, and commit again.

## Self-Review

- Spec coverage: tasks implement structured `UserInput`, remove the text-only collector, add Codex-compatible mention rules, update runtime flow, keep CLI text behavior, and add skills/kernel/CLI verification.
- Scope check: app discovery, disable config loading, UI picker support, and implicit invocation remain out of scope while API seams support future integration.
- Placeholder scan: no implementation step depends on unspecified future work.
- Type consistency: `skills::SkillInput` is crate-local; `kernel::UserInput` converts to it, avoiding dependency cycles.
