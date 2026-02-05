# Mica: A TUI for Managing Nix Environments

## Executive Summary

Mica is a terminal user interface (TUI) application that simplifies Nix environment management. It provides an intuitive interface for searching packages, managing presets, and maintaining reproducible development environments—without requiring deep Nix expertise.

Mica operates in two modes:
1. **Project Mode**: Manages `default.nix` files for per-project development environments (used with direnv)
2. **Global Mode**: Manages a user-wide package profile installed via `nix-env`

The core philosophy is to make Nix's power accessible while preserving its reproducibility guarantees.

---

## Goals & Non-Goals

### Goals
- Provide a friendly TUI for browsing and selecting Nix packages
- Support searching packages by name, description, and provided binaries
- Manage `default.nix` files with a well-defined, parseable structure
- Support composable presets for common development environments
- Enable version pinning with multiple nixpkgs sources
- Maintain backward compatibility with hand-edited files where possible
- Fast startup and responsive search (pre-built indexes)
- Work with classic Nix (no flakes requirement)

### Non-Goals
- Replace Nix or nixpkgs
- Support arbitrary Nix expression modification (only tool-managed formats)
- Provide a GUI (terminal only)
- Manage NixOS system configurations
- Support flakes (may be added later as optional feature)

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                            CLI (clap)                               │
│   mica [search|add|remove|update|presets|export|index|sync]        │
└─────────────────────────────────────────────────────────────────────┘
                                   │
                                   ▼
┌─────────────────────────────────────────────────────────────────────┐
│                          TUI (ratatui)                              │
│  ┌───────────────┐  ┌───────────────┐  ┌───────────────┐           │
│  │ Package       │  │ Preset        │  │ Version       │           │
│  │ Browser       │  │ Selector      │  │ Picker        │           │
│  └───────────────┘  └───────────────┘  └───────────────┘           │
└─────────────────────────────────────────────────────────────────────┘
                                   │
                                   ▼
┌─────────────────────────────────────────────────────────────────────┐
│                         Core Library                                │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐ ┌────────────┐ │
│  │ State        │ │ Nix          │ │ Package      │ │ Preset     │ │
│  │ Manager      │ │ Generator    │ │ Index        │ │ Engine     │ │
│  └──────────────┘ └──────────────┘ └──────────────┘ └────────────┘ │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐               │
│  │ Nix Parser   │ │ Version      │ │ Evaluator    │               │
│  │ (rnix)       │ │ Resolver     │ │ (nix-inst.)  │               │
│  └──────────────┘ └──────────────┘ └──────────────┘               │
└─────────────────────────────────────────────────────────────────────┘
                                   │
                                   ▼
┌─────────────────────────────────────────────────────────────────────┐
│                        Storage Layer                                │
│  ~/.config/mica/                                                    │
│  ├── config.toml           # User configuration                     │
│  ├── profile.toml          # Global profile state                   │
│  ├── profile.nix           # Generated global profile               │
│  ├── presets/              # User-defined presets                   │
│  ├── cache/                                                         │
│  │   ├── index-<commit>.db # Package indexes (SQLite)              │
│  │   └── versions.db       # Version history database              │
│  └── generations/          # Profile generation history             │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Crate Structure

```
mica/
├── Cargo.toml                 # Workspace definition
├── crates/
│   ├── mica-core/            # Core library
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── state.rs      # State management (TOML read/write)
│   │       ├── nixgen.rs     # Nix file generation
│   │       ├── nixparse.rs   # Nix file parsing (rnix)
│   │       ├── index.rs      # Package index (SQLite queries)
│   │       ├── preset.rs     # Preset loading and merging
│   │       ├── version.rs    # Version resolution
│   │       ├── eval.rs       # Nix evaluation/validation
│   │       └── config.rs     # User configuration
│   │
│   ├── mica-index/           # Index generation tooling
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── generate.rs   # Generate index from nixpkgs
│   │       ├── schema.rs     # SQLite schema
│   │       └── import.rs     # Import from nix-env JSON
│   │
│   └── mica-cli/             # CLI and TUI
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs
│           ├── cli.rs        # Argument parsing (clap)
│           ├── tui/
│           │   ├── mod.rs
│           │   ├── app.rs    # Application state
│           │   ├── ui.rs     # UI rendering
│           │   ├── input.rs  # Input handling
│           │   ├── search.rs # Search view
│           │   ├── preset.rs # Preset view
│           │   └── version.rs# Version picker view
│           └── commands/
│               ├── mod.rs
│               ├── add.rs
│               ├── remove.rs
│               ├── search.rs
│               ├── update.rs
│               ├── export.rs
│               ├── sync.rs
│               └── index.rs
│
├── presets/                   # Bundled presets
│   ├── rust.toml
│   ├── python.toml
│   ├── go.toml
│   ├── node.toml
│   ├── devops.toml
│   └── data.toml
│
└── scripts/
    ├── generate-index.sh     # CI script for index generation
    └── backfill-versions.py  # Historical version indexing
```

