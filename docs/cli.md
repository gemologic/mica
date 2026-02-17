# CLI Reference

## Top-level Commands

```text
tui, init, list, presets, add, remove, search, env, shell,
apply, unapply, update, pin, generations, export, index, sync, eval, diff, completion
```

See full help:

```bash
mica --help
```

## Common Commands

```bash
# initialize and launch
mica init
mica tui

# package management
mica add ripgrep fd
mica remove fd

# preset management
mica presets
mica apply rust
mica unapply rust

# search
mica search ripgrep
mica search rg --mode binary
```

## Target Selection (`--file`, `--global`)

```bash
# operate on a specific managed nix file
mica --file ./default.nix list
mica --file ./default.nix diff
mica --file ./default.nix sync

# operate on the global profile
mica --global list
mica --global add ripgrep
mica --global generations list
```

## Search Query Shortcuts

Shortcuts work in both CLI search and the TUI package search box:

- `'` prefix means exact match
- `bin:` targets binary/main program names
- `name:` targets package/attr names
- `desc:` targets descriptions
- `all:` resets to mixed mode

Examples:

```bash
mica search "'bin:rg"
mica search "name:ripgrep"
mica search "'desc:fast grep"
```

## Pinning

```bash
# update the primary nixpkgs pin
mica update --latest

# update a pinned package source
mica update nodejs --latest
```

Advanced pin workflows are available via:

```bash
mica pin --help
```

## Index Operations

```bash
mica index status
mica index rebuild /tmp/nixpkgs.json
mica index rebuild-local ~/dev/jpetrucciani-nix --skip-attr home-packages,watcher --show-trace
mica index fetch
```

With `index.remote_url` set to a base URL, mica fetches `<remote>/<nixpkgs_commit>.db`; if it is missing, mica rebuilds locally.

## Validation and Drift

```bash
mica eval
mica diff
mica sync
mica sync --from-nix
```

## Global Profile

```bash
mica --global list
mica --global add ripgrep
mica --global generations list
mica --global generations rollback
```

## Shell Completions

```bash
mica completion bash
mica completion zsh
mica completion fish
```
