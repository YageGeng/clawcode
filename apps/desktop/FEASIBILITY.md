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

# R1 feasibility record

Spec: [Desktop NSH design, section 9](../../docs/superpowers/specs/2026-11-25-desktop-nsh-design.md)

Checked 2026-11-25 on Linux (KDE Wayland, `nsh 0.6.1`, `cargo 1.100.0`) with `iroh 1.5.0` (`presets::N0`: n0 relays and
address lookup) and ALPN `clawcode/acp/1`. One machine only: its network exits in the US (public address of an AT&T
range), so n0 picked `usw1-1.relay.n0.iroh.link` as the home relay.

| # | Item | Result | Evidence |
| --- | --- | --- | --- |
| 1 | iroh beside the NSH runtime and the backend's tokio runtime | Pass on Linux; macOS and Windows not checked | `iroh = "1.5"` in the root workspace and `ext/backend`: `cargo build -p clawcode-backend` and the headless runner built with `app`, `acp` and `kernel`, no version conflicts (122 new crates in `Cargo.lock`, a few RustCrypto and `getrandom` crates in two versions). An NSH async export ran the endpoint on a tokio runtime of its own (spawn, then await a `futures` oneshot from the engine's executor) while `Backend.start()` ran the real backend on its thread. No Windows cross-compiler (`x86_64-w64-mingw32-gcc`) or macOS SDK here: those builds need CI or another machine |
| 2 | Connectivity, first frame, streaming throughput | Pass for LAN and relay on one machine; two real devices across NAT not checked | Line-framed echo on one bidirectional stream. Direct (host address with IPs): connect 4 ms, first frame 1.1 ms, LAN path, 2500 frames (912 KiB) echoed at ~246 000 frames/s. Relay only (`clear_ip_transports`, `AddrFilter::relay_only`): connect 356 ms, first frame 405 ms, path `usw1` relay RTT ~350 ms, 2500 frames echoed in 9.0 s (~277 frames/s, ~0.1 MiB/s each way). Dial by id alone (n0 address lookup): connect 0.74 s, first frame 414 ms through the relay, then upgraded to the LAN path. Endpoints come online (home relay connected) 2.6–4 s after bind |
| 3 | QR code | Pass | A sample pairing code (`clawcode-pair:1:` + base32 of key, 128-bit secret and expiry, + relay URL; 154 chars) rendered by the `qrcode` crate as SVG (24 KiB), shown by NSH `Image` from a `data:image/svg+xml` URI (both `;utf8,` with `encodeURIComponent` and `;base64,`) in a real window; `zbarimg` decoded the screenshot back to the exact code |
| 4 | Two instances on one machine, separate data dirs, connecting | Pass | Two `nsh test` processes (headless, then two real GTK windows at once), each with its own `CLAW_CONFIG` data home and its own device key file, each ran `Backend.start()` (separate SQLite state) and an endpoint; the second dialed the first by id and relay URL, as a pairing code would carry them: connect 1.25 s, first frame ~550 ms through the relay, then the LAN path. The key file survived across runs (same id) |

## Notes

- The relay path is slow from this network: ~350 ms RTT and ~0.1 MiB/s on n0's development relay. A typing-speed
  stream is fine; replaying a long Session (megabytes of transcript) remotely takes seconds. Choose a closer or
  self-hosted relay before release (spec decision 6), and keep replay batched.
- n0's endpoints need 2.6–4 s to come online. The host should bind its endpoint at startup (while remote control is on),
  not when a remote connects; a remote's first connection pays roughly one second for lookup and the relay handshake.
- NSH async exports run on the engine's executor, not tokio: every iroh future goes through a tokio runtime handle
  and comes back over a channel. R1 can put the endpoint on the backend thread's runtime (spec section 3) instead of a
  second one; the probe used its own.
- LAN discovery (mDNS) is not part of `iroh` 1.5; n0 ships it as `iroh-mdns-address-lookup` 0.6.1
  (n0-computer/iroh-address-lookups), not probed here. Without it, same-LAN peers still went direct after the relay
  exchanged their addresses, but they need the relay to find each other.
- The NSH app data directory override (two instances with separate NSH storage) is not built yet: the probe separated
  only the backend data and the key files. R1 adds the override with the device key (N0 supplement note).

## Stop gate

No item failed. Item 1 passed on Linux only, and item 2 only on one machine: macOS and Windows builds and a
two-device cross-NAT run stay open for R1's acceptance. The probe crate, the extension probe module and the probe
pages were deleted after these runs.