---

## Data Models

### User Configuration (`~/.config/mica/config.toml`)

```toml
[mica]
# Tool version that last wrote this config
version = "0.1.0"

[nixpkgs]
# Default nixpkgs source for new projects
default_url = "https://github.com/jpetrucciani/nix"
default_branch = "main"

[index]
# Base URL for pre-built indexes (optional)
remote_url = "https://s3.amazonaws.com/your-bucket/mica-index"
# How long before checking for index updates (hours)
update_check_interval = 24

[presets]
# Additional preset directories to scan
extra_dirs = ["~/my-presets"]

[tui]
# Show package details pane by default
show_details = true
# Default search mode: "name", "description", "binary", "all"
search_mode = "all"
```

### Project State (`.mica.toml` alongside `default.nix`)

```toml
[mica]
version = "0.1.0"
created = "2025-02-04T12:00:00Z"
modified = "2025-02-04T14:30:00Z"

[pin]
# Nixpkgs source
url = "https://github.com/jpetrucciani/nix"
rev = "a1b2c3d4e5f6g7h8i9j0"
sha256 = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
branch = "main"
updated = "2025-02-04"

[presets]
# Active presets (in order of application)
active = ["rust", "devops"]

[packages]
# Packages explicitly added (beyond presets)
added = [
    "jq",
    "yq",
    "httpie",
]

# Packages explicitly removed (override presets)
removed = [
    "cargo-edit",  # Don't want this from rust preset
]

[env]
# Environment variables to set
EDITOR = "nvim"
RUST_BACKTRACE = "1"

[shell]
# Additional shell hook content
hook = '''
echo "Welcome to the dev environment!"
'''
```

### Global Profile State (`~/.config/mica/profile.toml`)

```toml
[mica]
version = "0.1.0"
created = "2025-02-04T12:00:00Z"
modified = "2025-02-04T14:30:00Z"

[pin]
# Primary nixpkgs source
url = "https://github.com/jpetrucciani/nix"
rev = "a1b2c3d4e5f6g7h8i9j0"
sha256 = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
branch = "main"
updated = "2025-02-04"

[presets]
active = ["devops"]

[packages]
added = [
    "ripgrep",
    "fd",
    "bat",
    "eza",
    "zoxide",
]
removed = []

# Version-pinned packages (use different nixpkgs commits)
[packages.pinned.nodejs]
version = "18.19.0"
pin.url = "https://github.com/NixOS/nixpkgs"
pin.rev = "nixos-23.11"
pin.sha256 = "sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB="

[packages.pinned.python3]
version = "3.11.7"
pin.url = "https://github.com/NixOS/nixpkgs"
pin.rev = "abc123def456"
pin.sha256 = "sha256-CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC="

[generations]
# Track recent generations for rollback reference
[[generations.history]]
id = 1
timestamp = "2025-02-01T10:00:00Z"
packages = ["ripgrep", "fd"]

[[generations.history]]
id = 2
timestamp = "2025-02-04T14:30:00Z"
packages = ["ripgrep", "fd", "bat", "eza", "zoxide"]
```

### Preset Format (`presets/*.toml`)

```toml
[preset]
name = "rust"
description = "Rust development environment with common tools"
# Layer order for merging (lower = applied first)
order = 10

[packages]
# Always included when preset is active
required = [
    "rustc",
    "cargo",
    "rust-analyzer",
    "rustfmt",
    "clippy",
]

# Shown in UI as suggestions, not auto-selected
optional = [
    "cargo-watch",
    "cargo-edit",
    "cargo-expand",
    "cargo-outdated",
    "cargo-audit",
    "bacon",
    "mold",  # Fast linker
]

[env]
RUST_BACKTRACE = "1"
# Use mold linker if available
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER = "clang"
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "-C link-arg=-fuse-ld=mold"

[shell]
hook = '''
# Rust environment loaded
if command -v rustc &> /dev/null; then
    echo "🦀 Rust $(rustc --version | cut -d' ' -f2)"
fi
'''
```

