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

Keep the `nsh-*` crates in the root `Cargo.toml` at the version of the `nsh` CLI in use (currently 0.6.1).

The TSX tests start the real backend, which reads `CLAW_CONFIG`. Always point it at
`tests/fixture/config.toml` (an offline provider, data under `target/fixture-data`) so tests never open your own
configuration or Sessions.
