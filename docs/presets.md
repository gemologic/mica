# Presets

Mica ships with bundled presets in `presets/` and can load additional presets from directories listed in `presets.extra_dirs`.

## Inspect Available Presets

```bash
mica presets
```

## Apply and Remove

```bash
mica apply rust
mica unapply rust
```

## Preset File Format

Example:

```toml
[preset]
name = "my-stack"
description = "My project baseline"
order = 20

[packages]
required = ["ripgrep", "fd"]
optional = ["jq"]

[env]
RUST_BACKTRACE = "1"

[shell]
hook = "echo ready"

[nix]
let = '''
myVar = "value";
'''
scripts = '''
hello = writers.writeBashBin "hello" "echo hi";
'''
```

## Merge Behavior

- Presets are ordered by `preset.order`
- Required package lists are merged in order
- Removed packages in project state are respected
- Project-level env and shell settings override preset values

