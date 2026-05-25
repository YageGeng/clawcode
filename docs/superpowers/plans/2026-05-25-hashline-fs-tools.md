# Hashline FS Tools Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add hashline-backed filesystem tools that can replace the current built-in fs tools at registration time while preserving the legacy implementation as an alternate tool set.

**Architecture:** Move existing fs tools under `fs::legacy` without behavior changes, then add `fs::hashline` with pure hashline formatting/editing logic and model-visible `read_file`, `write_file`, and `edit_file` tools. Registration selects either legacy or hashline; hashline edit emits final `FileChangeItem` via `execute_streaming()` but does not provide `arguments_consumer()`.

**Tech Stack:** Rust 2024 workspace, `async_trait`, `tokio`, `serde`, `typed-builder`, `protocol::FileChangeItem`, `xxhash-rust` for xxHash32 compatibility.

---

## File Map

- Modify `Cargo.toml`: add workspace dependency for `xxhash-rust`.
- Modify `crates/tools/Cargo.toml`: depend on `xxhash-rust`.
- Modify `crates/tools/src/builtin/fs/mod.rs`: expose `legacy` and `hashline`, add `FsToolSet`, add selected registration methods.
- Move `crates/tools/src/builtin/fs/read.rs` to `crates/tools/src/builtin/fs/legacy/read.rs`.
- Move `crates/tools/src/builtin/fs/write.rs` to `crates/tools/src/builtin/fs/legacy/write.rs`.
- Move `crates/tools/src/builtin/fs/edit.rs` to `crates/tools/src/builtin/fs/legacy/edit.rs`.
- Move `crates/tools/src/builtin/fs/apply_patch/` to `crates/tools/src/builtin/fs/legacy/apply_patch/`.
- Create `crates/tools/src/builtin/fs/hashline/format.rs`: xxHash32 line hash, line refs, hashline formatting.
- Create `crates/tools/src/builtin/fs/hashline/edit.rs`: edit model, edit application, `HashlineEditFile`.
- Create `crates/tools/src/builtin/fs/hashline/read.rs`: hashline `read_file`.
- Create `crates/tools/src/builtin/fs/hashline/write.rs`: hashline `write_file`.
- Create `crates/tools/src/builtin/fs/hashline/mod.rs`: module exports and tool-set registration helper.
- Modify tests in moved files only for import paths if needed.

## Tasks

### Task 1: Branch and Legacy Module Move

**Files:**
- Modify: `crates/tools/src/builtin/fs/mod.rs`
- Move: current fs tool files into `crates/tools/src/builtin/fs/legacy/`

- [ ] **Step 1: Move files without changing logic**

Run:

```bash
mkdir -p crates/tools/src/builtin/fs/legacy
git mv crates/tools/src/builtin/fs/read.rs crates/tools/src/builtin/fs/legacy/read.rs
git mv crates/tools/src/builtin/fs/write.rs crates/tools/src/builtin/fs/legacy/write.rs
git mv crates/tools/src/builtin/fs/edit.rs crates/tools/src/builtin/fs/legacy/edit.rs
git mv crates/tools/src/builtin/fs/apply_patch crates/tools/src/builtin/fs/legacy/apply_patch
```

- [ ] **Step 2: Update `fs/mod.rs` module paths**

Keep `register_fs_tools()` defaulting to legacy. Add `legacy` module and route existing registrations through it.

- [ ] **Step 3: Run legacy tests**

Run:

```bash
rtk cargo test -p tools builtin::fs -- --nocapture
```

Expected: existing fs tests pass or fail only on import paths, which must be fixed before proceeding.

### Task 2: Selected FS Tool Registration

**Files:**
- Modify: `crates/tools/src/builtin/fs/mod.rs`
- Modify: `crates/tools/src/builtin/mod.rs`

- [ ] **Step 1: Write failing registration tests**

Add tests showing `FsToolSet::Legacy` keeps current `apply_patch`/`edit` behavior and `FsToolSet::Hashline` will later register `edit_file`.

- [ ] **Step 2: Implement `FsToolSet` and selected registration methods**

Add:

```rust
pub enum FsToolSet {
    Legacy,
    Hashline,
}
```

Add `register_fs_tools_with_set()` and `register_fs_tools_with_backend_and_set()`.

- [ ] **Step 3: Run registration tests**

Run:

```bash
rtk cargo test -p tools register_fs_tools -- --nocapture
```

Expected: legacy registration tests pass; hashline registration can only pass after hashline stub tools exist.

