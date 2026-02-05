use crate::preset::{MergedProfileResult, MergedResult};
use crate::state::{GlobalProfileState, ProjectState};
use chrono::{DateTime, Utc};
use std::collections::HashSet;

pub fn generate_project_nix(
    state: &ProjectState,
    merged: &MergedResult,
    project_name: &str,
    generated_at: DateTime<Utc>,
) -> String {
    let mut output = String::new();
    output.push_str("# Managed by Mica v0.1.0\n");
    output.push_str("# Do not edit sections between mica: markers\n");
    output.push_str("# Manual additions outside markers will be preserved\n");
    output.push_str(&format!(
        "# Last generated: {}\n\n",
        generated_at.to_rfc3339()
    ));

    output.push_str("{ pkgs ? import (fetchTarball {\n");
    output.push_str("    # mica:pin:begin\n");
    if let Some(name) = &state.pin.name {
        output.push_str(&format!("    name = \"{}\";\n", escape_nix_string(name)));
    }
    output.push_str(&format!(
        "    url = \"{}/archive/{}.tar.gz\";\n",
        state.pin.url, state.pin.rev
    ));
    output.push_str(&format!("    sha256 = \"{}\";\n", state.pin.sha256));
    output.push_str("    # mica:pin:end\n");
    output.push_str("  }) {}\n");
    output.push_str("  # mica:pins:begin\n");
    let state_pin_names: HashSet<String> = state.pins.keys().cloned().collect();
    for (name, pin) in &state.pins {
        let name = sanitize_nix_identifier(name);
        output.push_str(&format!("  , {} ? import (fetchTarball {{\n", name));
        if let Some(fetch_name) = &pin.name {
            output.push_str(&format!(
                "      name = \"{}\";\n",
                escape_nix_string(fetch_name)
            ));
        }
        output.push_str(&format!(
            "      url = \"{}/archive/{}.tar.gz\";\n",
            pin.url, pin.rev
        ));
        output.push_str(&format!("      sha256 = \"{}\";\n", pin.sha256));
        output.push_str("    }) {}\n");
    }
    let mut filtered_pin_blocks = Vec::new();
    for block in &merged.pin_blocks {
        if let Some(name) = extract_pin_name_from_block(block) {
            if state_pin_names.contains(&name) {
                continue;
            }
        }
        filtered_pin_blocks.push(block.clone());
    }
    write_blocks(&mut output, "  ", &filtered_pin_blocks);
    output.push_str("  # mica:pins:end\n");
    output.push_str("}:\n\n");

    output.push_str("let\n");
    output.push_str(&format!(
        "  name = \"{}\";\n\n",
        escape_nix_string(project_name)
    ));
    output.push_str("  # mica:let:begin\n");
    write_blocks(&mut output, "  ", &merged.let_blocks);
    output.push_str("  # mica:let:end\n\n");
    output.push_str("  scripts = with pkgs; {\n");
    output.push_str("    # mica:scripts:begin\n");
    write_blocks(&mut output, "    ", &merged.scripts_blocks);
    output.push_str("    # mica:scripts:end\n");
    output.push_str("  };\n\n");
    output.push_str("  # mica:packages:begin\n");
    output.push_str("  tools = with pkgs; [\n");
    for group in &merged.preset_packages {
        output.push_str(&format!("    # Preset: {}\n", group.preset));
        for pkg in &group.packages {
            output.push_str(&format!("    {}\n", pkg));
        }
        output.push('\n');
    }
    if !merged.user_packages.is_empty() {
        output.push_str("    # User additions\n");
        for pkg in &merged.user_packages {
            output.push_str(&format!("    {}\n", pkg));
        }
    }
    output.push_str("    # mica:packages-raw:begin\n");
    write_blocks(&mut output, "    ", &merged.packages_raw_blocks);
    output.push_str("    # mica:packages-raw:end\n");
    output.push_str("  ] ++ (pkgs.lib.attrsets.attrValues scripts);\n");
    output.push_str("  # mica:packages:end\n\n");
    output.push_str("  paths = pkgs.lib.flatten [ tools ];\n");
    output.push_str("  env = pkgs.buildEnv {\n");
    output.push_str("    inherit name paths; buildInputs = paths;\n");
    output.push_str("    # mica:env:begin\n");
    for (key, value) in &merged.env {
        output.push_str(&format!("    {} = \"{}\";\n", key, value));
    }
    output.push_str("    # mica:env-raw:begin\n");
    write_blocks(&mut output, "    ", &merged.env_raw_blocks);
    output.push_str("    # mica:env-raw:end\n");
    output.push_str("    # mica:env:end\n\n");
    output.push_str("    # mica:shellhook:begin\n");
    if !merged.shell_hooks.is_empty() {
        output.push_str("    shellHook = ''\n");
        for (idx, hook) in merged.shell_hooks.iter().enumerate() {
            if idx > 0 {
                output.push('\n');
            }
            output.push_str(hook);
            if !hook.ends_with('\n') {
                output.push('\n');
            }
        }
        output.push_str("    '';\n");
    } else {
        output.push_str("    shellHook = ''\n    '';\n");
    }
    output.push_str("    # mica:shellhook:end\n");
    output.push_str("  };\n");
    output.push_str("in\n");
    output.push_str("env.overrideAttrs (prev: {\n");
    output.push_str("  # mica:override:begin\n");
    write_blocks(&mut output, "  ", &merged.override_blocks);
    output.push_str("  # mica:override:end\n");
    if !merged.override_shellhook_blocks.is_empty() {
        output.push_str("  # mica:override-shellhook:begin\n");
        output.push_str("  shellHook = ''\n");
        output.push_str("    ${prev.shellHook or \"\"}\n");
        write_blocks(&mut output, "    ", &merged.override_shellhook_blocks);
        output.push_str("  '';\n");
        output.push_str("  # mica:override-shellhook:end\n");
    }
    output.push_str("}\n");
    output.push_str("  # mica:override-merge:begin\n");
    write_blocks(&mut output, "  ", &merged.override_merge_blocks);
    output.push_str("  # mica:override-merge:end\n");
    output.push_str("  // { inherit scripts; }\n");
    output.push_str(")\n");

    output
}

