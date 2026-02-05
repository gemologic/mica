# mica

[![uses nix](https://img.shields.io/badge/uses-nix-%237EBAE4)](https://nixos.org/)
![rust](https://img.shields.io/badge/Rust-1.95%2B-orange.svg)

A TUI for managing Nix environments. Mica lets you search packages, apply presets, and keep a reproducible `default.nix` with minimal Nix ceremony.

**Features**
- Interactive TUI for package search and preset selection
- Generates a managed `default.nix` and preserves edits outside `# mica:` markers
- Pin management for nixpkgs and extra pins
- Bundled presets plus user presets from configurable directories
- Cached SQLite index for fast search

**Requirements**
- Nix installed, with `nix-env` and `nix-prefetch-url` available
- Rust toolchain to build and run
- Optional: direnv for project shells

**Quick Start (Project Mode)**
1. `cargo run -- init`
2. `cargo run -- tui`
3. In the TUI, search packages, toggle selections, and press `Ctrl+S` to save

**TUI Keys**
- `Tab` switch focus between packages and presets
- `Type` search in the focused panel
- `Enter` or `Space` toggle selection
- `Ctrl+P` package info
- `Ctrl+N` add an extra pin
- `E` edit environment variables
- `H` edit shell hook
- `D` diff preview, `T` toggle full vs changes-only
- `U` update nixpkgs pin
- `R` rebuild index, `Y` reload from nix
- `Ctrl+S` save, `Ctrl+Q` quit, `?` help

**CLI Examples**
- `mica init` initialize a project `default.nix`
- `mica tui` launch the TUI
- `mica add ripgrep fd` add packages
- `mica remove fd` remove packages
- `mica apply rust` apply a preset
- `mica unapply rust` remove a preset
- `mica pin add --name rust --url https://github.com/oxalica/rust-overlay --latest`
- `mica diff` show drift between state and `default.nix`
- `mica sync` regenerate `default.nix` from state
- `mica index rebuild --input /tmp/nixpkgs.json`

**Configuration**
Config lives at `~/.config/mica/config.toml`.

```toml
[nixpkgs]
default_url = "https://github.com/jpetrucciani/nix"
default_branch = "main"

[presets]
extra_dirs = ["~/my-presets"]
```

You can also override the repo for `mica init` with `--repo` or `MICA_NIXPKGS_REPO`.

**Files and State**
- Project state is embedded in `default.nix`
- Global profile state uses `~/.config/mica/profile.toml` and `~/.config/mica/profile.nix`
- Package index cache is stored at `~/.config/mica/cache/index.db`

**Presets**
Mica ships with bundled presets in `presets/` and can load extra preset directories via `presets.extra_dirs`.

**Development**
- `direnv exec . cargo run -- tui`
- `direnv exec . cargo fmt`
- `direnv exec . cargo clippy --all --benches --tests --examples --all-features`
