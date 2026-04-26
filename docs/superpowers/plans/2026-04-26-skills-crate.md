# Skills Crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a reusable `skills` crate and wire explicit skill invocation into the kernel runtime.

**Architecture:** The new `skills` crate owns discovery, parsing, mention detection, rendering, and injection message construction. `kernel` depends on it through a small `SkillConfig` carried by `AgentLoopConfig`, and `run_persisted_turn` enriches the system prompt plus working messages before calling the model loop.

**Tech Stack:** Rust 2024, SNAFU, Tokio tests, existing `llm::completion::Message`, existing `kernel` runtime tests.

---

## File Structure

- Create `crates/skills/Cargo.toml`: crate manifest with `llm`, `snafu`, and `tokio` dependencies.
- Create `crates/skills/src/lib.rs`: public module exports.
- Create `crates/skills/src/error.rs`: SNAFU error type and result alias.
- Create `crates/skills/src/model.rs`: `SkillMetadata`, `SkillLoadError`, `SkillLoadOutcome`, and `SkillConfig`.
- Create `crates/skills/src/loader.rs`: filesystem discovery and `SKILL.md` frontmatter parser.
- Create `crates/skills/src/render.rs`: available-skills system prompt rendering.
- Create `crates/skills/src/mentions.rs`: `$skill-name` and linked `skill://` mention parsing.
- Create `crates/skills/src/injection.rs`: read selected skill bodies and wrap them in prompt-visible messages.
- Create `crates/skills/tests/skills.rs`: crate behavior tests.
- Modify `Cargo.toml`: add `tempfile` workspace dev dependency if needed for filesystem tests.
- Modify `crates/kernel/Cargo.toml`: add `skills` dependency and `tempfile` dev dependency if not already available.
- Modify `crates/kernel/src/runtime/continuation/config.rs`: add skill configuration to `AgentLoopConfig`.
- Modify `crates/kernel/src/runtime/turn/runner.rs`: load/render/inject skills before the model loop.
- Modify `crates/kernel/src/error.rs`: wrap `skills::Error`.
- Modify `crates/kernel/tests/agent_loop.rs`: add integration tests for prompt rendering and explicit injection.

## Task 1: Add Skills Crate Skeleton And Parser

**Files:**
- Create: `crates/skills/Cargo.toml`
- Create: `crates/skills/src/lib.rs`
- Create: `crates/skills/src/error.rs`
- Create: `crates/skills/src/model.rs`
- Create: `crates/skills/src/loader.rs`
- Create: `crates/skills/tests/skills.rs`
- Modify: `Cargo.toml`

- [ ] **Step 1: Write failing parser tests**

```rust
use std::fs;

use skills::{SkillConfig, SkillsManager};

#[tokio::test]
async fn load_from_root_parses_skill_frontmatter() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let skill_dir = temp.path().join("rust-error-snafu");
    fs::create_dir_all(&skill_dir).expect("skill dir should be created");
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: rust-error-snafu\ndescription: Typed Rust errors.\n---\nBody\n",
    )
    .expect("skill file should be written");

    let manager = SkillsManager::new(SkillConfig {
        roots: vec![temp.path().to_path_buf()],
        cwd: None,
        enabled: true,
    });

    let outcome = manager.load().await;

    assert!(outcome.errors.is_empty());
    assert_eq!(outcome.skills.len(), 1);
    assert_eq!(outcome.skills[0].name, "rust-error-snafu");
    assert_eq!(outcome.skills[0].description, "Typed Rust errors.");
    assert_eq!(outcome.skills[0].path, skill_dir.join("SKILL.md"));
}

#[tokio::test]
async fn invalid_skill_records_load_error_without_failing_root() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let invalid_dir = temp.path().join("invalid");
    let valid_dir = temp.path().join("valid");
    fs::create_dir_all(&invalid_dir).expect("invalid dir should be created");
    fs::create_dir_all(&valid_dir).expect("valid dir should be created");
    fs::write(invalid_dir.join("SKILL.md"), "missing frontmatter").expect("invalid file");
    fs::write(
        valid_dir.join("SKILL.md"),
        "---\nname: valid\ndescription: Valid skill.\n---\nBody\n",
    )
    .expect("valid file");

    let outcome = SkillsManager::new(SkillConfig {
        roots: vec![temp.path().to_path_buf()],
        cwd: None,
        enabled: true,
    })
    .load()
    .await;

    assert_eq!(outcome.skills.len(), 1);
    assert_eq!(outcome.skills[0].name, "valid");
    assert_eq!(outcome.errors.len(), 1);
    assert!(outcome.errors[0].message.contains("missing YAML frontmatter"));
}
```

