# Global Rules

## Mandatory

1. All newly added or modified code must include comments for non-trivial logic, and every newly added function must include a function-level comment. All comments must be written in English.
2. Do not run `git commit` without the user's explicit permission.
3. Before running `git commit`, run and check the relevant `pre-commit` hooks.
4. Every commit message must follow the template and constraints defined in `.gitmessage`.
5. Structs with more than 3 fields must use `typed-builder` and be constructed via the builder pattern. `Option` fields must be annotated with `#[builder(default, setter(strip_option))]` so callers pass `value` instead of `Some(value)`.
6. When cloning a field wrapped in `Arc<T>`, use the explicit form `Arc::clone(&self.field)` instead of `self.field.clone()`. This makes it obvious at the call site that only the reference count is being incremented, not a deep copy.

@/home/isbest/.codex/RTK.md
