# TUI Guide

## Core Navigation

- `Tab` cycles focus between packages, presets, and changes
- Arrow keys move selection
- `Enter` or `Space` toggles selected item
- `Ctrl+S` saves changes
- `Ctrl+Q` quits
- `?` opens help

## Package Search

- Type to search in the focused package panel
- `Ctrl+U` clears query
- `S` cycles search mode: `all`, `name`, `desc`, `bin`
- Query shortcuts:
  - `'` exact
  - `bin:`, `name:`, `desc:`, `all:`
  - Example: `'bin:rg`

## Filters

- `B` toggle broken filter
- `I` toggle insecure filter
- `V` toggle installed-only view
- `L` edit license filter
- `O` edit platform filter

## Information and Diff

- `Ctrl+P` package info overlay
- `Ctrl+V` version picker overlay
- `D` open diff preview
- In diff overlay: `T` toggles full vs changes-only
- `K` toggles details panel visibility

## Editing and Pin Actions

- `U` update primary pin to latest revision
- `E` edit environment variables (`Tab` toggles value mode: string vs nix expression)
- `H` edit shell hook
- `R` rebuild index
- `Y` reload state from nix

## Panel Layout

- `T` toggles the presets panel
- `C` toggles the changes panel
- `M` opens columns configuration
