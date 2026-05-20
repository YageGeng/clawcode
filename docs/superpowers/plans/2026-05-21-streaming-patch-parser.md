# Streaming Patch Parser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add generic tool arguments streaming support and implement apply_patch streaming preview using a Codex-style parser while keeping JSON `patchText` arguments.

**Architecture:** Provider argument deltas remain forwarded as `ToolCallDelta`, and kernel additionally routes them to an optional `Tool::arguments_consumer()`. `apply_patch` owns the JSON extractor and streaming parser, emits `PatchApplyUpdated`, and ACP/TUI render preview via ACP `Diff` updates before final `FileChangeItem` replaces them.

**Tech Stack:** Rust workspace, async kernel event stream, `agent-client-protocol` ACP schema, existing `typed-builder`, `futures`, `serde`.

---

## File Map

- Modify `crates/protocol/src/event.rs`: add `PatchApplyUpdated` and `ToolArgumentsStreamItem`.
- Modify `crates/protocol/src/item.rs`: add `PatchPreviewChange`.
- Modify `crates/tools/src/lib.rs`: add `ToolArgumentsConsumer` and `Tool::arguments_consumer()`.
- Move `crates/tools/src/builtin/fs/patch.rs` to `crates/tools/src/builtin/fs/apply_patch/mod.rs`.
- Create `crates/tools/src/builtin/fs/apply_patch/stream_parser.rs`: JSON extractor, streaming parser, preview consumer.
- Modify `crates/tools/src/builtin/fs/mod.rs`: point module export to `apply_patch`.
- Modify `crates/kernel/src/turn.rs`: maintain argument consumers and emit preview events.
- Modify `crates/acp/src/agent.rs`: map preview events to ACP `ToolCallUpdate` diff content.
- Modify `crates/tui/src/ui/state.rs` and `crates/tui/src/ui/cell/tool.rs`: replace diff content on updates instead of accumulating stale preview.

## Tasks

### Task 1: Protocol Types

- [ ] Add failing protocol tests for `PatchApplyUpdated` serialization and `PatchPreviewChange::Update`.
- [ ] Implement `PatchPreviewChange`, `ToolArgumentsStreamItem`, and `Event::PatchApplyUpdated`.
- [ ] Run `rtk cargo test -p protocol patch_preview -- --nocapture`.

### Task 2: Tool Arguments Consumer API

- [ ] Add failing tests that default tools return no arguments consumer and `ToolRegistry` exposes the hook indirectly through `Tool::arguments_consumer()`.
- [ ] Implement `ToolArgumentsConsumer` and default `Tool::arguments_consumer()`.
- [ ] Run `rtk cargo test -p tools arguments_consumer -- --nocapture`.

### Task 3: apply_patch Module Split

- [ ] Move `patch.rs` to `apply_patch/mod.rs` and update `fs/mod.rs`.
- [ ] Keep existing tests passing without behavior changes.
- [ ] Run `rtk cargo test -p tools apply_patch -- --nocapture`.

### Task 4: Streaming Parser and JSON Extractor

- [ ] Add failing tests for `PatchTextDeltaExtractor` split keys, escaped newlines, escaped quotes, backslashes, and unicode escapes.
- [ ] Add failing tests for `StreamingPatchParser` add/update/delete/move, CRLF, bare empty lines, and missing end marker.
- [ ] Implement extractor, parser, preview conversion, and `ApplyPatchArgumentsConsumer`.
- [ ] Run `rtk cargo test -p tools stream_parser -- --nocapture`.

### Task 5: Kernel Argument Delta Dispatch

- [ ] Add failing kernel tests for creating an apply_patch consumer on name delta and emitting `PatchApplyUpdated` on argument delta.
- [ ] Implement consumer map in `turn.rs`, route deltas, and flush on final tool call.
- [ ] Run `rtk cargo test -p kernel patch_apply_updated -- --nocapture`.

### Task 6: ACP Bridge Preview Diff

- [ ] Add failing ACP tests that `PatchApplyUpdated` becomes `ToolCallUpdate` with `ToolKind::Edit`, `InProgress`, and `ToolCallContent::Diff`.
- [ ] Implement ACP conversion helpers for `PatchPreviewChange`.
- [ ] Run `rtk cargo test -p acp patch_apply_updated -- --nocapture`.

### Task 7: TUI Diff Replacement

- [ ] Add failing TUI state tests showing repeated apply_patch diff updates replace previous diff content instead of appending.
- [ ] Implement diff replacement for ACP content updates while preserving output append behavior.
- [ ] Run `rtk cargo test -p tui tool_call_diff -- --nocapture`.

### Task 8: Full Verification

- [ ] Run `rtk cargo fmt --all -- --check`.
- [ ] Run targeted tests: `rtk cargo test -p protocol -p tools -p kernel -p acp -p tui`.
- [ ] Run `rtk cargo clippy --all-targets --all-features --locked -- -D warnings` if dependencies permit.
- [ ] Inspect `rtk git diff --stat` and `rtk git diff`.

