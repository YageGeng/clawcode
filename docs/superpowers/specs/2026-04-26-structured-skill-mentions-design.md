# Structured Skill Mentions Design

## Goal

Refactor skill mention handling to match Codex's full explicit mention semantics by introducing structured user inputs in the kernel and replacing the current text-only skill mention API.

## Context

The current implementation supports only text-based mention matching:

- `$skill-name`
- `[$skill-name](skill:///abs/path/SKILL.md)`

This is useful but incomplete. Codex supports richer explicit selection semantics where structured `UserInput::Skill { name, path }` is resolved first, text mentions are resolved second, linked paths override ambiguous names, disabled paths block selection, connector name conflicts suppress plain-name matches, and common environment variables are ignored.

The current workspace does not have a structured user input pipeline, so implementing the full rule set requires a deliberate kernel-level input model change rather than adding more ad hoc parsing to `collect_explicit_skill_mentions(text, skills)`.

## Scope

Implement the large refactor path:

- Introduce a kernel-owned `UserInput` type with at least `Text` and `Skill` variants.
- Change `ThreadRunRequest` and `RunRequest` to carry `Vec<UserInput>` instead of only a plain `String`.
- Remove the old text-only `collect_explicit_skill_mentions(text, skills)` API.
- Add a full mention collector that accepts structured inputs, loaded skills, disabled paths, and connector slug counts.
- Preserve existing CLI behavior by wrapping command-line prompt text into `UserInput::Text`.
- Preserve model-facing user text behavior for existing callers that only send text.

Out of scope:

- UI picker support.
- App/connector discovery itself. The mention API accepts connector slug counts, but this workspace can pass an empty map until apps exist.
- Runtime skill disable config. The mention API accepts disabled paths, but this workspace can pass an empty set until skill config rules exist.
- Implicit skill invocation from shell commands.

## Data Model

Add a kernel user input type, likely under `crates/kernel/src/input.rs`:

```rust
pub enum UserInput {
    Text { text: String },
    Skill { name: String, path: PathBuf },
}
```

Provide helpers:

- `UserInput::text(...)`
- `UserInput::skill(name, path)`
- `user_inputs_display_text(&[UserInput]) -> String`
- `user_inputs_to_messages(&[UserInput]) -> Vec<Message>`

`Skill` inputs do not become ordinary model messages. They are only selection metadata. This mirrors Codex, where tool bodies are injected later in core rather than included in normal user content.

## Runtime Flow

One turn will execute as follows:

1. Load skills from configured roots.
2. Render available skills into the system prompt.
3. Collect explicit skill mentions from all structured inputs.
4. Build skill instruction messages for selected skills.
5. Convert text inputs into normal user messages.
6. Append skill instruction messages before normal user messages.
7. Persist a display string for turn history and events.

For existing single-prompt callers, `ThreadRunRequest::new("...")` can build `vec![UserInput::Text { text }]` internally. This preserves external ergonomics while removing the old skills crate API.

## Full Mention Rules

The new collector will implement the Codex rules:

- Structured `UserInput::Skill { name, path }` is processed before text.
- Structured skill inputs match loaded skills by exact path.
- Invalid, disabled, missing, or duplicate structured skill selections are skipped.
- Any structured skill name is added to `blocked_plain_names`, so a failed structured path cannot fall back to `$name`.
- Text mentions parse both plain `$name` and linked `[$name](path)` mentions.
- Linked paths are selected before plain names.
- Linked path mentions normalize `skill://` paths and also accept direct paths whose basename is `SKILL.md`.
- Linked path mentions are skipped when the resolved path is disabled.
- Plain `$name` matches only when exactly one enabled skill has that name.
- Plain `$name` is skipped when the lowercased name conflicts with a connector/app slug count.
- Selection preserves loaded skill order.
- Selection deduplicates by skill path.
- Common shell environment variable names are ignored: `PATH`, `HOME`, `USER`, `SHELL`, `PWD`, `TMPDIR`, `TEMP`, `TMP`, `LANG`, `TERM`, and `XDG_CONFIG_HOME`.
- Mention name characters are ASCII letters, digits, `_`, `-`, and `:`.

## Skills Crate API

Replace the current text-only API with:

```rust
pub struct SkillMentionOptions {
    pub disabled_paths: HashSet<PathBuf>,
    pub connector_slug_counts: HashMap<String, usize>,
}

pub fn collect_explicit_skill_mentions(
    inputs: &[UserInput],
    skills: &[SkillMetadata],
    options: &SkillMentionOptions,
) -> Vec<SkillMetadata>;
```

Because `skills` should not depend on `kernel`, define a small `SkillInput` type in `skills` and implement conversion from `kernel::UserInput` at the integration boundary. This keeps crates acyclic:

```rust
pub enum SkillInput {
    Text { text: String },
    Skill { name: String, path: PathBuf },
}
```

Kernel converts its `UserInput` values into `skills::SkillInput` only for mention collection.

## Compatibility Impact

This is intentionally not API-compatible with the old `collect_explicit_skill_mentions(text, skills)` function.

Expected call-site updates:

- `crates/kernel/src/runtime/task/api.rs`
- `crates/kernel/src/runtime/task/runner.rs`
- `crates/kernel/src/runtime/turn/runner.rs`
- `crates/cli/src/runtime.rs`
- tests that construct `RunRequest` or `ThreadRunRequest`

`ThreadRunRequest::new(text)` and `RunRequest::new(session, thread, text)` can stay as convenience constructors, but their internal storage changes to structured inputs.

## Error Handling

This refactor should not introduce new hard error paths. Existing SNAFU handling for skill injection read failures remains unchanged.

## Testing

Skills crate tests:

- Structured skill input selects by path.
- Missing structured path blocks plain `$name` fallback.
- Disabled structured path blocks plain `$name` fallback.
- Linked path mention selects even when name is ambiguous.
- Plain ambiguous name selects nothing.
- Connector slug conflict suppresses plain `$name`.
- Connector slug conflict does not suppress linked path.
- Disabled linked path selects nothing.
- `$PATH`, `$HOME`, and `$XDG_CONFIG_HOME` are ignored.
- Mention parser stops at non-name chars.
- Selection preserves loaded skill order and deduplicates by path.

Kernel tests:

- Structured `UserInput::Skill` injects the selected skill body.
- Structured `UserInput::Skill` does not become a normal user message.
- Plain text prompt still behaves as before.
- Multiple text inputs are converted to prompt-visible user messages in order.

CLI tests:

- Existing prompt string execution still constructs a text input and reaches the model.

## Risks

The main risk is broad test churn because `RunRequest` currently exposes `input: String`. The mitigation is to keep text convenience constructors and expose display helpers, so most callers can keep using `.new("text")`.

Another risk is crate coupling. Defining `skills::SkillInput` avoids a dependency cycle between `skills` and `kernel`.

## Acceptance Criteria

- `cargo test -p skills` passes.
- `cargo test -p kernel` passes.
- `cargo test -p cli` passes if the CLI crate has tests selected by Cargo.
- `cargo check --workspace` passes.
- No remaining call sites use the removed text-only skill mention collector.
- Structured skill input can trigger skill injection without `$skill-name` appearing in user text.
