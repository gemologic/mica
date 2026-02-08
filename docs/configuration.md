# Configuration

Configuration lives at:

`~/.config/mica/config.toml`

## Example

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
search_mode = "all" # name | description | binary | all

[tui.columns]
version = true
description = true
license = false
platforms = false
main_program = false
```

## Repo Override for Init

You can override the repo used by `mica init`:

- CLI: `mica init --repo <url>`
- Environment: `MICA_NIXPKGS_REPO=<url>`

