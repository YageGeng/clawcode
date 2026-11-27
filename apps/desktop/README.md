# Clawcode desktop (NSH)

The desktop client written in TSX on [NSH](https://nsh.dev), with one Rust crate of its own:

- `App.tsx`: the entry.
- `ext/backend`: the `clawcode-backend` native module (`nsh.json` `"rust"`). It is a member of the root Cargo
  workspace, so it inherits dependencies from the root `[workspace.dependencies]`.
- `src/client`: the ACP client (transport interface, protocol, session state), owned by this app.
- `tests`: unit and UI tests (`tests/**/*.test.{ts,tsx}`), entries and fixtures, run by `nsh test`.

```sh
nsh check    # cargo check of ext/backend, then type checks the TSX
nsh build    # bundles the TSX; every warning is a finding
CLAW_CONFIG=$PWD/tests/fixture/config.toml nsh test   # cargo test, then *.test.tsx headless
nsh run      # opens the app with hot reload
```

Use the nightly `nsh` CLI (`nsh update nightly`). The root `Cargo.toml` keeps the crates.io release; the repository's
`.cargo/config.toml` patches every nsh crate to the GitHub commit that CLI was built from (`nsh --version` shows it), and
NSH builds this app's hosts from the same checkout. After `nsh update nightly`, set the `rev`s there to the new commit.
Delete that file to go back to the release (then use the stable CLI of the same version).
The supported Linux session is Wayland. Use `GDK_BACKEND=wayland nsh run` to launch the GTK app
and the same environment variable with `nsh test --target linux` for native UI checks.

The TSX tests start the real backend, which reads `CLAW_CONFIG`. Always point it at
`tests/fixture/config.toml` (an offline provider, data under `target/fixture-data`) so tests never open your own
configuration or Sessions.

## Pair two computers

On the host, open **Settings → Remote control**, enable **Allow remote control**, and choose **Add device**. On the controller, open the computer menu, choose **Add remote computer**, and paste the pairing code. Check the device name on the host before choosing **Allow**. A paired device can start conversations, run tools and modify files with the host user's permissions. Provider sign-in and credential management are available through the host interface.

Select the host in the controller's sidebar and choose a workspace on that host. A file permission request can be answered from the remote conversation. On the host, **Disconnect** ends the current connection. **Revoke** also removes trust, so the device must pair again.

### Run two Wayland instances on one machine

Each instance needs its own desktop profile and backend state. From this directory run the following commands in separate terminals, with your normal provider configuration. The explicit backend override takes precedence over a data directory in that configuration. Use two free dev-server ports.

```sh
GDK_BACKEND=wayland XDG_DATA_HOME="$PWD/target/r1-host/desktop" CLAW_SESSION_PERSISTENCE__DATA_HOME="$PWD/target/r1-host/backend" CLAW_DESKTOP_RELAY=direct nsh run --port 18790
```

```sh
GDK_BACKEND=wayland XDG_DATA_HOME="$PWD/target/r1-controller/desktop" CLAW_SESSION_PERSISTENCE__DATA_HOME="$PWD/target/r1-controller/backend" CLAW_DESKTOP_RELAY=direct nsh run --port 18791
```

NSH keeps language and appearance settings under `$XDG_DATA_HOME/com.clawcode.desktop/`. In that directory the native bridge stores the private key in `device.key` and the sharing preference, trusted devices and remembered hosts in `remote.json`. Sharing starts off until the owner explicitly enables it in this profile. Each running process needs its own profile directory.

`CLAW_DESKTOP_RELAY=direct` selects loopback for same-machine testing. Leave the variable unset for LAN discovery and the default relay, or set it to your relay URL.
