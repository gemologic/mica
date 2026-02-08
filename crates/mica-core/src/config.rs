use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file: {0}")]
    Read(std::io::Error),
    #[error("failed to write config file: {0}")]
    Write(std::io::Error),
    #[error("failed to parse toml: {0}")]
    Parse(toml::de::Error),
    #[error("failed to serialize toml: {0}")]
    Serialize(toml::ser::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Config {
    #[serde(default)]
    pub mica: MicaSection,
    #[serde(default)]
    pub nixpkgs: NixpkgsSection,
    #[serde(default)]
    pub index: IndexSection,
    #[serde(default)]
    pub presets: PresetSection,
    #[serde(default)]
    pub tui: TuiSection,
}

impl Config {
    pub fn load_from_path(path: &Path) -> Result<Config, ConfigError> {
        let content = std::fs::read_to_string(path).map_err(ConfigError::Read)?;
        let config = toml::from_str(&content).map_err(ConfigError::Parse)?;
        Ok(config)
    }

    pub fn save_to_path(&self, path: &Path) -> Result<(), ConfigError> {
        let content = toml::to_string_pretty(self).map_err(ConfigError::Serialize)?;
        std::fs::write(path, content).map_err(ConfigError::Write)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MicaSection {
    pub version: String,
}

impl Default for MicaSection {
    fn default() -> Self {
        MicaSection {
            version: "0.1.0".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NixpkgsSection {
    pub default_url: String,
    pub default_branch: String,
}

impl Default for NixpkgsSection {
    fn default() -> Self {
        NixpkgsSection {
            default_url: "https://github.com/jpetrucciani/nix".to_string(),
            default_branch: "main".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexSection {
    pub remote_url: String,
    pub update_check_interval: u64,
}

impl Default for IndexSection {
    fn default() -> Self {
        IndexSection {
            remote_url: "https://static.g7c.us/mica".to_string(),
            update_check_interval: 24,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PresetSection {
    #[serde(default)]
    pub extra_dirs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TuiSection {
    pub show_details: bool,
    pub search_mode: SearchMode,
    #[serde(default)]
    pub columns: TuiColumns,
}

impl Default for TuiSection {
    fn default() -> Self {
        TuiSection {
            show_details: true,
            search_mode: SearchMode::All,
            columns: TuiColumns::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TuiColumns {
    pub version: bool,
    pub description: bool,
    pub license: bool,
    pub platforms: bool,
    pub main_program: bool,
}

impl Default for TuiColumns {
    fn default() -> Self {
        TuiColumns {
            version: true,
            description: true,
            license: false,
            platforms: false,
            main_program: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Name,
    Description,
    Binary,
    #[default]
    All,
}

#[cfg(test)]
mod tests {
    use crate::config::{Config, SearchMode};

    #[test]
    fn config_round_trip() {
        let mut config = Config::default();
        config.tui.search_mode = SearchMode::Binary;
        config.presets.extra_dirs = vec!["~/my-presets".to_string()];

        let toml = toml::to_string(&config).expect("serialize failed");
        let decoded: Config = toml::from_str(&toml).expect("deserialize failed");
        assert_eq!(config, decoded);
    }

    #[test]
    fn default_config_has_remote_index_url() {
        let config = Config::default();
        assert_eq!(config.index.remote_url, "https://static.g7c.us/mica");
    }
}