### Package Index Schema (SQLite)

```sql
-- Package metadata
CREATE TABLE packages (
    id INTEGER PRIMARY KEY,
    attr_path TEXT NOT NULL UNIQUE,  -- e.g., "ripgrep", "python3Packages.requests"
    name TEXT NOT NULL,               -- e.g., "ripgrep"
    version TEXT,                     -- e.g., "14.1.0"
    description TEXT,
    homepage TEXT,
    license TEXT,                     -- JSON array of license names
    platforms TEXT,                   -- JSON array of platforms
    main_program TEXT,                -- Primary binary name
    broken INTEGER DEFAULT 0,
    insecure INTEGER DEFAULT 0
);

-- Full-text search
CREATE VIRTUAL TABLE packages_fts USING fts5(
    attr_path,
    name,
    description,
    content='packages',
    content_rowid='id'
);

-- Triggers to keep FTS in sync
CREATE TRIGGER packages_ai AFTER INSERT ON packages BEGIN
    INSERT INTO packages_fts(rowid, attr_path, name, description)
    VALUES (new.id, new.attr_path, new.name, new.description);
END;

-- Binary/program lookup
CREATE TABLE package_binaries (
    id INTEGER PRIMARY KEY,
    package_id INTEGER NOT NULL REFERENCES packages(id),
    binary_name TEXT NOT NULL
);

CREATE INDEX idx_binaries_name ON package_binaries(binary_name);

-- Index metadata
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Example meta entries:
-- ('nixpkgs_commit', 'a1b2c3d4...')
-- ('nixpkgs_url', 'https://github.com/...')
-- ('generated_at', '2025-02-04T12:00:00Z')
-- ('package_count', '80000')
-- ('mica_version', '0.1.0')
```

### Version History Schema (SQLite)

```sql
-- Track package versions across nixpkgs history
CREATE TABLE package_versions (
    id INTEGER PRIMARY KEY,
    attr_path TEXT NOT NULL,
    version TEXT NOT NULL,
    nixpkgs_commit TEXT NOT NULL,
    commit_date TEXT NOT NULL,
    branch TEXT NOT NULL,  -- nixos-unstable, nixos-24.05, etc.
    
    UNIQUE(attr_path, version, branch)
);

CREATE INDEX idx_versions_attr ON package_versions(attr_path);
CREATE INDEX idx_versions_date ON package_versions(commit_date DESC);
CREATE INDEX idx_versions_branch ON package_versions(branch);

-- Track which commits we've indexed
CREATE TABLE indexed_commits (
    commit TEXT PRIMARY KEY,
    branch TEXT NOT NULL,
    commit_date TEXT NOT NULL,
    indexed_at TEXT NOT NULL,
    package_count INTEGER
);
```

---

## Generated Nix File Format

### Project Mode (`default.nix`)

```nix
# Managed by Mica v0.1.0
# Do not edit sections between mica: markers
# Manual additions outside markers will be preserved
# Last generated: 2025-02-04T14:30:00Z

{ pkgs ? import (fetchTarball {
    # mica:pin:begin
    url = "https://github.com/jpetrucciani/nix/archive/a1b2c3d4e5f6g7h8i9j0.tar.gz";
    sha256 = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    # mica:pin:end
  }) {}
}:

let
  # mica:packages:begin
  packages = with pkgs; [
    # Preset: rust
    rustc
    cargo
    rust-analyzer
    rustfmt
    clippy
    
    # Preset: devops
    kubectl
    kubernetes-helm
    terraform
    
    # User additions
    jq
    yq
    httpie
  ];
  # mica:packages:end

in pkgs.buildEnv {
  name = "dev-environment";
  
  buildInputs = packages;
  
  # mica:env:begin
  EDITOR = "nvim";
  RUST_BACKTRACE = "1";
  # mica:env:end
  
  # mica:shellhook:begin
  shellHook = ''
    # Rust environment loaded
    if command -v rustc &> /dev/null; then
        echo "🦀 Rust $(rustc --version | cut -d' ' -f2)"
    fi
    
    echo "Welcome to the dev environment!"
  '';
  # mica:shellhook:end
}
```

