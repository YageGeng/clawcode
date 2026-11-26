# N0 feasibility record

Spec: [Desktop NSH design, section 8](../../docs/superpowers/specs/2026-11-25-desktop-nsh-design.md)

Toolchain: `nsh 0.6.1 (2f51d86)`, `cargo 1.100.0`, GTK 4.24.1, glibc 2.44 (Arch Linux).

| # | Item | Result | Evidence |
| --- | --- | --- | --- |
| 1 | Import `packages/client` from outside the app root | Pass (no longer needed) | `App.tsx` imported `../../packages/client/src/probe`; `nsh check`, `nsh build` and `nsh test` passed with no `tsconfig` changes. After the spec revision the client code lives in `src/client`, inside the app |
| 2 | Rust extension depending on repository crates | Pass | `ext/backend` is a root workspace member; it inherited `protocol = { workspace = true }`, called `ProductIdentity::NAME`, and the value reached the headless test |
| 3 | Native module frame throughput | Pass | A thread posted 2500 ACP-shaped frames at 500/s through a channel and an engine task (`Callback::post` per frame, the `clawcode-backend` path): all arrived in order in 5.3 s; the engine's longest gap between 50 ms ticks during the burst was 1 ms. Batching frames made no difference, so per-frame `post` stays. (`app.type` waited ~4 s during a burst against 104 ms idle: the test harness waits for the screen to settle, not the engine) |
| 4 | Composer input: IME, Enter / Shift+Enter, auto-grow | Conditional | Headless: Enter while an input method composes does not submit (`app.compose`); Shift+Enter inserts a line; Enter submits. Real GTK window: the textarea grows from 2 to 8 lines, then stops. Two gaps: past 8 lines the view does not follow the caret (lines 1–8 stay in view at line 13), and a real Linux input method (fcitx/ibus) cannot be driven by `nsh test` (`compose` is headless and macOS only) and needs a manual check |
| 5 | 3000 transcript rows, streaming markdown, recycling, far jumps | Pass | `List` of 3000 `TextView` markdown rows (paragraphs, code blocks, short lines): `scrollToEnd` reached row 2999 in 117 ms; the last row streamed 50 chunks at 20 ms with `options={{ streaming: true }}`; jumping back to row 0 showed only rows near the top (no recycled cell kept another row's text). Gesture scrolling across 2300 rows in the headless harness is slow; far jumps use the list ref |
| 6 | `DiffView` with a 1 MiB patch | Pass | A 1036 KiB unified diff (20 files × 600 changed lines) showed its first line 322 ms after the press |
| 7 | Tree, resizable panes, clipboard | Pass | `nsh/base` 0.6.1 has `Tree` (wrapped as `ui` `TreeList`, used in the gallery), `ResizablePanelGroup` / `ResizablePanel` (rendered in the probe page) and `Clipboard.writeText` (used by `CopyButton`; not asserted in a test) |

## Notes

- nsh 0.6.1 does not support the `ext/` + `nsh.json` `"dependencies"` layout yet (there is no `nsh add`), so the app
  uses `"rust": "ext/backend"`. The crate still lives in `ext/backend`; moving to `"dependencies"` later is a manifest
  change only.
- Because the crate is a root workspace member, it follows AGENTS.md rule 12: `nsh-core` is declared once in the root
  `[workspace.dependencies]` (pinned to the CLI version, 0.6.1; add `nsh-base` there when Rust components need it) and inherited with
  `{ workspace = true }`. The crate drops NSH's empty `[workspace]`.
- Cargo builds the crate into the root `target/`; `nsh` builds the headless runner into `apps/desktop/target/`
  (git-ignored by the app's `.gitignore`).

## Stop gate

No item failed; item 4 passed with conditions, carried into N1 as composer work:

- make the capped textarea follow the caret (or bound it with a scroll container that does);
- check Enter with a real Linux input method (fcitx or ibus) by hand once the composer exists.

The probe crate (`ext/probe`) and probe pages were deleted after these runs.
