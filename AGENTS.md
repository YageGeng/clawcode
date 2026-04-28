# Repository Guidelines

## Project Structure & Module Organization

This is a Rust workspace.

- `Cargo.toml`: workspace manifest and shared dependencies.
- `crates/*`: core crates:
  - `cli`: command-line entrypoint
  - `kernel`: orchestration/runtime
  - `tools`: tool registry and built-ins
  - `llm`: model providers and completion adapters
  - `acp`: ACP protocol types
  - `skills`: skill integration
- `docs/`: design docs and notes.
- `target/`: build artifacts (generated).

### Layering expectations

- Keep changes aligned to clear module boundaries:
  - `kernel`: orchestration, loop control, lifecycle.
  - `tools` / `acp` / `skills`: tooling and external contract handling.
  - `llm`: model/provider integrations.
  - `cli`: user entrypoint and argument/IO glue.
- Prefer adding behavior in the highest-appropriate layer and avoid bypassing adapters, routers, or runtime contracts for side effects.
- Do not mix cross-layer concerns in one patch (for example, avoid model protocol parsing in `cli`).

## Build, Test, and Development Commands

- `cargo build`: build all workspace crates.
- `cargo build -p cli`: build only CLI binary.
- `cargo run -p cli`: run CLI.
- `cargo test`: run all tests.
- `cargo test -p kernel` (or another crate name): run crate-level tests.
- `cargo clean`: clear build artifacts.

## Coding Style & Naming Conventions

- Use Rust 2021 idioms with `rustfmt` formatting.
- Indentation: 4 spaces, one statement per line when readable.
- Naming:
  - `snake_case` for vars/functions/modules.
  - `PascalCase` for types/structs/enums.
  - `SCREAMING_SNAKE_CASE` for constants.
- Error handling is implemented with `snafu` across project errors.
- Keep changes small and scoped; prefer clear helper methods over deeply nested closures.

## Testing Guidelines

- Tests use Rust built-in test harness with `tokio::test` for async code.
- Typical test file patterns: `crates/*/src/.../tests/*.rs` and inline `mod tests`.
- Use descriptive test names like `feature_path_or_condition_does_expected_thing`.
- Prefer adding regression tests for behavior changes (especially tool execution and model-adapter paths).

## Commit & Pull Request Guidelines

- Use imperative commit messages and keep them short; existing history uses prefixes such as:
  - `feat: ...`
  - `fix: ...`
  - `fix: ...` for bug fixes.
- PRs should include:
  - Summary of user-visible behavior change.
  - Commands run (`cargo test`, `cargo build`, etc.).
  - Any manual verification done in terminal.
- If project-facing behavior, module layout, build/run commands, or public usage changed, verify both `README.md` and `README_CN.md` are still accurate; update them as part of the same changeset.
- Before creating a commit, obtain explicit approval for the specific changeset.
- If approval is not given, postpone commit and provide a change summary only.

## Runtime and Tooling Notes

- The default runtime includes deepthink/tool-result compatibility changes; when touching model/request conversion, verify provider conversion and tool-result IDs remain aligned.
- For shell/tool failures, preserve user-facing text and keep internal detail in structured error context.
- Keep edits compatible with existing pre-commit hooks (`fmt`, `clippy -Dwarnings`).