### Global Profile Mode (`~/.config/mica/profile.nix`)

```nix
# Managed by Mica v0.1.0
# Global user profile - install with: nix-env -if ~/.config/mica/profile.nix
# Last generated: 2025-02-04T14:30:00Z

let
  # mica:pins:begin
  # Primary nixpkgs
  pkgs = import (fetchTarball {
    url = "https://github.com/jpetrucciani/nix/archive/a1b2c3d4e5f6g7h8i9j0.tar.gz";
    sha256 = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
  }) {};
  
  # Pinned sources for specific versions
  pkgs-nodejs = import (fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/nixos-23.11.tar.gz";
    sha256 = "sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=";
  }) {};
  
  pkgs-python = import (fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/abc123def456.tar.gz";
    sha256 = "sha256-CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC=";
  }) {};
  # mica:pins:end

in pkgs.buildEnv {
  name = "mica-profile";
  
  # mica:paths:begin
  paths = [
    # From primary nixpkgs
    pkgs.ripgrep
    pkgs.fd
    pkgs.bat
    pkgs.eza
    pkgs.zoxide
    
    # Preset: devops
    pkgs.kubectl
    pkgs.kubernetes-helm
    
    # Version-pinned packages
    pkgs-nodejs.nodejs_18  # 18.19.0
    pkgs-python.python311  # 3.11.7
  ];
  # mica:paths:end
  
  pathsToLink = [ "/bin" "/share" ];
  extraOutputsToInstall = [ "man" "doc" ];
}
```

---

## CLI Interface

```
mica - A TUI for managing Nix environments

USAGE:
    mica [OPTIONS] [COMMAND]

COMMANDS:
    (none)          Launch TUI for current directory's default.nix
    init --repo <URL> Initialize state file (uses MICA_NIXPKGS_REPO if set)
    add <pkg>...    Add packages to environment
    remove <pkg>... Remove packages from environment
    search <query>  Search packages (non-interactive)
    list            List currently selected packages
    presets         List available presets
    apply <preset>  Apply a preset
    unapply <preset> Remove a preset
    update --latest Update nixpkgs pin to latest
    update <pkg>    Update specific package version (global mode)
    export          Output standalone nix file to stdout
    sync            Regenerate nix file from state
    diff            Show pending changes
    eval            Validate current configuration
    index           Manage package index
      index status  Show index info
      index rebuild Force rebuild local index
      index fetch   Download latest remote index

OPTIONS:
    -f, --file <PATH>    Target specific nix file
    -d, --dir <PATH>     Target directory (uses default.nix)
    -g, --global         Operate on global profile (~/.config/mica/profile.nix)
    -n, --dry-run        Show what would change without modifying files
    -v, --verbose        Increase verbosity
    -q, --quiet          Suppress non-error output
    -h, --help           Print help
    -V, --version        Print version

EXAMPLES:
    mica                        # Launch TUI for ./default.nix
    mica init                   # Initialize default.nix with latest pin
    MICA_NIXPKGS_REPO=... mica init  # Initialize using a custom repo
    mica -g                     # Launch TUI for global profile
    mica add ripgrep fd jq      # Add packages to current project
    mica -g add ripgrep         # Add to global profile
    mica search "json parser"   # Search packages
    mica apply rust devops      # Apply presets
    mica update --latest        # Update nixpkgs pin (sha auto-computed)
    mica -g update nodejs       # Update nodejs version in global profile
    mica export > env.nix       # Export standalone file

Note: If the index DB is missing, `mica tui` attempts to build it from `nix-env -qaP --json`.
```

---

## TUI Design

### Main Layout

