# Getting Started

## Prerequisites

- Nix installed with `nix-env` and `nix-prefetch-url` available in `PATH`
- Rust toolchain if you are running from source
- Optional: `direnv` for shell ergonomics

## Run From Source

From the repository root:

```bash
cargo run -p mica -- init
cargo run -p mica -- tui
```

`init` creates a mica-managed `default.nix` in the target directory. `tui` opens the interactive interface.

## Use From a Nix Flake

Example dev shell consuming this repository directly:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    mica-src.url = "github:gemologic/mica";
  };

  outputs = { self, nixpkgs, mica-src, ... }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      mica = import mica-src { inherit pkgs; };
    in {
      devShells.${system}.default = pkgs.mkShell {
        nativeBuildInputs = [
          mica.bin
        ];
      };
    };
}
```

## Quick Flow

1. `mica init`
2. `mica tui`
3. Search and toggle packages/presets
4. `Ctrl+S` to save
5. `mica diff` to inspect drift when needed

## Project vs Global Mode

- Project mode (default): manages `./default.nix`
- Global mode (`--global`): manages `~/.config/mica/profile.toml` and `~/.config/mica/profile.nix`

Common targeting examples:

```bash
# target one specific managed file
mica --file ./default.nix list
mica --file ./default.nix diff

# operate on global profile state
mica --global list
mica --global add ripgrep
mica --global generations list
```

Use `mica --help` to see global options:

- `-g, --global`
- `-f, --file <PATH>`
- `-d, --dir <PATH>`
- `-n, --dry-run`
- `-v, --verbose`
- `-q, --quiet`
