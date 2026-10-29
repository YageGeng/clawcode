# Global Rules

## Mandatory

1. All newly added or modified code must include comments for non-trivial logic, and every newly added function must include a function-level comment. All comments must be written in English.
2. Do not run `git commit` without the user's explicit permission.
3. Before running `git commit`, run and check the relevant `pre-commit` hooks.
4. Before every `git commit`, read `.gitmessage` and apply its template exactly.
5. Structs with more than 3 fields must use `typed-builder` and be constructed via the builder pattern. `Option` fields must be annotated with `#[builder(default)]`. Prefer `setter(strip_option)` when most call sites hold a concrete value, but omit it when callers already hold `Option<T>` and would need to wrap/unwrap.
6. When cloning a field wrapped in `Arc<T>`, use the explicit form `Arc::clone(&self.field)` instead of `self.field.clone()`. This makes it obvious at the call site that only the reference count is being incremented, not a deep copy.
7. Frontend code is exempt from test-driven development and does not require unit tests.
8. Avoid proliferating short helper functions. When a helper takes exactly one argument and that argument has a project-defined type, prefer an associated method or trait implementation on that type instead of a free helper function.
9. WebUI end-to-end tests must run against the production `app` backend and its configured provider. Do not introduce or use a fixture backend for WebUI acceptance.
10. Test code may exist only in crate-level `tests/` integration-test directories or inside modules declared exactly as `#[cfg(test)] mod tests`. Production source must not contain standalone `#[cfg(test)]` fields, functions, implementations, test-support modules, or other code added only to support tests.
11. Organize Cargo dependency tables into semantic groups matching the root `Cargo.toml`. Put relative-path workspace crates first, separate groups with one blank line and a category comment, and sort dependencies alphabetically within each group.
12. Declare every dependency path or version in the root `[workspace.dependencies]` table, including internal workspace crates. Sub-crates must inherit dependencies with `{ workspace = true }` and may add only crate-specific features, optional flags, default-feature settings, or target conditions locally.
13. Add logs only at meaningful lifecycle boundaries, retries, state transitions, and error paths. Logs must provide enough context to debug failures without recording every token, tight-loop iteration, complete prompt or response bodies, credentials, authentication headers, or other sensitive data. The only prompt-body exception is ACP request/notification parameter logging at `DEBUG`: it may preserve complete prompt, shell command, resource, and extension parameter content, but it must recursively redact credential-like fields including API keys, tokens, authorization values, cookies, passwords, and secrets. ACP `INFO` logs must not include parameters.
14. Invoke tracing event macros through their fully qualified paths, such as `tracing::info!()` and `tracing::warn!()`. Do not import tracing event macros directly or through wildcard imports. Write readable event messages with normal formatting arguments, such as `tracing::info!("completed Turn {}", turn_id)`, instead of structured event-field syntax such as `tracing::info!(turn = %turn_id, "completed Turn")`. Tracing spans may use the minimum structured fields required for inherited correlation context, including `trace_id`.

@RTK.md