```
┌─ Mica ──────────────────────────────────────────────────────────────────────┐
│ Mode: [P]roject  Pin: jpetrucciani/nix@a1b2c3d (2025-02-04)  Pkgs: 12      │
├─────────────────────────────────────────────────────────────────────────────┤
│ Search: rg█                                                    [Tab] Details│
├─────────────────────────────────────────────────────────────────────────────┤
│ ┌─ Packages (2 matches) ───────────────────┬─ Details ─────────────────────┐│
│ │  ▶ [x] ripgrep                  14.1.0  │ ripgrep                        ││
│ │    [ ] ripgrep-all              1.0.0   │                                ││
│ │                                         │ rg - recursively searches      ││
│ │                                         │ directories for a regex        ││
│ │                                         │ pattern while respecting       ││
│ │                                         │ gitignore rules.               ││
│ │                                         │                                ││
│ │                                         │ Version:  14.1.0               ││
│ │                                         │ License:  MIT                  ││
│ │                                         │ Homepage: github.com/BurntSu...││
│ │                                         │ Binaries: rg                   ││
│ │                                         │                                ││
│ │                                         │ [v] Pick version               ││
│ └─────────────────────────────────────────┴────────────────────────────────┘│
│ ┌─ Selected (12) ──────────────────────────────────────────────────────────┐│
│ │ ripgrep, fd, jq, yq, rustc, cargo, rust-analyzer, kubectl, helm, ...     ││
│ └──────────────────────────────────────────────────────────────────────────┘│
│ ┌─ Presets ────────────────────────────────────────────────────────────────┐│
│ │ [x] rust (5 pkgs)  [x] devops (8 pkgs)  [ ] python  [ ] node  [ ] go     ││
│ └──────────────────────────────────────────────────────────────────────────┘│
├─────────────────────────────────────────────────────────────────────────────┤
│ [S]ave  [E]val  [U]pdate  [D]iff  [/]Search  [p]resets  [?]Help     [q]uit │
└─────────────────────────────────────────────────────────────────────────────┘
```

Current implementation notes:
- Two-column layout with a packages table on the left and preset search/list/details plus a changes panel on the right.
- Package filters (broken/insecure/license/platform) are shown in the search title and toggled via shortcuts.
- `?` opens a help modal with the full key map.

### Version Picker (Modal)

