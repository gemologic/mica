# Troubleshooting

## `nix state parse error: not a mica-managed nix file`

You are targeting a `default.nix` that is not managed by mica markers.

Fix:

1. Run `mica init` in that directory, or
2. Point mica at a file/directory that already has mica markers with `--file` or `--dir`.

## `building index failed`

This usually means nix evaluation failed for one or more attrs while indexing.

Try:

```bash
MICA_NIX_SHOW_TRACE=1 mica index rebuild /tmp/nixpkgs.json
```

For local repo evaluation with built-in retry/skip behavior:

```bash
mica index rebuild-local ~/dev/jpetrucciani-nix --skip-attr home-packages,watcher --show-trace
```

For pin-driven rebuild diagnostics:

```bash
MICA_KEEP_INDEX_NIX=1 MICA_NIX_SHOW_TRACE=1 mica tui
```

Then inspect the error and optionally skip offenders with:

```bash
MICA_NIX_SKIP_ATTRS=attrA,attrB mica tui
```

## `nix-prefetch-url not found` or `nix-env not found`

Install Nix and ensure these commands are in `PATH`.

## GitHub API errors while updating pins

`mica update --latest` resolves revision through GitHub API. Network issues or rate limits can fail this step.

Workarounds:

- Retry later
- Provide explicit `--rev` and `--sha256`
- Use a reachable mirror/repo URL

## Preview Current Effective State

Use these commands to understand what mica currently sees:

```bash
mica list
mica diff
mica export
```