fn escape_nix_string(value: &str) -> String {
    let mut out = value.replace('\\', "\\\\").replace('\"', "\\\"");
    if out.contains("${") {
        out = out.replace("${", "\\${");
    }
    out
}

pub fn generate_profile_nix(
    state: &GlobalProfileState,
    merged: &MergedProfileResult,
    generated_at: DateTime<Utc>,
) -> String {
    let mut output = String::new();
    output.push_str("# Managed by Mica v0.1.0\n");
    output
        .push_str("# Global user profile - install with: nix-env -if ~/.config/mica/profile.nix\n");
    output.push_str(&format!(
        "# Last generated: {}\n\n",
        generated_at.to_rfc3339()
    ));

    output.push_str("let\n");
    output.push_str("  # mica:pins:begin\n");
    output.push_str("  # Primary nixpkgs\n");
    output.push_str("  pkgs = import (fetchTarball {\n");
    if let Some(name) = &state.pin.name {
        output.push_str(&format!("    name = \"{}\";\n", escape_nix_string(name)));
    }
    output.push_str(&format!(
        "    url = \"{}/archive/{}.tar.gz\";\n",
        state.pin.url, state.pin.rev
    ));
    output.push_str(&format!("    sha256 = \"{}\";\n", state.pin.sha256));
    output.push_str("  }) {};\n");
    for (attr, pinned) in &state.packages.pinned {
        let var_name = sanitize_var_name(attr);
        output.push_str(&format!("\n  # Pin for {}\n", attr));
        output.push_str(&format!("  pkgs-{} = import (fetchTarball {{\n", var_name));
        if let Some(name) = &pinned.pin.name {
            output.push_str(&format!("    name = \"{}\";\n", escape_nix_string(name)));
        }
        output.push_str(&format!(
            "    url = \"{}/archive/{}.tar.gz\";\n",
            pinned.pin.url, pinned.pin.rev
        ));
        output.push_str(&format!("    sha256 = \"{}\";\n", pinned.pin.sha256));
        output.push_str("  }) {};\n");
    }
    output.push_str("  # mica:pins:end\n\n");

    output.push_str("in pkgs.buildEnv {\n");
    output.push_str("  name = \"mica-profile\";\n\n");
    output.push_str("  # mica:paths:begin\n");
    output.push_str("  paths = [\n");
    for group in &merged.preset_packages {
        output.push_str(&format!("    # Preset: {}\n", group.preset));
        for pkg in &group.packages {
            output.push_str(&format!("    pkgs.{}\n", pkg));
        }
        output.push('\n');
    }
    if !merged.user_packages.is_empty() {
        output.push_str("    # User additions\n");
        for pkg in &merged.user_packages {
            output.push_str(&format!("    pkgs.{}\n", pkg));
        }
    }
    for (attr, pinned) in &state.packages.pinned {
        let var_name = sanitize_var_name(attr);
        output.push_str(&format!(
            "    pkgs-{}.{}  # {}\n",
            var_name, attr, pinned.version
        ));
    }
    output.push_str("  ];\n");
    output.push_str("  # mica:paths:end\n\n");
    output.push_str("  pathsToLink = [ \"/bin\" \"/share\" ];\n");
    output.push_str("  extraOutputsToInstall = [ \"man\" \"doc\" ];\n");
    output.push_str("}\n");

    output
}

fn sanitize_var_name(name: &str) -> String {
    sanitize_nix_identifier(name)
}

fn sanitize_nix_identifier(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        return "_pin".to_string();
    }
    if out
        .chars()
        .next()
        .map(|ch| ch.is_ascii_digit())
        .unwrap_or(false)
    {
        out.insert(0, '_');
    }
    out
}

fn extract_pin_name_from_block(block: &str) -> Option<String> {
    for line in block.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(',') && trimmed.contains("? import (fetchTarball") {
            let rest = trimmed.trim_start_matches(',').trim();
            if let Some((name, _)) = rest.split_once('?') {
                return Some(name.trim().to_string());
            }
        }
    }
    None
}

fn write_blocks(output: &mut String, indent: &str, blocks: &[String]) {
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        for line in block.lines() {
            output.push_str(indent);
            output.push_str(line);
            output.push('\n');
        }
    }
}
