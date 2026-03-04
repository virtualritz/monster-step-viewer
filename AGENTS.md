# Repository Guidelines

## Project Structure & Module Organization

- `src/main.rs` boots the Bevy app, passing optional STEP paths from CLI.
- `src/lib.rs` re-exports the public API from `step_loader`.
- `src/step_loader.rs` owns STEP parsing, tessellation, transforms, colors, and streaming loader.
- `assets/` contains icons, manifest, and `sw.js` for the web build; keep cache names in sync when renaming the crate.
- `index.html` and `Trunk.toml` configure the WASM build; `examples/parse_test.rs` is a CLI parser smoke test.
- `check.sh` runs the full CI-like suite locally. Avoid editing `target/` or other generated outputs.

## Build, Test, and Development Commands

- `cargo run --release [<path/to/file.step>]` — launch the native viewer (you can pass a STEP path).
- `cargo run --example parse_test` — parse the sample STEP file without the GUI.
- `./check.sh` — fmt, clippy (warnings-as-errors), tests, wasm check, and a Trunk build; fastest way to match CI.
- Web: `rustup target add wasm32-unknown-unknown && cargo install --locked trunk` once, then `trunk serve` for live reload at `http://127.0.0.1:8080/index.html#dev`; `trunk build --release` for deployable `dist/`.

## Coding Style & Naming Conventions

- Rust 2024 edition; prefer idiomatic snake_case for files/modules and UpperCamelCase for types (e.g., `StepViewerApp`, `StepRenderer`).
- Run `cargo fmt` before committing; `cargo clippy --all-targets --all-features -- -D warnings` should stay clean.
- RFC 430 casing. Use `as_`/`to_`/`into_` conversion conventions.
- No `get_` prefix on getters: use `width()` not `get_width()`.

## Functional Style

- Prefer `collect()`/iterator pipelines over `Vec::new()` + for + push.
- Use `map`, `filter`, `for_each`.
- Direct init with `vec![]`, `BTreeMap::from([..])` where possible.

## Import Rules

- Always import types at file top with `use`. Never use `std::path::PathBuf` or other qualified paths inline in function bodies.

## Expression Style

- Avoid explicit `return` statements. Structure with if/else expression blocks instead.

## Comment Rules

- All `//` and `///` comments must end with a period.
- Comments go on their own line. Never put comments at end of a line of code.

## Testing

- Add unit tests near the code they cover or in `tests/` for integration; include doc tests for examples.
- Run `cargo test --all-targets --all-features` and `cargo test --doc` (already in `check.sh`).
- For parser changes, update or extend `examples/parse_test.rs`; keep sample STEP fixtures small and committed.
- No `test_` prefix on test functions.

## Performance

- Use rayon for parallel processing of larger data.
- Use SmallVec for small fixed-size collections in hot paths.
- Avoid unnecessary allocations and clones.

## Type Design

- C-COMMON-TRAITS: Derive `Debug`, `Clone`, `Hash`, `PartialEq`, `Eq`, `Copy` where possible on public types.
- C-STRUCT-PRIVATE: Prefer private fields with accessors.
- No unsafe unless absolutely necessary.

## Documentation

- Comments end with a period.
- First reference to external types linked with backtick brackets.

## Module Size

- Target ~300-500 lines per file. Split larger files into submodules.

## Commit & Pull Request Guidelines

- Prefer concise, imperative commits (`Add STEP entity legend`, `Fix trunk build warnings`); keep scope tight.
- In PRs, describe the user-visible change, how it was tested (commands), and attach screenshots/GIFs for UI updates.
- Link related issues when available; note any platform-specific considerations (native vs. wasm) and asset changes that affect caching.