```
┌─ Select Version: nodejs ────────────────────────────────────────────────────┐
│                                                                             │
│  Source                    Version     Date                                 │
│  ───────────────────────────────────────────────────────────────────────── │
│  ▶ jpetrucciani/nix@main   22.11.0     2025-02-01                          │
│    nixos-unstable          22.9.0      2025-01-15                          │
│    nixos-24.05             20.18.0     2024-11-20                          │
│    nixos-23.11             18.19.0     2024-05-15                          │
│    nixos-23.05             18.16.1     2023-11-01                          │
│                                                                             │
│  [Enter] Select  [Esc] Cancel                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Diff View (Modal)

```
┌─ Pending Changes ───────────────────────────────────────────────────────────┐
│                                                                             │
│  Packages:                                                                  │
│    + ripgrep (14.1.0)                                                       │
│    + fd (9.0.0)                                                             │
│    - httpie (removed)                                                       │
│                                                                             │
│  Presets:                                                                   │
│    + rust                                                                   │
│                                                                             │
│  Environment:                                                               │
│    + RUST_BACKTRACE=1                                                       │
│                                                                             │
│  [S]ave  [Esc] Cancel                                                       │
└─────────────────────────────────────────────────────────────────────────────┘
```

Current diff modal shows a unified diff between the generated nix and the on-disk file, with `+` and `-` lines and scroll support.

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| Type | Search in focused panel |
| `Ctrl+U` | Clear current search |
| `Enter` / `Space` | Toggle selection |
| `Tab` | Switch focus |
| `↑` / `↓` | Move selection |
| `s` | Save changes |
| `q` / `Esc` | Quit |
| `?` | Show help |
| `B` | Toggle broken filter |
| `I` | Toggle insecure filter |
| `L` | Edit license filter |
| `P` | Edit platform filter |
| `E` | Edit environment variables |
| `H` | Edit shell hook |
| `D` | Preview diff |
| `U` | Update nixpkgs pin |
| `R` | Rebuild index |
| `Y` | Reload from nix |

---

## Core Algorithms

### Preset Merging

```rust
/// Merge multiple presets in order, producing final package list
fn merge_presets(presets: &[Preset], state: &ProjectState) -> MergedResult {
    let mut packages = IndexSet::new();
    let mut env = HashMap::new();
    let mut shell_hooks = Vec::new();
    
    // Sort presets by order field
    let sorted: Vec<_> = presets.iter()
        .sorted_by_key(|p| p.order)
        .collect();
    
    // Apply each preset
    for preset in sorted {
        for pkg in &preset.packages.required {
            packages.insert(pkg.clone());
        }
        env.extend(preset.env.clone());
        if let Some(hook) = &preset.shell.hook {
            shell_hooks.push(hook.clone());
        }
    }
    
    // Apply user additions
    for pkg in &state.packages.added {
        packages.insert(pkg.clone());
    }
    
    // Apply user removals
    for pkg in &state.packages.removed {
        packages.shift_remove(pkg);
    }
    
    MergedResult { packages, env, shell_hooks }
}
```

### Nix File Parsing Strategy

```rust
/// Attempt to parse a nix file and extract mica-managed state
fn parse_nix_file(content: &str) -> Result<ParsedNix, ParseError> {
    // 1. Check for mica header comment
    if !content.starts_with("# Managed by Mica") {
        return Err(ParseError::NotMicaManaged);
    }
    
    // 2. Parse with rnix
    let root = rnix::Root::parse(content);
    if !root.errors().is_empty() {
        return Err(ParseError::NixSyntax(root.errors()));
    }
    
    // 3. Extract marker sections via string search
    // (More reliable than AST walking for our controlled format)
    let pin = extract_between_markers(content, "mica:pin:begin", "mica:pin:end")?;
    let packages = extract_between_markers(content, "mica:packages:begin", "mica:packages:end")?;
    let env = extract_between_markers(content, "mica:env:begin", "mica:env:end")?;
    let shell_hook = extract_between_markers(content, "mica:shellhook:begin", "mica:shellhook:end")?;
    
    // 4. Parse each section
    Ok(ParsedNix {
        pin: parse_pin_section(&pin)?,
        packages: parse_package_list(&packages)?,
        env: parse_env_section(&env)?,
        shell_hook: parse_shell_hook(&shell_hook)?,
        // Preserve content outside markers for re-emission
        preamble: extract_before_marker(content, "mica:pin:begin"),
        postamble: extract_after_marker(content, "mica:shellhook:end"),
    })
}
```

### Package Search

```rust
/// Search packages using SQLite FTS5
fn search_packages(
    db: &Connection,
    query: &str,
    mode: SearchMode,
    limit: usize,
) -> Result<Vec<PackageInfo>> {
    let sql = match mode {
        SearchMode::All => r#"
            SELECT p.*, rank
            FROM packages p
            JOIN packages_fts fts ON p.id = fts.rowid
            WHERE packages_fts MATCH ?1
            ORDER BY rank
            LIMIT ?2
        "#,
        SearchMode::Name => r#"
            SELECT p.*, rank
            FROM packages p
            JOIN packages_fts fts ON p.id = fts.rowid
            WHERE packages_fts MATCH 'name:' || ?1
            ORDER BY rank
            LIMIT ?2
        "#,
        SearchMode::Binary => r#"
            SELECT DISTINCT p.*
            FROM packages p
            JOIN package_binaries b ON p.id = b.package_id
            WHERE b.binary_name LIKE ?1 || '%'
            LIMIT ?2
        "#,
    };
    
    // Prepare query for FTS (add * for prefix matching)
    let fts_query = format!("{}*", query.replace(" ", " OR "));
    
    let mut stmt = db.prepare(sql)?;
    let rows = stmt.query_map([&fts_query, &limit.to_string()], |row| {
        Ok(PackageInfo {
            attr_path: row.get(1)?,
            name: row.get(2)?,
            version: row.get(3)?,
            description: row.get(4)?,
            // ...
        })
    })?;
    
    rows.collect()
}
```

### Nix Evaluation/Validation

```rust
/// Validate that the current state produces a valid nix expression
async fn validate_state(state: &ProjectState) -> Result<ValidationResult> {
    // 1. Generate nix to temp file
    let temp_dir = tempfile::tempdir()?;
    let temp_nix = temp_dir.path().join("validate.nix");
    let content = generate_nix(state)?;
    fs::write(&temp_nix, &content)?;
    
    // 2. Try nix-instantiate --eval
    let eval_output = Command::new("nix-instantiate")
        .args(["--eval", "--strict", "--json"])
        .arg(&temp_nix)
        .output()
        .await?;
    
    if !eval_output.status.success() {
        let stderr = String::from_utf8_lossy(&eval_output.stderr);
        return Ok(ValidationResult::EvalError(parse_nix_error(&stderr)));
    }
    
    // 3. Optionally, try dry-run build
    let build_output = Command::new("nix-build")
        .args(["--dry-run"])
        .arg(&temp_nix)
        .output()
        .await?;
    
    if !build_output.status.success() {
        let stderr = String::from_utf8_lossy(&build_output.stderr);
        return Ok(ValidationResult::BuildError(parse_nix_error(&stderr)));
    }
    
    Ok(ValidationResult::Valid)
}
```

---

## Implementation Phases

### Phase 1: Foundation (Week 1-2)

**Deliverables:**
- [ ] Cargo workspace structure
- [ ] State types with serde (de)serialization
- [ ] TOML config loading
- [ ] Basic Nix file generator (template-based)
- [ ] CLI skeleton with clap (subcommands stubbed)
- [ ] Integration tests for state round-trip

**Acceptance Criteria:**
- `mica --version` prints version
- Can load/save `.mica.toml` and `config.toml`
- Can generate valid `default.nix` from state

### Phase 2: Package Index (Week 2-3)

**Deliverables:**
- [ ] SQLite schema and migrations
- [ ] Index generator from `nix-env -qaP --json`
- [ ] FTS5 search implementation
- [ ] Binary lookup table population
- [ ] Remote index fetching (S3)
- [ ] Local index caching

**Acceptance Criteria:**
- `mica index rebuild` generates working index
- `mica search ripgrep` returns results in <100ms
- Index cached and reused across invocations

### Phase 3: TUI MVP (Week 3-4)

**Deliverables:**
- [ ] ratatui application scaffold
- [ ] Package list view with search
- [ ] Selection state management
- [ ] Details pane
- [ ] Save flow (state -> nix file)
- [ ] Basic keyboard navigation

**Acceptance Criteria:**
- Can launch TUI, search packages, select, save
- Generated `default.nix` works with `nix-shell`
- Responsive search (<50ms keystroke latency)

### Phase 4: Presets (Week 4-5)

**Deliverables:**
- [ ] Preset TOML loader
- [ ] Bundled presets (rust, python, node, go, devops, data)
- [ ] Preset merging logic
- [ ] TUI preset selector panel
- [ ] CLI `apply`/`unapply` commands

**Acceptance Criteria:**
- `mica apply rust` adds rust tooling
- Presets merge correctly (order, override)
- TUI shows preset status and package counts

### Phase 5: Global Profile Mode (Week 5-6)

**Deliverables:**
- [ ] Profile state management (`~/.config/mica/profile.toml`)
- [ ] Multi-source pin support
- [ ] Profile nix generator (buildEnv style)
- [ ] `nix-env -if` integration
- [ ] Generation tracking
- [ ] `-g` flag throughout CLI/TUI

**Acceptance Criteria:**
- `mica -g add ripgrep` updates global profile
- `mica -g` launches TUI in profile mode
- Can pin specific package versions from different nixpkgs

### Phase 6: Version History (Week 6-7)

**Deliverables:**
- [ ] Version history schema
- [ ] Backfill script for historical versions
- [ ] Incremental update pipeline
- [ ] TUI version picker modal
- [ ] CLI `update <pkg>` command

**Acceptance Criteria:**
- Can see available versions for a package
- Can pin specific version from history
- Incremental updates work in CI

### Phase 7: Polish (Week 7-8)

**Deliverables:**
- [ ] Nix file parsing with rnix (read existing files)
- [ ] Drift detection (state vs file mismatch)
- [ ] `sync` command to reconcile
- [ ] `eval` command for validation
- [ ] `diff` command/view
- [ ] `export` command
- [ ] Error handling improvements
- [ ] Help screens in TUI
- [ ] Documentation (README, man page)

**Acceptance Criteria:**
- Gracefully handles hand-edited files
- Clear error messages for common issues
- Complete documentation

---

## Dependencies

### Rust Crates

```toml
[workspace.dependencies]
# CLI
clap = { version = "4", features = ["derive"] }

