# `monster-step-viewer`

A 3D viewer for STEP (ISO 10303-21) CAD files, built with [`monstertruck`](https://github.com/virtualritz/monstertruck), `bevy` and `egui`.

## Features

- Load and display STEP files with full assembly support.
- Model hierarchy panel showing shells and faces.
- Per-face visibility toggling.
- Random colors mode for visualizing individual faces.
- STEP-defined colors from the file.
- Adjustable tessellation density.
- Wireframe edge display (boundary edges).
- Bounding box visualization.
- Pan/orbit camera controls.
- File metadata display.

## Installation

```bash
cargo install monster-step-viewer
```

The installed binary is called `mstpv`

## Usage

```bash
# Run with a STEP file
mstpv path/to/model.step

# Or run and use the file dialog
mstpv
```

## Controls

- **Left mouse drag**: Rotate view
- **Right mouse drag**: Pan view
- **Scroll wheel**: Zoom in/out
- **Escape**: Quit

## Toolbar Icons

- **Dice icon**: Toggle random face colors
- **Box icon**: Toggle bounding box
- **Grid icon**: Toggle wireframe edges

## Building

Requires Rust 1.88+ (2024 edition).

```bash
# Debug build
cargo build

# Release build (recommended for performance)
cargo build --release

# Run tests
cargo test
```

### Linux Dependencies

```bash
# Debian/Ubuntu
sudo apt-get install libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libssl-dev libasound2-dev libudev-dev

# Fedora
dnf install clang clang-devel clang-tools-extra libxkbcommon-devel pkg-config openssl-devel libxcb-devel alsa-lib-devel systemd-devel
```

## Dependencies

- [`bevy`](https://bevyengine.org/) - Game engine for rendering
- [`egui`](https://github.com/emilk/egui/) - Immediate mode GUI
- [`truck`](https://github.com/ricosjp/truck) - STEP file parsing and tessellation
- [`bevy_panorbit_camera`](https://github.com/Plonq/bevy_panorbit_camera) - Camera controls

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
