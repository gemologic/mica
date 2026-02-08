# mica

[![uses nix](https://img.shields.io/badge/uses-nix-%237EBAE4)](https://nixos.org/)
![rust](https://img.shields.io/badge/Rust-1.95%2B-orange.svg)

A TUI for managing Nix environments. Mica lets you search packages, apply presets, and keep a reproducible `default.nix` with minimal Nix ceremony.

**Features**
- Interactive TUI for package search and preset selection
- Generates a managed `default.nix` and preserves edits outside `# mica:` markers
- Pin management for nixpkgs
- Bundled presets plus user presets from configurable directories
- Cached SQLite index for fast search

**Requirements**
- Nix installed, with `nix-env` and `nix-prefetch-url` available
- Rust toolchain to build and run
- Optional: direnv for project shells

**Quick Start (Project Mode)**
1. In a project directory without an existing managed `default.nix`, run `cargo run -p mica -- init`
2. Launch the TUI with `cargo run -p mica -- tui`
3. In the TUI, search packages, toggle selections, and press `Ctrl+S` to save

**Targeting Examples (`--file`, `--global`)**
- `mica --file ./default.nix list` inspect a specific managed file
- `mica --file ./default.nix diff` check drift for that file
- `mica --global list` inspect your global profile state
- `mica --global add ripgrep` add a package globally
- `mica --global generations list` inspect global generations

**TUI Keys**
- `Tab` switch focus between packages, presets, and changes
- `Type` search in the focused panel, `Ctrl+U` clears search
- Search shortcuts: `'exact`, `bin:`, `name:`, `desc:` (for example `bin:rg` or `'name:ripgrep`)
- `Enter` or `Space` toggle selection
- `S` cycle search mode (`all`, `name`, `desc`, `bin`)
- `Ctrl+P` package info, `Ctrl+V` version picker
- `B`/`I`/`V` toggle broken, insecure, and installed-only filters
- `L`/`O` edit license and platform filters
- `E` edit environment variables
- `H` edit shell hook
- `T` toggle presets panel, `C` toggle changes panel
- `D` diff preview, and inside diff view `T` toggles full vs changes-only
- `K` toggle details panel
- `U` update nixpkgs pin
- `R` rebuild index, `Y` reload from nix
- `Ctrl+S` save, `Ctrl+Q` quit, `?` help

**CLI Examples**
- `mica init` initialize a project `default.nix`
- `mica tui` launch the TUI
- `mica add ripgrep fd` add packages
- `mica remove fd` remove packages
- `mica search rg --mode binary` search by binary name
- `mica apply rust` apply a preset
- `mica unapply rust` remove a preset
- `mica --file ./default.nix list` target a specific file
- `mica --global list` target global profile state
- `mica diff` show drift between state and `default.nix`
- `mica sync` regenerate `default.nix` from state
- `mica index rebuild /tmp/nixpkgs.json`

**Configuration**
Config lives at `~/.config/mica/config.toml`.

```toml
[nixpkgs]
default_url = "https://github.com/jpetrucciani/nix"
default_branch = "main"

[presets]
extra_dirs = ["~/my-presets"]

[index]
remote_url = "https://example.com/mica-index"
update_check_interval = 24

[tui]
show_details = true
```

You can also override the repo for `mica init` with `--repo` or `MICA_NIXPKGS_REPO`.

**Files and State**
- Project state is embedded in `default.nix`
- Global profile state uses `~/.config/mica/profile.toml` and `~/.config/mica/profile.nix`
- Package index cache is stored at `~/.config/mica/cache/index.db`

**Presets**
Mica ships with bundled presets in `presets/` and can load extra preset directories via `presets.extra_dirs`.

**Advanced**
- Extra pin workflows are available via `mica pin --help`.

**Development**
- `direnv exec . cargo run -p mica -- tui`
- `direnv exec . cargo fmt`
- `direnv exec . cargo clippy --all --benches --tests --examples --all-features -- -D warnings -W clippy::collapsible_else_if`

**Docs (VitePress + bun)**
- `bun install`
- `bun run docs:dev`
- `bun run docs:build`
