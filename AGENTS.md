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

<!-- rtk-instructions v2 -->
# RTK (Rust Token Killer) - Token-Optimized Commands

## Golden Rule

**Always prefix commands with `rtk`**. If RTK has a dedicated filter, it uses it. If not, it passes through unchanged. This means RTK is always safe to use.

**Important**: Even in command chains with `&&`, use `rtk`:
```bash
# ❌ Wrong
git add . && git commit -m "msg" && git push

# ✅ Correct
rtk git add . && rtk git commit -m "msg" && rtk git push
```

## RTK Commands by Workflow

### Build & Compile (80-90% savings)
```bash
rtk cargo build         # Cargo build output
rtk cargo check         # Cargo check output
rtk cargo clippy        # Clippy warnings grouped by file (80%)
rtk tsc                 # TypeScript errors grouped by file/code (83%)
rtk lint                # ESLint/Biome violations grouped (84%)
rtk prettier --check    # Files needing format only (70%)
rtk next build          # Next.js build with route metrics (87%)
```

### Test (60-99% savings)
```bash
rtk cargo test          # Cargo test failures only (90%)
rtk go test             # Go test failures only (90%)
rtk jest                # Jest failures only (99.5%)
rtk vitest              # Vitest failures only (99.5%)
rtk playwright test     # Playwright failures only (94%)
rtk pytest              # Python test failures only (90%)
rtk rake test           # Ruby test failures only (90%)
rtk rspec               # RSpec test failures only (60%)
rtk test <cmd>          # Generic test wrapper - failures only
```

### Git (59-80% savings)
```bash
rtk git status          # Compact status
rtk git log             # Compact log (works with all git flags)
rtk git diff            # Compact diff (80%)
rtk git show            # Compact show (80%)
rtk git add             # Ultra-compact confirmations (59%)
rtk git commit          # Ultra-compact confirmations (59%)
rtk git push            # Ultra-compact confirmations
rtk git pull            # Ultra-compact confirmations
rtk git branch          # Compact branch list
rtk git fetch           # Compact fetch
rtk git stash           # Compact stash
rtk git worktree        # Compact worktree
```

Note: Git passthrough works for ALL subcommands, even those not explicitly listed.

### GitHub (26-87% savings)
```bash
rtk gh pr view <num>    # Compact PR view (87%)
rtk gh pr checks        # Compact PR checks (79%)
rtk gh run list         # Compact workflow runs (82%)
rtk gh issue list       # Compact issue list (80%)
rtk gh api              # Compact API responses (26%)
```

### JavaScript/TypeScript Tooling (70-90% savings)
```bash
rtk pnpm list           # Compact dependency tree (70%)
rtk pnpm outdated       # Compact outdated packages (80%)
rtk pnpm install        # Compact install output (90%)
rtk npm run <script>    # Compact npm script output
rtk npx <cmd>           # Compact npx command output
rtk prisma              # Prisma without ASCII art (88%)
rtk uv run <cmd>        # Compact uv project command output
```

### Files & Search (60-75% savings)
```bash
rtk ls <path>           # Tree format, compact (65%)
rtk read <file>         # Code reading with filtering (60%)
rtk grep <pattern>      # Search grouped by file (75%). Format flags (-c, -l, -L, -o, -Z) run raw.
rtk find <pattern>      # Find grouped by directory (70%)
```

### Analysis & Debug (70-90% savings)
```bash
rtk err <cmd>           # Filter errors only from any command
rtk log <file>          # Deduplicated logs with counts
rtk json <file>         # JSON structure without values
rtk deps                # Dependency overview
rtk env                 # Environment variables compact
rtk summary <cmd>       # Smart summary of command output
rtk diff                # Ultra-compact diffs
```

### Infrastructure (85% savings)
```bash
rtk docker ps           # Compact container list
rtk docker images       # Compact image list
rtk docker logs <c>     # Deduplicated logs
rtk kubectl get         # Compact resource list
rtk kubectl logs        # Deduplicated pod logs
```

### Network (65-70% savings)
```bash
rtk curl <url>          # Compact HTTP responses (70%)
rtk wget <url>          # Compact download output (65%)
```

### Meta Commands
```bash
rtk gain                # View token savings statistics
rtk gain --history      # View command history with savings
rtk discover            # Analyze Claude Code sessions for missed RTK usage
rtk proxy <cmd>         # Run command without filtering (for debugging)
rtk init                # Add RTK instructions to CLAUDE.md
rtk init --global       # Add RTK to ~/.claude/CLAUDE.md
```

## Token Savings Overview

| Category | Commands | Typical Savings |
|----------|----------|-----------------|
| Tests | vitest, playwright, cargo test | 90-99% |
| Build | next, tsc, lint, prettier | 70-87% |
| Git | status, log, diff, add, commit | 59-80% |
| GitHub | gh pr, gh run, gh issue | 26-87% |
| Package Managers | pnpm, npm, npx | 70-90% |
| Files | ls, read, grep, find | 60-75% |
| Infrastructure | docker, kubectl | 85% |
| Network | curl, wget | 65-70% |

Overall average: **60-90% token reduction** on common development operations.
<!-- /rtk-instructions -->
