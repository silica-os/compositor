# compositor

Silica's Wayland compositor, the equivalent of macOS's WindowServer: it owns the displays and
the input devices, places and animates windows, and draws the glass materials. Built on
[Smithay](https://github.com/Smithay/smithay).

| Crate | Role |
| --- | --- |
| `crates/compositor` | The `silica-compositor` binary: Wayland server, window management, input, outputs. Backends: `drm` (the real session, on logind through libseat) and `headless` (tests). |
| `crates/compositor-render` | GLES shaders: glass, materials, shadows, corners, titlebars of foreign apps. |
| `crates/protocols` | Silica's Wayland protocols (`xml/`) with server and client bindings; AppKit and the shell use them. |
| `crates/control` | The control channel (JSON lines on a user-only Unix socket) for tests and agents. |
| `crates/cli` | `silica-cli` (control channel from the terminal, MCP) and `silica-action`. |

Docs: [`docs/protocols.md`](docs/protocols.md), [`docs/control.md`](docs/control.md) and the
measured behaviour in [`docs/spec/`](docs/spec/).

## Build and test

The `drm` feature needs the libudev, libinput, libseat, libgbm and libxkbcommon development
files; the tests draw with EGL on any Mesa driver, llvmpipe included.

```
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test
```

On Silica the compositor is built into the image by the
[`system`](https://github.com/silica-os/system) repo.

## Origin

Migrated from Ossido, in the archived `silica-os/silica-desktop` (branch `vetro`, commit
`02a8972`), with the winit backend, the app embedder and the TCP control channel removed.

## License

MIT or Apache-2.0, at your option.
