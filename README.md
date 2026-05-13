# `monster-step-viewer`

A 3D viewer for STEP (ISO 10303-21) CAD files, built with [`monstertruck`](https://github.com/virtualritz/monstertruck), `bevy` and `egui`. Optional path-traced overlay via [3Delight](https://www.3delight.com) (NSI).

![Screenshot](screenshot.png)

## Features

- Load and display STEP files with full assembly support.
- Model hierarchy panel showing shells and faces.
- Per-face visibility toggling.
- Random colors mode for visualizing individual faces.
- STEP-defined colors from the file.
- Adjustable tessellation density.
- Wireframe edge display (boundary edges).
- Polygon-edge overlay (triangulated mesh edges) and isoparametric curve overlay.
- Per-axis clip planes (X / Y / Z, flippable, with optional solidify).
- Bounding box visualization.
- Pan/orbit camera controls, with keyboard shortcuts to frame the scene or focus the current selection.
- Optional 3Delight (NSI) path-traced overlay — see [3Delight rendering](#3delight-rendering).
- File metadata display.

## Installation

```bash
cargo install monster-step-viewer
```

The installed binary is called `mstpv`.

## Usage

```bash
# Run with a STEP file
mstpv path/to/model.step

# Or run and use the file dialog
mstpv
```

## Controls

### Mouse

- **Right-drag**: Orbit
- **Middle-drag**: Pan
- **Scroll wheel**: Zoom (dolly)
- **Left-click**: Pick face / hierarchy item

### Keyboard

- **R**: Reset view (frame the whole scene from the initial vantage)
- **F**: Frame scene (re-fit current camera angle to the scene bounds)
- **C**: Center on selection (fit the camera to the currently-selected face)
- **Ctrl** + **+** / **-** / **0**: Zoom the egui UI in / out / reset
- **Esc**: Quit

## Toolbar Icons

Top-right viewport overlay; icons are [Material Symbols](https://fonts.google.com/icons) (outlined). Click to toggle.

| Icon | Symbol | Action |
| :--: | :----- | :----- |
| <img src="https://fonts.gstatic.com/s/i/short-term/release/materialsymbolsoutlined/casino/default/24px.svg" width="20" alt="casino"> | `casino` | Random face colors |
| <img src="https://fonts.gstatic.com/s/i/short-term/release/materialsymbolsoutlined/palette/default/24px.svg" width="20" alt="palette"> | `palette` | STEP-defined colors |
| <img src="https://fonts.gstatic.com/s/i/short-term/release/materialsymbolsoutlined/view_in_ar/default/24px.svg" width="20" alt="view_in_ar"> | `view_in_ar` | Bounding box |
| <img src="https://fonts.gstatic.com/s/i/short-term/release/materialsymbolsoutlined/deployed_code/default/24px.svg" width="20" alt="deployed_code"> | `deployed_code` | STEP curve wireframe |
| <img src="https://fonts.gstatic.com/s/i/short-term/release/materialsymbolsoutlined/grid_on/default/24px.svg" width="20" alt="grid_on"> | `grid_on` | Isoparametric curves |
| <img src="https://fonts.gstatic.com/s/i/short-term/release/materialsymbolsoutlined/details/default/24px.svg" width="20" alt="details"> | `details` | Polygon (mesh) edges |
| <img src="https://fonts.gstatic.com/s/i/short-term/release/materialsymbolsoutlined/counter_3/default/24px.svg" width="20" alt="counter_3"> | `counter_3` | 3Delight NSI overlay (requires `nsi-render` feature + 3Delight installed) |

The toolbar also exposes per-axis clip planes (X / Y / Z) — click to cycle Off → +axis → −axis → Off — plus a tessellation-quality slider and shading-mode picker.

## 3Delight rendering

Build with the `nsi-render` Cargo feature to embed a 3Delight (NSI) progressive-render overlay:

```bash
cargo run --features nsi-render -- path/to/model.step
```

[3Delight](https://www.3delight.com) must be installed locally; the build auto-detects it at `/usr/local/3delight` or via `$DELIGHT`. When enabled, the `counter_3` toolbar button starts an interactive path-traced render of the model's exact NURBS surfaces in 3Delight's `idisplay` window — geometry is sent once via NSI's `nurbs` node, camera and visibility toggles stream as incremental `set_attribute` updates.

This is a native-only feature; wasm builds are unaffected.

## Building

Requires Rust 1.89+ (2024 edition).

```bash
# Debug build
cargo build

# Release build (recommended for performance)
cargo build --release

# Native build with the 3Delight overlay
cargo build --release --features nsi-render

# Run tests
cargo test
```

### Linux Dependencies

```bash
# Debian/Ubuntu
sudo apt-get install libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libssl-dev libudev-dev

# Fedora
dnf install clang clang-devel clang-tools-extra libxkbcommon-devel pkg-config openssl-devel libxcb-devel systemd-devel
```

## Dependencies

- [`bevy`](https://bevyengine.org/) – game engine for rendering (audio features are disabled, so ALSA/`libasound` is not required)
- [`egui`](https://github.com/emilk/egui/) – immediate-mode GUI
- [`monstertruck`](https://github.com/virtualritz/monstertruck) – STEP parsing, BRep topology, and tessellation
- [`bevy_editor_cam`](https://github.com/aevyrie/bevy_editor_cam) – orbit / pan / dolly camera controls
- [`meshopt`](https://github.com/gwihlidal/meshopt-rs) – mesh post-processing (vertex cache / fetch reordering); native-only
- [`nsi`](https://github.com/virtualritz/nsi) – 3Delight scene interface bindings (only with `nsi-render`)

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
