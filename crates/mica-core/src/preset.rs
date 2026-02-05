use crate::state::{NixBlocks, ProjectState, ShellState};
use indexmap::IndexSet;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum PresetError {
    #[error("failed to read preset file: {0}")]
    Read(std::io::Error),
    #[error("failed to parse preset toml: {0}")]
    Parse(toml::de::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PresetFile {
    pub preset: PresetMetadata,
    #[serde(default)]
    pub packages: PresetPackages,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub shell: ShellState,
    #[serde(default)]
    pub nix: NixBlocks,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PresetMetadata {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub order: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PresetPackages {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub optional: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub name: String,
    pub description: String,
    pub order: i32,
    pub packages_required: Vec<String>,
    pub packages_optional: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub shell: ShellState,
    pub nix: NixBlocks,
    pub source: PathBuf,
}

#[derive(Debug, Clone)]
pub struct EmbeddedPreset {
    pub name: &'static str,
    pub content: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/embedded_presets.rs"));

impl Preset {
    pub fn from_file(file: PresetFile, source: PathBuf) -> Preset {
        Preset {
            name: file.preset.name,
            description: file.preset.description,
            order: file.preset.order,
            packages_required: file.packages.required,
            packages_optional: file.packages.optional,
            env: file.env,
            shell: file.shell,
            nix: file.nix,
            source,
        }
    }
}

pub fn load_embedded_presets() -> Result<Vec<Preset>, PresetError> {
    let mut presets = Vec::new();
    for embedded in EMBEDDED_PRESETS {
        let preset_file: PresetFile =
            toml::from_str(embedded.content).map_err(PresetError::Parse)?;
        let source = PathBuf::from(format!("<embedded:{}>", embedded.name));
        presets.push(Preset::from_file(preset_file, source));
    }
    Ok(presets)
}

pub fn load_presets_from_dir(path: &Path) -> Result<Vec<Preset>, PresetError> {
    let mut presets = Vec::new();
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(presets),
        Err(err) => return Err(PresetError::Read(err)),
    };

    for entry in entries {
        let entry = entry.map_err(PresetError::Read)?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let content = std::fs::read_to_string(&path).map_err(PresetError::Read)?;
        let preset_file: PresetFile = toml::from_str(&content).map_err(PresetError::Parse)?;
        presets.push(Preset::from_file(preset_file, path));
    }

    Ok(presets)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetPackageGroup {
    pub preset: String,
    pub packages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedResult {
    pub preset_packages: Vec<PresetPackageGroup>,
    pub user_packages: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub shell_hooks: Vec<String>,
    pub all_packages: Vec<String>,
    pub let_blocks: Vec<String>,
    pub pin_blocks: Vec<String>,
    pub packages_raw_blocks: Vec<String>,
    pub scripts_blocks: Vec<String>,
    pub env_raw_blocks: Vec<String>,
    pub override_blocks: Vec<String>,
    pub override_merge_blocks: Vec<String>,
    pub override_shellhook_blocks: Vec<String>,
}

fn push_block(target: &mut Vec<String>, block: &Option<String>) {
    if let Some(value) = block {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            target.push(trimmed.to_string());
        }
    }
}

pub fn merge_presets(presets: &[Preset], state: &ProjectState) -> MergedResult {
    let mut ordered = presets.to_vec();
    ordered.sort_by_key(|preset| preset.order);

    let removed: HashSet<&String> = state.packages.removed.iter().collect();
    let mut seen = IndexSet::new();
    let mut preset_packages = Vec::new();

    for preset in &ordered {
        let mut group = PresetPackageGroup {
            preset: preset.name.clone(),
            packages: Vec::new(),
        };

        for pkg in &preset.packages_required {
            if removed.contains(pkg) {
                continue;
            }
            if seen.insert(pkg.clone()) {
                group.packages.push(pkg.clone());
            }
        }

        if !group.packages.is_empty() {
            preset_packages.push(group);
        }
    }

    let mut user_packages = Vec::new();
    for pkg in &state.packages.added {
        if removed.contains(pkg) {
            continue;
        }
        if seen.insert(pkg.clone()) {
            user_packages.push(pkg.clone());
        }
    }

    let mut env = BTreeMap::new();
    for preset in &ordered {
        for (key, value) in &preset.env {
            env.insert(key.clone(), value.clone());
        }
    }
    for (key, value) in &state.env {
        env.insert(key.clone(), value.clone());
    }

    let mut shell_hooks = Vec::new();
    for preset in &ordered {
        if let Some(hook) = &preset.shell.hook {
            shell_hooks.push(hook.clone());
        }
    }
    if let Some(hook) = &state.shell.hook {
        shell_hooks.push(hook.clone());
    }

    let mut let_blocks = Vec::new();
    let mut pin_blocks = Vec::new();
    let mut packages_raw_blocks = Vec::new();
    let mut scripts_blocks = Vec::new();
    let mut env_raw_blocks = Vec::new();
    let mut override_blocks = Vec::new();
    let mut override_merge_blocks = Vec::new();
    let mut override_shellhook_blocks = Vec::new();

    for preset in &ordered {
        push_block(&mut let_blocks, &preset.nix.let_block);
        push_block(&mut pin_blocks, &preset.nix.pins);
        push_block(&mut packages_raw_blocks, &preset.nix.packages_raw);
        push_block(&mut scripts_blocks, &preset.nix.scripts);
        push_block(&mut env_raw_blocks, &preset.nix.env_raw);
        push_block(&mut override_blocks, &preset.nix.override_attrs);
        push_block(&mut override_merge_blocks, &preset.nix.override_merge);
        push_block(
            &mut override_shellhook_blocks,
            &preset.nix.override_shell_hook,
        );
    }
    push_block(&mut let_blocks, &state.nix.let_block);
    push_block(&mut pin_blocks, &state.nix.pins);
    push_block(&mut packages_raw_blocks, &state.nix.packages_raw);
    push_block(&mut scripts_blocks, &state.nix.scripts);
    push_block(&mut env_raw_blocks, &state.nix.env_raw);
    push_block(&mut override_blocks, &state.nix.override_attrs);
    push_block(&mut override_merge_blocks, &state.nix.override_merge);
    push_block(
        &mut override_shellhook_blocks,
        &state.nix.override_shell_hook,
    );

    let all_packages = seen.into_iter().collect();

    MergedResult {
        preset_packages,
        user_packages,
        env,
        shell_hooks,
        all_packages,
        let_blocks,
        pin_blocks,
        packages_raw_blocks,
        scripts_blocks,
        env_raw_blocks,
        override_blocks,
        override_merge_blocks,
        override_shellhook_blocks,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedProfileResult {
    pub preset_packages: Vec<PresetPackageGroup>,
    pub user_packages: Vec<String>,
    pub all_packages: Vec<String>,
}

pub fn merge_profile_presets(
    presets: &[Preset],
    state: &crate::state::GlobalProfileState,
) -> MergedProfileResult {
    let mut ordered = presets.to_vec();
    ordered.sort_by_key(|preset| preset.order);

    let removed: HashSet<&String> = state.packages.removed.iter().collect();
    let mut seen = IndexSet::new();
    let mut preset_packages = Vec::new();

    for preset in &ordered {
        let mut group = PresetPackageGroup {
            preset: preset.name.clone(),
            packages: Vec::new(),
        };

        for pkg in &preset.packages_required {
            if removed.contains(pkg) {
                continue;
            }
            if seen.insert(pkg.clone()) {
                group.packages.push(pkg.clone());
            }
        }

        if !group.packages.is_empty() {
            preset_packages.push(group);
        }
    }

    let mut user_packages = Vec::new();
    for pkg in &state.packages.added {
        if removed.contains(pkg) {
            continue;
        }
        if seen.insert(pkg.clone()) {
            user_packages.push(pkg.clone());
        }
    }

    let all_packages = seen.into_iter().collect();

    MergedProfileResult {
        preset_packages,
        user_packages,
        all_packages,
    }
}

#[cfg(test)]
mod tests {
    use crate::preset::{merge_presets, Preset};
    use crate::state::{MicaMetadata, NixBlocks, Pin, PresetState, ProjectState, ShellState};
    use chrono::{DateTime, NaiveDate, Utc};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn timestamp() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2025-02-04T12:00:00Z")
            .expect("timestamp parse failed")
            .with_timezone(&Utc)
    }

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2025, 2, 4).expect("date parse failed")
    }

    fn base_state() -> ProjectState {
        ProjectState {
            mica: MicaMetadata {
                version: "0.1.0".to_string(),
                created: timestamp(),
                modified: timestamp(),
            },
            pin: Pin {
                name: None,
                url: "https://github.com/jkachmar/nixpkgs".to_string(),
                rev: "a1b2c3".to_string(),
                sha256: "sha256-AAAA".to_string(),
                branch: "main".to_string(),
                updated: date(),
            },
            pins: BTreeMap::new(),
            presets: PresetState { active: vec![] },
            packages: Default::default(),
            env: BTreeMap::new(),
            shell: ShellState::default(),
            nix: NixBlocks::default(),
        }
    }

    #[test]
    fn merge_presets_respects_order_and_removals() {
        let preset_a = Preset {
            name: "a".to_string(),
            description: String::new(),
            order: 10,
            packages_required: vec!["foo".to_string(), "bar".to_string()],
            packages_optional: Vec::new(),
            env: BTreeMap::from([("A".to_string(), "1".to_string())]),
            shell: ShellState {
                hook: Some("echo a".to_string()),
            },
            nix: NixBlocks::default(),
            source: PathBuf::from("a.toml"),
        };
        let preset_b = Preset {
            name: "b".to_string(),
            description: String::new(),
            order: 5,
            packages_required: vec!["bar".to_string(), "baz".to_string()],
            packages_optional: Vec::new(),
            env: BTreeMap::from([("A".to_string(), "2".to_string())]),
            shell: ShellState {
                hook: Some("echo b".to_string()),
            },
            nix: NixBlocks::default(),
            source: PathBuf::from("b.toml"),
        };

        let mut state = base_state();
        state.packages.added = vec!["extra".to_string()];
        state.packages.removed = vec!["bar".to_string()];

        let merged = merge_presets(&[preset_a, preset_b], &state);

        assert_eq!(
            merged.all_packages,
            vec!["baz".to_string(), "foo".to_string(), "extra".to_string()]
        );
        assert_eq!(merged.env.get("A"), Some(&"1".to_string()));
        assert_eq!(merged.shell_hooks.len(), 2);
        assert_eq!(merged.preset_packages.len(), 2);
    }
}