### Task 3: Hashline Formatting and xxHash32

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/tools/Cargo.toml`
- Create: `crates/tools/src/builtin/fs/hashline/format.rs`
- Create: `crates/tools/src/builtin/fs/hashline/mod.rs`

- [ ] **Step 1: Add failing tests for hash vectors and parsing**

Tests must cover whitespace-insensitive hashes, `\r` stripping, `LINE:HASH` parsing, copied `LINE:HASH|content` parsing, and formatted output with `start_line`.

- [ ] **Step 2: Add `xxhash-rust` dependency**

Use `xxhash-rust` with the `xxh32` feature.

- [ ] **Step 3: Implement formatting primitives**

Implement `LineRef`, `compute_line_hash()`, `format_hash_lines()`, and `LineRef::try_from(&str)`.

- [ ] **Step 4: Run format tests**

Run:

```bash
rtk cargo test -p tools hashline::format -- --nocapture
```

Expected: all format tests pass.

### Task 4: Hashline Read and Write Tools

**Files:**
- Create: `crates/tools/src/builtin/fs/hashline/read.rs`
- Create: `crates/tools/src/builtin/fs/hashline/write.rs`
- Modify: `crates/tools/src/builtin/fs/hashline/mod.rs`

- [ ] **Step 1: Write failing read/write tests**

Tests must cover hashline read header, 1-indexed offset, default 2000-line limit, `plain` mode, injected backend use, and write backend delegation.

- [ ] **Step 2: Implement `HashlineReadFile`**

Use `FsBackend::read_text_file()` with `offset = 0` and no `limit` so hashline read can compute total line count and slice using 1-indexed semantics.

- [ ] **Step 3: Implement `HashlineWriteFile`**

Delegate to `FsBackend::write_text_file()` and keep approval behavior equivalent to legacy write.

- [ ] **Step 4: Run read/write tests**

Run:

```bash
rtk cargo test -p tools hashline::read hashline::write -- --nocapture
```

Expected: all read/write tests pass.

### Task 5: Hashline Edit Model and Application

**Files:**
- Create: `crates/tools/src/builtin/fs/hashline/edit.rs`
- Modify: `crates/tools/src/builtin/fs/hashline/mod.rs`

- [ ] **Step 1: Write failing edit application tests**

Tests must cover `set_line`, `replace_lines`, `insert_after`, deletion, expansion, empty insert rejection, start/end validation, mismatch diagnostics, unique relocation, duplicate-hash mismatch, prefix stripping, echo stripping, duplicate edit deduplication, bottom-up multi-edit, blank-line preservation, indentation restoration, and no-op diagnostics.

- [ ] **Step 2: Implement edit model types**

Use serde enums/structs that match the JSON shape exactly: `set_line`, `replace_lines`, and `insert_after`.

- [ ] **Step 3: Implement pure edit application**

Implement parsing, validation, relocation, deduplication, bottom-up application, prefix stripping, echo stripping, indentation restoration, warnings, and no-op tracking.

- [ ] **Step 4: Run edit application tests**

Run:

```bash
rtk cargo test -p tools hashline::edit -- --nocapture
```

Expected: pure edit tests pass.

### Task 6: Hashline Edit Tool and Final Streaming Diff

**Files:**
- Modify: `crates/tools/src/builtin/fs/hashline/edit.rs`

- [ ] **Step 1: Write failing tool tests**

Tests must cover reading from backend, writing changed content, returning model text, rejecting no-op without writing, `arguments_consumer().is_none()`, and `execute_streaming()` emitting final `FileChangeItem` with complete old/new text.

- [ ] **Step 2: Implement `HashlineEditFile`**

`execute()` should call a shared `do_edit()` and return the model text. `execute_streaming()` should call the same method and emit `Begin`/`End` file-change lifecycle items.

- [ ] **Step 3: Run tool tests**

Run:

```bash
rtk cargo test -p tools hashline_edit -- --nocapture
```

Expected: hashline edit tool tests pass.

### Task 7: Registry Integration and Full Tools Verification

**Files:**
- Modify: `crates/tools/src/builtin/fs/mod.rs`
- Modify: `crates/tools/src/builtin/mod.rs`

- [ ] **Step 1: Finish hashline registration tests**

Assert `FsToolSet::Hashline` registers `read_file`, `write_file`, and `edit_file`, and does not register `apply_patch`.

- [ ] **Step 2: Wire `hashline::register()` into selected registration**

Make `register_fs_tools_with_backend_and_set(..., FsToolSet::Hashline)` register the hashline tool set.

- [ ] **Step 3: Run integration tests**

Run:

```bash
rtk cargo test -p tools register_fs_tools hashline -- --nocapture
```

Expected: selected registration and hashline tool tests pass.

### Task 8: Formatting and Final Verification

**Files:**
- All modified files.

- [ ] **Step 1: Format**

Run:

```bash
rtk cargo fmt --all -- --check
```

If it fails, run `rtk cargo fmt --all`, then rerun the check.

- [ ] **Step 2: Run targeted tools tests**

Run:

```bash
rtk cargo test -p tools
```

Expected: all tools tests pass.

- [ ] **Step 3: Run clippy for touched crate**

Run:

```bash
rtk cargo clippy -p tools --all-targets --locked -- -D warnings
```

Expected: no warnings. If dependency lockfile update is needed, run the same command without `--locked` only after confirming the new dependency requires lockfile refresh.

- [ ] **Step 4: Inspect diff**

Run:

```bash
rtk git diff --stat
rtk git diff
```

Expected: diff is scoped to fs tool restructuring, hashline implementation, dependency metadata, spec, and plan.
