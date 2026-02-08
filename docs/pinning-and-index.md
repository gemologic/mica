# Pinning and Index

## Primary Pin

The primary nixpkgs pin is stored in state and used for generation and index building.

Update it:

```bash
mica update --latest
```

Or target a specific file/global profile directly:

```bash
mica --file ./default.nix update --latest
mica --global update --latest
```

For explicit pin values:

```bash
mica update --url https://github.com/jpetrucciani/nix --branch main --rev <rev> --sha256 <sha>
```

## Advanced Pins (Optional)

Extra pin workflows are available, but most users can ignore them:

```bash
mica pin --help
```

## Package Index

Mica maintains a local SQLite index at:

`~/.config/mica/cache/index.db`

Useful commands:

```bash
mica index status
mica index rebuild /tmp/nixpkgs.json
mica index rebuild-local ~/dev/jpetrucciani-nix --skip-attr home-packages,watcher --show-trace
mica index fetch
```

## Versions Database

Mica also tracks package version history in:

`~/.config/mica/cache/versions.db`

This powers version-aware workflows in the TUI.

## Index-related Environment Variables

- `MICA_KEEP_INDEX_NIX=1` keeps temporary index input files for debugging
- `MICA_NIX_SKIP_ATTRS=a,b,c` skips problematic attrs when evaluating index sources
- `MICA_NIX_SHOW_TRACE=1` enables `--show-trace` for nix evaluation
