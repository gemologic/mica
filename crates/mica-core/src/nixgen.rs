use crate::preset::{MergedProfileResult, MergedResult};
use crate::state::{GlobalProfileState, PinnedPackage, ProjectState, NIX_EXPR_PREFIX};
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, HashSet};

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
    let pinned_var_names = build_pinned_var_names(&state.packages.pinned);
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
    for (attr, pinned) in &state.packages.pinned {
        let var_name = pinned_var_names
            .get(attr)
            .cloned()
            .unwrap_or_else(|| sanitize_var_name(attr));
        output.push_str(&format!(
            "  , pkgs-{} ? import (fetchTarball {{\n",
            var_name
        ));
        if let Some(name) = &pinned.pin.name {
            output.push_str(&format!("      name = \"{}\";\n", escape_nix_string(name)));
        }
        output.push_str(&format!(
            "      url = \"{}/archive/{}.tar.gz\";\n",
            pinned.pin.url, pinned.pin.rev
        ));
        output.push_str(&format!("      sha256 = \"{}\";\n", pinned.pin.sha256));
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
    if !state.packages.pinned.is_empty() {
        output.push_str("    # Pinned packages\n");
        for (attr, pinned) in &state.packages.pinned {
            let var_name = pinned_var_names
                .get(attr)
                .cloned()
                .unwrap_or_else(|| sanitize_var_name(attr));
            output.push_str(&format!(
                "    pkgs-{}.{}  # {}\n",
                var_name, attr, pinned.version
            ));
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
        output.push_str(&format!("    {} = {};\n", key, render_nix_env_value(value)));
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

fn render_nix_env_value(value: &str) -> String {
    if let Some(raw_expression) = value.strip_prefix(NIX_EXPR_PREFIX) {
        return render_raw_nix_expression(raw_expression);
    }
    if is_nix_expression_literal(value) {
        return value.trim().to_string();
    }
    format!("\"{}\"", escape_nix_string(value))
}

fn render_raw_nix_expression(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "\"\"".to_string();
    }
    if trimmed.starts_with("${") {
        return format!("\"{}\"", trimmed);
    }
    trimmed.to_string()
}

fn is_nix_expression_literal(value: &str) -> bool {
    let trimmed = value.trim();
    (trimmed.len() >= 2 && trimmed.starts_with('\"') && trimmed.ends_with('\"'))
        || (trimmed.len() >= 4 && trimmed.starts_with("''") && trimmed.ends_with("''"))
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
    let pinned_var_names = build_pinned_var_names(&state.packages.pinned);
    for (attr, pinned) in &state.packages.pinned {
        let var_name = pinned_var_names
            .get(attr)
            .cloned()
            .unwrap_or_else(|| sanitize_var_name(attr));
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
        let var_name = pinned_var_names
            .get(attr)
            .cloned()
            .unwrap_or_else(|| sanitize_var_name(attr));
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

fn build_pinned_var_names(pinned: &BTreeMap<String, PinnedPackage>) -> BTreeMap<String, String> {
    let mut mapping = BTreeMap::new();
    let mut used = HashSet::new();
    for attr in pinned.keys() {
        let base = sanitize_var_name(attr);
        let mut candidate = base.clone();
        let mut idx = 2usize;
        while used.contains(&candidate) {
            candidate = format!("{}_{}", base, idx);
            idx += 1;
        }
        used.insert(candidate.clone());
        mapping.insert(attr.clone(), candidate);
    }
    mapping
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

#[cfg(test)]
mod tests {
    use crate::nixgen::{generate_profile_nix, generate_project_nix};
    use crate::preset::{MergedProfileResult, MergedResult};
    use crate::state::{
        GenerationsState, GlobalProfileState, MicaMetadata, PackagesState, Pin, PinnedPackage,
        PresetState, ProjectState, ShellState, NIX_EXPR_PREFIX,
    };
    use chrono::{DateTime, NaiveDate, Utc};
    use std::collections::BTreeMap;

    fn timestamp() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-02-06T00:00:00Z")
            .expect("timestamp parse failed")
            .with_timezone(&Utc)
    }

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 2, 6).expect("date parse failed")
    }

    fn base_pin() -> Pin {
        Pin {
            name: None,
            url: "https://github.com/NixOS/nixpkgs".to_string(),
            rev: "deadbeef".to_string(),
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123".to_string(),
            branch: "main".to_string(),
            updated: date(),
        }
    }

    fn pinned_packages() -> BTreeMap<String, PinnedPackage> {
        let pin = base_pin();
        BTreeMap::from([
            (
                "foo-bar".to_string(),
                PinnedPackage {
                    version: "1.0.0".to_string(),
                    pin: pin.clone(),
                },
            ),
            (
                "foo_bar".to_string(),
                PinnedPackage {
                    version: "2.0.0".to_string(),
                    pin,
                },
            ),
        ])
    }

    fn empty_merged_result() -> MergedResult {
        MergedResult {
            preset_packages: Vec::new(),
            user_packages: Vec::new(),
            env: BTreeMap::new(),
            shell_hooks: Vec::new(),
            all_packages: Vec::new(),
            let_blocks: Vec::new(),
            pin_blocks: Vec::new(),
            packages_raw_blocks: Vec::new(),
            scripts_blocks: Vec::new(),
            env_raw_blocks: Vec::new(),
            override_blocks: Vec::new(),
            override_merge_blocks: Vec::new(),
            override_shellhook_blocks: Vec::new(),
        }
    }

    #[test]
    fn project_generation_uses_unique_vars_for_colliding_pinned_attrs() {
        let state = ProjectState {
            mica: MicaMetadata {
                version: "0.1.0".to_string(),
                created: timestamp(),
                modified: timestamp(),
            },
            pin: base_pin(),
            pins: BTreeMap::new(),
            presets: PresetState::default(),
            packages: PackagesState {
                added: Vec::new(),
                removed: Vec::new(),
                pinned: pinned_packages(),
            },
            env: BTreeMap::new(),
            shell: ShellState::default(),
            nix: Default::default(),
        };

        let output = generate_project_nix(
            &state,
            &empty_merged_result(),
            "collision-test",
            timestamp(),
        );

        assert!(output.contains("  , pkgs-foo_bar ? import (fetchTarball {"));
        assert!(output.contains("  , pkgs-foo_bar_2 ? import (fetchTarball {"));
        assert!(output.contains("    pkgs-foo_bar.foo-bar  # 1.0.0"));
        assert!(output.contains("    pkgs-foo_bar_2.foo_bar  # 2.0.0"));
    }

    #[test]
    fn profile_generation_uses_unique_vars_for_colliding_pinned_attrs() {
        let state = GlobalProfileState {
            mica: MicaMetadata {
                version: "0.1.0".to_string(),
                created: timestamp(),
                modified: timestamp(),
            },
            pin: base_pin(),
            presets: PresetState::default(),
            packages: PackagesState {
                added: Vec::new(),
                removed: Vec::new(),
                pinned: pinned_packages(),
            },
            generations: GenerationsState::default(),
        };
        let merged = MergedProfileResult {
            preset_packages: Vec::new(),
            user_packages: Vec::new(),
            all_packages: Vec::new(),
        };

        let output = generate_profile_nix(&state, &merged, timestamp());

        assert!(output.contains("  pkgs-foo_bar = import (fetchTarball {"));
        assert!(output.contains("  pkgs-foo_bar_2 = import (fetchTarball {"));
        assert!(output.contains("    pkgs-foo_bar.foo-bar  # 1.0.0"));
        assert!(output.contains("    pkgs-foo_bar_2.foo_bar  # 2.0.0"));
    }

    #[test]
    fn project_generation_escapes_plain_env_values() {
        let state = ProjectState {
            mica: MicaMetadata {
                version: "0.1.0".to_string(),
                created: timestamp(),
                modified: timestamp(),
            },
            pin: base_pin(),
            pins: BTreeMap::new(),
            presets: PresetState::default(),
            packages: PackagesState::default(),
            env: BTreeMap::new(),
            shell: ShellState::default(),
            nix: Default::default(),
        };

        let mut merged = empty_merged_result();
        merged
            .env
            .insert("MICA_TEST".to_string(), "${HOME}/mica".to_string());

        let output = generate_project_nix(&state, &merged, "env-test", timestamp());

        assert!(output.contains("MICA_TEST = \"\\${HOME}/mica\";"));
    }

    #[test]
    fn project_generation_preserves_nix_expression_env_values() {
        let state = ProjectState {
            mica: MicaMetadata {
                version: "0.1.0".to_string(),
                created: timestamp(),
                modified: timestamp(),
            },
            pin: base_pin(),
            pins: BTreeMap::new(),
            presets: PresetState::default(),
            packages: PackagesState::default(),
            env: BTreeMap::new(),
            shell: ShellState::default(),
            nix: Default::default(),
        };

        let mut merged = empty_merged_result();
        merged
            .env
            .insert("MICA_TEST".to_string(), "\"${pkgs.path}/meme\"".to_string());

        let output = generate_project_nix(&state, &merged, "env-test", timestamp());

        assert!(output.contains("MICA_TEST = \"${pkgs.path}/meme\";"));
    }

    #[test]
    fn project_generation_renders_prefixed_nix_expression_values_raw() {
        let state = ProjectState {
            mica: MicaMetadata {
                version: "0.1.0".to_string(),
                created: timestamp(),
                modified: timestamp(),
            },
            pin: base_pin(),
            pins: BTreeMap::new(),
            presets: PresetState::default(),
            packages: PackagesState::default(),
            env: BTreeMap::new(),
            shell: ShellState::default(),
            nix: Default::default(),
        };

        let mut merged = empty_merged_result();
        merged.env.insert(
            "MICA_TEST".to_string(),
            format!("{}pkgs.path + \"/meme\"", NIX_EXPR_PREFIX),
        );

        let output = generate_project_nix(&state, &merged, "env-test", timestamp());

        assert!(output.contains("MICA_TEST = pkgs.path + \"/meme\";"));
    }

    #[test]
    fn project_generation_wraps_prefixed_interpolation_fragment_as_nix_string() {
        let state = ProjectState {
            mica: MicaMetadata {
                version: "0.1.0".to_string(),
                created: timestamp(),
                modified: timestamp(),
            },
            pin: base_pin(),
            pins: BTreeMap::new(),
            presets: PresetState::default(),
            packages: PackagesState::default(),
            env: BTreeMap::new(),
            shell: ShellState::default(),
            nix: Default::default(),
        };

        let mut merged = empty_merged_result();
        merged.env.insert(
            "MICA_TEST".to_string(),
            format!("{}${{pkgs.path}}/meme", NIX_EXPR_PREFIX),
        );

        let output = generate_project_nix(&state, &merged, "env-test", timestamp());

        assert!(output.contains("MICA_TEST = \"${pkgs.path}/meme\";"));
    }
}
