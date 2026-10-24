# compositor

Silica's Wayland compositor, the equivalent of macOS's WindowServer: it owns the displays and
the input devices, places and animates windows, and draws the glass materials. Built on
[Smithay](https://github.com/Smithay/smithay).

| Crate | Role |
| --- | --- |
| `crates/compositor` | The `silica-compositor` binary: Wayland server, window management, input, outputs. Backends: `drm` (the real session, on logind through libseat) and `headless` (tests). |
| `crates/compositor-render` | The effects for both renderers: glass, materials, shadows, corners, titlebars of foreign apps, as GLES shaders and as CPU kernels (`src/cpu/`). |
| `crates/raster` | `compositor-raster`, the CPU renderer behind Smithay's renderer traits: premultiplied BGRA8, a display list drawn in bands on a small thread pool, SIMD kernels (`fearless_simd`), compiled ahead of time. |
| `crates/protocols` | Silica's Wayland protocols (`xml/`) with server and client bindings; AppKit and the shell use them. |
| `crates/control` | The control channel (JSON lines on a user-only Unix socket) for tests and agents. |
| `crates/cli` | `silica-cli` (control channel from the terminal, MCP) and `silica-action`. |

Docs: [`docs/protocols.md`](docs/protocols.md), [`docs/control.md`](docs/control.md) and the
measured behaviour in [`docs/spec/`](docs/spec/).

## Rendering

- **Two renderers, one scene.** GLES on a GPU; the CPU renderer when GLES would run on a
  software rasterizer. The compositor holds no JIT entitlement, as WindowServer holds none, so it
  never renders on llvmpipe. `SILICA_RENDERER=auto|cpu|gles` overrides the choice; the headless
  backend takes `SILICA_COMPOSITOR_HEADLESS_RENDERER=cpu|gpu` (GLES on llvmpipe by default).
- **Planes.** DRM outputs go through Smithay's `DrmCompositor`: rendering into the swapchain with
  its buffer age, damage clips, no blocking wait on the GPU, the cursor on the cursor plane and
  opaque full-screen clients scanned out directly. Pointer motion alone draws no frame.
- **Frame budget** (compositor#9), per drawn frame at 1280 × 800 with one window: p99 under
  2.42 ms on the CPU; with a GPU, the target is 0.5 ms of compositor CPU time and 0.5 ms of GPU
  time.
  `perf.render` measures it per stage. `tests/frame_budget.rs` runs the scenarios on the host;
  `silica-test-client --scene` drags a window over glass in the VM, and `tools/test-vm
  --desktop` checks the CPU budget with it.

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
