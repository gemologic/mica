use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const NIX_EXPR_PREFIX: &str = "__mica_nix_expr__:";

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("failed to read state file: {0}")]
    Read(std::io::Error),
    #[error("failed to write state file: {0}")]
    Write(std::io::Error),
    #[error("failed to parse toml: {0}")]
    Parse(toml::de::Error),
    #[error("failed to serialize toml: {0}")]
    Serialize(toml::ser::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MicaMetadata {
    pub version: String,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Pin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub url: String,
    pub rev: String,
    pub sha256: String,
    pub branch: String,
    pub updated: NaiveDate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PresetState {
    #[serde(default)]
    pub active: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PackagesState {
    #[serde(default)]
    pub added: Vec<String>,
    #[serde(default)]
    pub removed: Vec<String>,
    #[serde(default)]
    pub pinned: BTreeMap<String, PinnedPackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinnedPackage {
    pub version: String,
    pub pin: Pin,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ShellState {
    #[serde(default)]
    pub hook: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct NixBlocks {
    #[serde(default, rename = "let")]
    pub let_block: Option<String>,
    #[serde(default)]
    pub pins: Option<String>,
    #[serde(default)]
    pub packages_raw: Option<String>,
    #[serde(default)]
    pub scripts: Option<String>,
    #[serde(default)]
    pub env_raw: Option<String>,
    #[serde(default, rename = "override")]
    pub override_attrs: Option<String>,
    #[serde(default)]
    pub override_merge: Option<String>,
    #[serde(default, rename = "override_shellhook")]
    pub override_shell_hook: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectState {
    pub mica: MicaMetadata,
    pub pin: Pin,
    #[serde(default)]
    pub pins: BTreeMap<String, Pin>,
    #[serde(default)]
    pub presets: PresetState,
    #[serde(default)]
    pub packages: PackagesState,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub shell: ShellState,
    #[serde(default)]
    pub nix: NixBlocks,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GlobalProfileState {
    pub mica: MicaMetadata,
    pub pin: Pin,
    #[serde(default)]
    pub presets: PresetState,
    #[serde(default)]
    pub packages: PackagesState,
    #[serde(default)]
    pub generations: GenerationsState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct GenerationsState {
    #[serde(default)]
    pub history: Vec<GenerationEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenerationEntry {
    pub id: u64,
    pub timestamp: DateTime<Utc>,
    pub packages: Vec<String>,
}

impl ProjectState {
    pub fn load_from_path(path: &Path) -> Result<ProjectState, StateError> {
        let content = std::fs::read_to_string(path).map_err(StateError::Read)?;
        let state = toml::from_str(&content).map_err(StateError::Parse)?;
        Ok(state)
    }

    pub fn save_to_path(&self, path: &Path) -> Result<(), StateError> {
        let content = toml::to_string_pretty(self).map_err(StateError::Serialize)?;
        std::fs::write(path, content).map_err(StateError::Write)?;
        Ok(())
    }
}

impl GlobalProfileState {
    pub fn load_from_path(path: &Path) -> Result<GlobalProfileState, StateError> {
        let content = std::fs::read_to_string(path).map_err(StateError::Read)?;
        let state = toml::from_str(&content).map_err(StateError::Parse)?;
        Ok(state)
    }

    pub fn save_to_path(&self, path: &Path) -> Result<(), StateError> {
        let content = toml::to_string_pretty(self).map_err(StateError::Serialize)?;
        std::fs::write(path, content).map_err(StateError::Write)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::state::{
        GenerationEntry, GenerationsState, GlobalProfileState, MicaMetadata, NixBlocks,
        PackagesState, Pin, PinnedPackage, PresetState, ProjectState, ShellState,
    };
    use chrono::{DateTime, NaiveDate, Utc};
    use std::collections::BTreeMap;

    fn timestamp() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2025-02-04T12:00:00Z")
            .expect("timestamp parse failed")
            .with_timezone(&Utc)
    }

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2025, 2, 4).expect("date parse failed")
    }

    #[test]
    fn project_state_round_trip() {
        let mut pinned = BTreeMap::new();
        pinned.insert(
            "nodejs".to_string(),
            PinnedPackage {
                version: "18.19.0".to_string(),
                pin: Pin {
                    name: None,
                    url: "https://github.com/NixOS/nixpkgs".to_string(),
                    rev: "nixos-23.11".to_string(),
                    sha256: "sha256-TEST".to_string(),
                    branch: "nixos-23.11".to_string(),
                    updated: date(),
                },
            },
        );

        let state = ProjectState {
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
            pins: BTreeMap::from([(
                "rust".to_string(),
                Pin {
                    name: None,
                    url: "https://github.com/oxalica/rust-overlay".to_string(),
                    rev: "deadbeef".to_string(),
                    sha256: "sha256-RUST".to_string(),
                    branch: "master".to_string(),
                    updated: date(),
                },
            )]),
            presets: PresetState {
                active: vec!["rust".to_string()],
            },
            packages: PackagesState {
                added: vec!["jq".to_string()],
                removed: vec!["cargo-edit".to_string()],
                pinned,
            },
            env: BTreeMap::from([("EDITOR".to_string(), "nvim".to_string())]),
            shell: ShellState {
                hook: Some("echo hi".to_string()),
            },
            nix: NixBlocks {
                let_block: Some("uvEnv = pkgs.uv-nix.mkEnv { };\n".to_string()),
                pins: Some(", rust ? import (fetchTarball { }) { }".to_string()),
                packages_raw: Some("uvEnv".to_string()),
                scripts: Some(
                    "build_static = writers.writeBashBin \"build_static\" \"\";".to_string(),
                ),
                env_raw: Some("DOTNET_ROOT = \"${pkgs.dotnet-sdk_9}\";".to_string()),
                override_attrs: Some("shellHook = prev.shellHook or \"\";".to_string()),
                override_merge: Some("// uvEnv.uvEnvVars".to_string()),
                override_shell_hook: Some("${uvEnv.shellHook or \"\"}".to_string()),
            },
        };

        let toml = toml::to_string(&state).expect("serialize failed");
        let decoded: ProjectState = toml::from_str(&toml).expect("deserialize failed");
        assert_eq!(state, decoded);
    }

    #[test]
    fn global_state_round_trip() {
        let state = GlobalProfileState {
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
            presets: PresetState {
                active: vec!["devops".to_string()],
            },
            packages: PackagesState::default(),
            generations: GenerationsState {
                history: vec![GenerationEntry {
                    id: 1,
                    timestamp: timestamp(),
                    packages: vec!["ripgrep".to_string()],
                }],
            },
        };

        let toml = toml::to_string(&state).expect("serialize failed");
        let decoded: GlobalProfileState = toml::from_str(&toml).expect("deserialize failed");
        assert_eq!(state, decoded);
    }
}
