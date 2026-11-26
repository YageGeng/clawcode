# Clawcode desktop (NSH)

The desktop client written in TSX on [NSH](https://nsh.dev), with one Rust crate of its own:

- `App.tsx`: the entry.
- `ext/backend`: the `clawcode-backend` native module (`nsh.json` `"rust"`). It is a member of the root Cargo
  workspace, so it inherits dependencies from the root `[workspace.dependencies]`.
- `src/client`: the ACP client (transport interface, protocol, session state), owned by this app. Its tests are `src/client/*.test.ts`, run by `nsh test`.

```sh
nsh check    # cargo check of ext/backend, then type checks the TSX
nsh build    # bundles the TSX; every warning is a finding
CLAW_CONFIG=$PWD/tests/fixture/config.toml nsh test   # cargo test, then *.test.tsx headless
nsh run      # opens the app with hot reload
```

Use the `nsh` CLI version 0.7.1 to match `nsh-core` in the root `Cargo.toml`.
The supported Linux session is Wayland. Use `GDK_BACKEND=wayland nsh run` to launch the GTK app
and the same environment variable with `nsh test --target linux` for native UI checks.

The TSX tests start the real backend, which reads `CLAW_CONFIG`. Always point it at
`tests/fixture/config.toml` (an offline provider, data under `target/fixture-data`) so tests never open your own
configuration or Sessions.
