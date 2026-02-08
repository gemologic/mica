---
layout: home

hero:
  name: 'mica'
  text: 'Manage Nix environments without drowning in boilerplate'
  tagline: Interactive TUI plus CLI workflows for packages, presets, pins, and reproducible default.nix files.
  actions:
    - theme: brand
      text: Get Started
      link: /getting-started
    - theme: alt
      text: View on GitHub
      link: https://github.com/gemologic/mica

features:
  - icon: 🧭
    title: TUI-first Workflow
    details: Search packages, toggle presets, preview diffs, and save changes in one place.
  - icon: 🧩
    title: Presets and Overrides
    details: Compose project defaults with bundled or custom presets, then layer project-specific overrides.
  - icon: 📌
    title: Pin Management
    details: Update nixpkgs revisions with repeatable, state-driven workflows.
  - icon: ⚡
    title: Local Index for Fast Search
    details: Build and reuse a SQLite package index for quick package discovery, including binary-name lookups.
  - icon: 🔁
    title: Drift Detection and Sync
    details: Compare current state to generated nix output and regenerate safely when needed.
  - icon: 🧱
    title: Nix-friendly by Design
    details: Keep a managed default.nix while preserving manual content outside mica markers.
---