# TUI
ratatui = "0.29"
crossterm = "0.28"

# Async
tokio = { version = "1", features = ["full"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"

# Database
rusqlite = { version = "0.32", features = ["bundled", "fts5"] }

# Nix parsing
rnix = "0.11"

# HTTP (for remote index fetch)
reqwest = { version = "0.12", features = ["json", "gzip"] }

# Error handling
thiserror = "2"
miette = { version = "7", features = ["fancy"] }
color-eyre = "0.6"

# Utilities
directories = "5"          # XDG paths
tempfile = "3"
chrono = { version = "0.4", features = ["serde"] }
indexmap = { version = "2", features = ["serde"] }
itertools = "0.13"
tracing = "0.1"
tracing-subscriber = "0.3"

# Fuzzy search (optional, for TUI search)
nucleo = "0.5"
```

### System Requirements

- Nix (>= 2.4) with `nix-env`, `nix-instantiate`, `nix-build`
- SQLite 3 (bundled via rusqlite)
- Terminal with 256-color support

---

## Testing Strategy

### Unit Tests
- State serialization round-trip
- Preset merging logic
- Nix generation output
- Search query construction

### Integration Tests
- Full CLI workflows (`add`, `remove`, `apply`, etc.)
- Index generation from fixture JSON
- Nix file parsing and regeneration

### End-to-End Tests
- Generate index from real nixpkgs (CI only, slow)
- Build generated nix files with `nix-build`
- TUI smoke test with simulated input

### Test Fixtures
- Sample `.mica.toml` files
- Sample `default.nix` files (tool-generated and hand-edited)
- Minimal nixpkgs JSON for index tests

---

## Open Questions / Future Work

1. **Conflict detection**: How do we identify packages that conflict? Nixpkgs doesn't expose this cleanly. May need heuristics or curated list.

2. **Flake support**: Add `--flake` mode that generates `flake.nix` instead of `default.nix`?

3. **Remote presets**: Allow referencing presets from URLs or git repos?

4. **Team sharing**: Sync presets/configs across team via git? Preset registry?

5. **Shell integration**: Generate shell completions? Integration with starship/prompt?

6. **Update notifications**: Check for newer package versions and notify?

7. **Garbage collection**: Help clean up old index caches, generations?

---

## Appendix: Bundled Presets

### rust.toml
```toml
[preset]
name = "rust"
description = "Rust development environment"
order = 10

[packages]
required = ["rustc", "cargo", "rust-analyzer", "rustfmt", "clippy"]
optional = ["cargo-watch", "cargo-edit", "cargo-expand", "cargo-audit", "bacon", "mold"]

[env]
RUST_BACKTRACE = "1"
```

### python.toml
```toml
[preset]
name = "python"
description = "Python development environment"
order = 10

[packages]
required = ["python3", "python3Packages.pip", "python3Packages.virtualenv"]
optional = ["python3Packages.ipython", "python3Packages.black", "python3Packages.ruff", "python3Packages.mypy", "python3Packages.pytest", "uv"]

[env]
PYTHONDONTWRITEBYTECODE = "1"
```

### node.toml
```toml
[preset]
name = "node"
description = "Node.js development environment"
order = 10

[packages]
required = ["nodejs", "nodePackages.npm"]
optional = ["nodePackages.pnpm", "yarn", "nodePackages.typescript", "nodePackages.ts-node"]
```

### go.toml
```toml
[preset]
name = "go"
description = "Go development environment"
order = 10

[packages]
required = ["go", "gopls", "gotools"]
optional = ["golangci-lint", "delve", "go-migrate"]

[env]
CGO_ENABLED = "0"
```

### devops.toml
```toml
[preset]
name = "devops"
description = "DevOps and infrastructure tools"
order = 20

[packages]
required = ["kubectl", "kubernetes-helm", "terraform", "jq", "yq"]
optional = ["k9s", "kubectx", "stern", "argocd", "vault", "awscli2", "google-cloud-sdk"]
```

### data.toml
```toml
[preset]
name = "data"
description = "Data engineering and analysis tools"
order = 20

[packages]
required = ["duckdb", "sqlite", "jq", "miller"]
optional = ["postgresql", "clickhouse", "xsv", "htmlq", "pup"]
```

---

## Glossary

- **Pin**: A specific nixpkgs commit used as the package source
- **Preset**: A predefined collection of packages and configuration
- **Profile**: The user's global set of installed packages (via `nix-env`)
- **Generation**: A snapshot of the profile state at a point in time
- **Index**: Pre-computed SQLite database of package metadata for fast search
- **attr_path**: The Nix attribute path to a package (e.g., `ripgrep`, `python3Packages.requests`)