- [ ] **Step 2: Run tests to verify RED**

Run: `rtk cargo test -p skills load_from_root_parses_skill_frontmatter invalid_skill_records_load_error_without_failing_root`

Expected: FAIL because package `skills` does not exist.

- [ ] **Step 3: Implement minimal parser and loader**

Implement:

```rust
pub struct SkillsManager {
    config: SkillConfig,
}

impl SkillsManager {
    pub fn new(config: SkillConfig) -> Self;
    pub async fn load(&self) -> SkillLoadOutcome;
}
```

Loader behavior:

- Return empty outcome when `enabled` is false.
- Recursively scan roots plus `cwd/.agents/skills`.
- Ignore hidden paths.
- Parse `SKILL.md` frontmatter delimited by `---`.
- Require non-empty `name` and `description`.
- Store per-file errors in `SkillLoadOutcome.errors`.

- [ ] **Step 4: Run tests to verify GREEN**

Run: `rtk cargo test -p skills load_from_root_parses_skill_frontmatter invalid_skill_records_load_error_without_failing_root`

Expected: PASS.

## Task 2: Add Rendering And Explicit Mention Matching

**Files:**
- Create: `crates/skills/src/render.rs`
- Create: `crates/skills/src/mentions.rs`
- Modify: `crates/skills/src/lib.rs`
- Modify: `crates/skills/tests/skills.rs`

- [ ] **Step 1: Write failing render and mention tests**

```rust
use skills::{SkillMetadata, collect_explicit_skill_mentions, render_skills_section};

#[test]
fn render_skills_section_lists_available_skills() {
    let skill = SkillMetadata {
        name: "rust-error-snafu".to_string(),
        description: "Typed Rust errors.".to_string(),
        path: "/tmp/skills/rust-error-snafu/SKILL.md".into(),
    };

    let rendered = render_skills_section(&[skill]).expect("section should render");

    assert!(rendered.contains("## Skills"));
    assert!(rendered.contains("- rust-error-snafu: Typed Rust errors."));
    assert!(rendered.contains("/tmp/skills/rust-error-snafu/SKILL.md"));
}

#[test]
fn explicit_mentions_select_unique_matching_skills() {
    let skill = SkillMetadata {
        name: "rust-error-snafu".to_string(),
        description: "Typed Rust errors.".to_string(),
        path: "/tmp/skills/rust-error-snafu/SKILL.md".into(),
    };
    let other = SkillMetadata {
        name: "other".to_string(),
        description: "Other skill.".to_string(),
        path: "/tmp/skills/other/SKILL.md".into(),
    };

    let selected = collect_explicit_skill_mentions(
        "Use $rust-error-snafu and [$rust-error-snafu](skill:///tmp/skills/rust-error-snafu/SKILL.md)",
        &[skill.clone(), other],
    );

    assert_eq!(selected, vec![skill]);
}
```

- [ ] **Step 2: Run tests to verify RED**

Run: `rtk cargo test -p skills render_skills_section_lists_available_skills explicit_mentions_select_unique_matching_skills`

Expected: FAIL because rendering and mention functions are missing.

- [ ] **Step 3: Implement rendering and matching**

Implement:

```rust
pub fn render_skills_section(skills: &[SkillMetadata]) -> Option<String>;
pub fn collect_explicit_skill_mentions(text: &str, skills: &[SkillMetadata]) -> Vec<SkillMetadata>;
```

Matching rules:

- `$name` matches a skill by exact name.
- `[$name](skill:///abs/path/SKILL.md)` matches by normalized absolute path after stripping `skill://`.
- Preserve skill list order.
- Deduplicate by path.
- Ignore unknown mentions.

- [ ] **Step 4: Run tests to verify GREEN**

Run: `rtk cargo test -p skills render_skills_section_lists_available_skills explicit_mentions_select_unique_matching_skills`

Expected: PASS.

## Task 3: Add Skill Instruction Injection

**Files:**
- Create: `crates/skills/src/injection.rs`
- Modify: `crates/skills/src/lib.rs`
- Modify: `crates/skills/tests/skills.rs`

- [ ] **Step 1: Write failing injection test**

```rust
use std::fs;

use skills::{SkillMetadata, build_skill_injections};

#[tokio::test]
async fn build_skill_injections_wraps_selected_skill_contents() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let skill_path = temp.path().join("SKILL.md");
    fs::write(
        &skill_path,
        "---\nname: rust-error-snafu\ndescription: Typed Rust errors.\n---\nUse SNAFU context.\n",
    )
    .expect("skill file should be written");

    let messages = build_skill_injections(&[SkillMetadata {
        name: "rust-error-snafu".to_string(),
        description: "Typed Rust errors.".to_string(),
        path: skill_path.clone(),
    }])
    .await
    .expect("injection should succeed");

    assert_eq!(messages.len(), 1);
    assert!(messages[0].content().contains("<skill_instructions"));
    assert!(messages[0].content().contains("Use SNAFU context."));
    assert!(messages[0].content().contains(skill_path.to_string_lossy().as_ref()));
}
```

