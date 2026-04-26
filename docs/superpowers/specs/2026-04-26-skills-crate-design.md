# Skills Crate Design

## Goal

Add first-class skill support to the current Rust workspace by introducing a reusable `skills` crate and wiring explicit skill invocation into the kernel runtime.

The initial scope is intentionally smaller than upstream Codex. It should deliver a usable loop: discover local `SKILL.md` files, expose available skills to the model, detect explicit user mentions, and inject selected skill instructions into the turn context.

## Context

Upstream Codex splits skill support across two crates:

- `codex-skills` installs bundled system skills into `CODEX_HOME/skills/.system`.
- `codex-core-skills` scans skill roots, parses metadata, resolves config rules, detects mentions, renders the available skills section, and injects skill contents.

This workspace is smaller and currently has `llm`, `tools`, `kernel`, and `cli` crates. The runtime already carries a system prompt through `ThreadHandle` and turn execution, so the lowest-risk integration point is to enrich the prompt and turn messages before the model request is built.

## Scope

Implement path B:

- Create `crates/skills`.
- Parse `SKILL.md` files with YAML frontmatter containing `name` and `description`.
- Discover skills from configured root directories and repo-local `.agents/skills`.
- Render a system-prompt section listing available skills.
- Detect explicit mentions in user text using `$skill-name` and linked skill mentions such as `[$skill-name](skill:///abs/path/SKILL.md)`.
- Inject matched `SKILL.md` contents into the current turn before the model sees the user request.
- Add tests for parsing, discovery, mention matching, rendering, injection, and kernel integration.

Out of scope for this iteration:

- Bundled/system skill installation cache.
- Implicit invocation based on edited scripts or files.
- Plugin namespace resolution.
- Skill enable/disable config rules.
- Environment variable dependency prompting.
- UI for skill selection.

## Crate API

The `skills` crate will expose small, stable building blocks:

- `SkillMetadata`: name, description, path to `SKILL.md`.
- `SkillError` and `Result<T>` using SNAFU for structured errors.
- `SkillLoadOutcome`: loaded skills plus non-fatal parse/load errors.
- `SkillsManager`: discovers skills from roots and optional cwd.
- `render_skills_section(skills: &[SkillMetadata]) -> Option<String>`.
- `collect_explicit_skill_mentions(text: &str, skills: &[SkillMetadata]) -> Vec<SkillMetadata>`.
- `build_skill_injections(skills: &[SkillMetadata]) -> Result<Vec<Message>>`.

Every public function and all non-trivial logic will include comments, matching the repository rule.

## Discovery

`SkillsManager` will accept:

- Explicit roots from CLI/runtime configuration.
- The current working directory for repo skill discovery.

Discovery rules:

- Search explicit roots directly.
- Search `.agents/skills` under the current working directory.
- Recursively scan directories up to a conservative depth limit.
- Ignore hidden entries.
- Treat each `SKILL.md` as one skill.
- Preserve non-fatal errors in `SkillLoadOutcome.errors` instead of failing the entire load.

This is enough for repo-local and user-provided skills without requiring the full Codex config stack.

## Injection Flow

At runtime startup or turn execution:

1. Load skills for the current cwd.
2. Render available skills into the system prompt so the model knows what exists and how to use them.
3. Scan the current user prompt for explicit skill mentions.
4. Read each selected `SKILL.md`.
5. Add the skill contents as prompt-visible context before the user message.

The injected message will be structured with clear tags, for example:

```text
<skill_instructions name="rust-error-snafu" path="/abs/path/SKILL.md">
...
</skill_instructions>
```

This keeps skill content distinct from normal user text and is simple to test.

## Kernel Integration

`kernel` will depend on `skills`.

`ThreadConfig` or `AgentLoopConfig` will gain a small optional skill configuration:

- enabled flag, default `true`.
- extra roots, default empty.
- optional cwd, falling back to process cwd when absent.

Before a turn enters the model loop, the runtime will build a skill-aware system prompt and skill injection messages. Existing tests that use plain prompts should continue to pass because no skills are found by default in temporary test contexts.

## Error Handling

All new Rust error handling will use SNAFU.

Parsing errors for one skill are recorded as `SkillError` entries. Hard failures are reserved for operations that are required to produce injection messages after a skill has already been selected, such as failing to read a selected `SKILL.md`.

## Testing

Add unit tests in `crates/skills`:

- Valid frontmatter parses into metadata.
- Missing or invalid frontmatter records a load error.
- Discovery finds nested `SKILL.md` files.
- Mention matching handles `$skill-name`, linked `skill://` paths, duplicates, and unknown names.
- Rendering omits empty skill lists and includes name, description, and path.
- Injection wraps selected skill contents.

Add kernel integration coverage:

- A runtime turn with a skill mention sends injected skill instructions to the model.
- A turn without a skill mention does not inject skill contents.
- Available skills are appended to the system prompt when discovered.

## Risks

The main risk is coupling skill loading too deeply into the runtime. Keeping `skills` independent and using plain kernel configuration avoids locking the implementation to CLI-only behavior.

Another risk is excessive prompt growth. This iteration only injects explicitly mentioned skills, so prompt cost stays predictable.

## Acceptance Criteria

- `cargo test -p skills` passes.
- Relevant kernel tests pass.
- Existing workspace tests continue to compile.
- Explicitly mentioning a discovered skill causes the corresponding `SKILL.md` content to be visible in the model request.
- Not mentioning a skill only exposes the available skills list, not full skill bodies.
