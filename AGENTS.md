# Repository Guidelines

## Project Structure & Module Organization
- `crates/mica-cli/`: CLI and TUI entrypoint (`src/main.rs`, `src/cli.rs`, `src/tui/`, `src/commands/`).
- `crates/mica-core/`: Core domain logic (state, config, presets, Nix parsing/generation).
- `crates/mica-index/`: Package index tooling (SQLite schema, import, generation).
- `presets/`: Bundled preset TOML files like `rust.toml`, `python.toml`.
- `spec.md`: Architecture and product spec reference.
- `default.nix`: Nix dev shell for the workspace.

## Build, Test, and Development Commands
- `cargo build`: Build the workspace.
- `cargo run -p mica -- --help`: Run the CLI/TUI binary.
- `cargo test`: Run all tests.
- `cargo test -p mica-core`: Focus on core library tests.
- `cargo fmt`: Format Rust sources.
- `cargo clippy --all --benches --tests --examples --all-features`: Lint for CI parity.
- Optional Nix shell: `nix-shell` (or direnv) to load the dev environment from `default.nix`.

## Coding Style & Naming Conventions
- Rust defaults apply: 4-space indentation, `snake_case` for functions/modules, `CamelCase` for types.
- Use `crate::` paths for internal imports over `super::`.
- Avoid panics in non-test code, return `Result` instead.
- Keep modules focused and delete unused helpers rather than leaving dead code.

## Testing Guidelines
- Tests live alongside code in `mod tests` blocks at the bottom of modules (see `crates/mica-core/src/*.rs`).
- Prefer real unit and integration tests over mocks.
- Name tests for behavior, for example `parses_preset_order()`.

## Commit & Pull Request Guidelines
- This repository has no commit history yet, so no established convention exists.
- Use short, imperative subjects, and consider a simple `type: summary` pattern like `feat: add preset merge`.
- PRs should include a clear description, test results, and screenshots for TUI changes.
- Call out dependency additions explicitly and explain maintenance tradeoffs.

## Configuration & Assets
- Presets live in `presets/` and should follow the existing TOML layout.
- Document user-facing behavior changes in `spec.md` when they affect the product surface area.