- [ ] **Step 2: Run test to verify RED**

Run: `rtk cargo test -p skills build_skill_injections_wraps_selected_skill_contents`

Expected: FAIL because `build_skill_injections` is missing.

- [ ] **Step 3: Implement injection**

Implement:

```rust
pub async fn build_skill_injections(skills: &[SkillMetadata]) -> Result<Vec<llm::completion::Message>>;
```

Each selected skill becomes a `Message::user` containing `<skill_instructions name="..." path="...">` tags and the full `SKILL.md` body.

- [ ] **Step 4: Run test to verify GREEN**

Run: `rtk cargo test -p skills build_skill_injections_wraps_selected_skill_contents`

Expected: PASS.

## Task 4: Wire Skills Into Kernel Runtime

**Files:**
- Modify: `crates/kernel/Cargo.toml`
- Modify: `crates/kernel/src/error.rs`
- Modify: `crates/kernel/src/runtime/continuation/config.rs`
- Modify: `crates/kernel/src/runtime/turn/runner.rs`
- Modify: `crates/kernel/tests/agent_loop.rs`

- [ ] **Step 1: Write failing kernel integration tests**

Add tests that:

- Create a temp skill root with `rust-error-snafu/SKILL.md`.
- Configure `AgentLoopConfig` with `skills::SkillConfig { roots: vec![root], cwd: None, enabled: true }`.
- Run a prompt containing `$rust-error-snafu`.
- Assert the recorded `ModelRequest.system_prompt` contains the available skills list.
- Assert `ModelRequest.messages` contains a skill instruction message before the final user prompt.
- Run another prompt without the mention and assert the skill body is not injected.

- [ ] **Step 2: Run tests to verify RED**

Run: `rtk cargo test -p kernel skill`

Expected: FAIL because `AgentLoopConfig` has no skill config and runtime does not inject skills.

- [ ] **Step 3: Implement runtime integration**

Implementation shape:

```rust
#[derive(Debug, Clone, Default)]
pub struct AgentLoopConfig {
    pub skills: skills::SkillConfig,
    // existing fields stay unchanged
}
```

In `run_persisted_turn`:

```rust
let outcome = skills::SkillsManager::new(config.skills.clone()).load().await;
let system_prompt = merge_skill_prompt(system_prompt, &outcome.skills);
let selected = skills::collect_explicit_skill_mentions(&request.input, &outcome.skills);
let injections = skills::build_skill_injections(&selected).await?;
history.extend(injections);
history.push(user_message);
```

Add `Error::Skills { source: skills::Error, stage: String }` and use SNAFU context when converting injection failures.

- [ ] **Step 4: Run tests to verify GREEN**

Run: `rtk cargo test -p kernel skill`

Expected: PASS.

## Task 5: Verification And Cleanup

**Files:**
- Modify only files touched by prior tasks if verification exposes issues.

- [ ] **Step 1: Run crate tests**

Run: `rtk cargo test -p skills`

Expected: PASS.

- [ ] **Step 2: Run kernel tests**

Run: `rtk cargo test -p kernel`

Expected: PASS.

- [ ] **Step 3: Run workspace check**

Run: `rtk cargo check --workspace`

Expected: PASS.

- [ ] **Step 4: Commit implementation**

```bash
rtk git add Cargo.toml Cargo.lock crates/skills crates/kernel/Cargo.toml crates/kernel/src/error.rs crates/kernel/src/runtime/continuation/config.rs crates/kernel/src/runtime/turn/runner.rs crates/kernel/tests/agent_loop.rs
rtk git commit -m "feat: add explicit skill support"
```

Expected: commit succeeds with only the skill implementation files staged.

## Self-Review

- Spec coverage: the plan creates `crates/skills`, parses `SKILL.md`, discovers roots and `.agents/skills`, renders available skills, matches explicit mentions, injects skill contents, and adds crate/kernel tests.
- Scope check: bundled skills, implicit invocation, plugin namespaces, disable rules, env dependency prompting, and UI are intentionally excluded.
- Placeholder scan: no `TBD`, `TODO`, or unspecified implementation-only steps remain.
- Type consistency: the plan consistently uses `SkillConfig`, `SkillsManager`, `SkillMetadata`, `SkillLoadOutcome`, `render_skills_section`, `collect_explicit_skill_mentions`, and `build_skill_injections`.
