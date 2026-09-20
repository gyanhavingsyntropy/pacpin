// pacpin - Declarative Package Resolver & Upgrade Engine for Arch Linux / CachyOS
// Copyright (C) 2026 Gyan <330976822+gyanhavingsyntropy@users.noreply.github.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use colored::Colorize;
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Options {
    #[serde(default = "default_helper")]
    pub helper: String,
}

fn default_helper() -> String {
    "paru".to_string()
}

impl Default for Options {
    fn default() -> Self {
        Self {
            helper: default_helper(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Features {
    #[serde(default = "default_true")]
    pub pinning: bool,
    #[serde(default = "default_false")]
    pub stability_delays: bool,
    #[serde(default = "default_true")]
    pub smart_orphans: bool,
    #[serde(default = "default_false")]
    pub integrations: bool,
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

impl Default for Features {
    fn default() -> Self {
        Self {
            pinning: true,
            stability_delays: false,
            smart_orphans: true,
            integrations: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IntegrationsConfig {
    #[serde(default = "default_false")]
    pub flatpak: bool,
    #[serde(default = "default_false")]
    pub nix: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub features: Features,
    #[serde(default)]
    pub options: Options,
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_exclude")]
    pub exclude: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub delay: BTreeMap<String, u32>,
    #[serde(default)]
    pub integrations: IntegrationsConfig,
}

fn deserialize_exclude<'de, D>(deserializer: D) -> Result<BTreeMap<String, Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }

    let map: BTreeMap<String, OneOrMany> = BTreeMap::deserialize(deserializer)?;
    let mut result = BTreeMap::new();
    for (k, v) in map {
        match v {
            OneOrMany::One(s) => {
                result.insert(k, vec![s]);
            }
            OneOrMany::Many(vec) => {
                result.insert(k, vec);
            }
        }
    }
    Ok(result)
}

pub fn get_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    Path::new(&home).join(".config").join("pacpin").join("config.toml")
}

pub fn config_exists() -> bool {
    get_config_path().exists()
}

pub fn load_config() -> Config {
    let path = get_config_path();
    if !path.exists() {
        return Config::default();
    }
    match fs::read_to_string(&path) {
        Ok(contents) => match toml::from_str(&contents) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!(
                    "{}",
                    format!("Error: Failed to parse configuration file ({}):", path.display()).red().bold()
                );
                eprintln!("  {}", e);
                eprintln!("Please fix the syntax error in your config.toml before proceeding.");
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!(
                "{}",
                format!("Error: Failed to read configuration file ({}): {}", path.display(), e).red().bold()
            );
            std::process::exit(1);
        }
    }
}

pub fn save_config(config: &Config) -> Result<(), std::io::Error> {
    let path = get_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let toml_str = toml::to_string_pretty(config)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    fs::write(path, toml_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_features_default() {
        let toml_str = r#"
        [options]
        helper = "paru"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.features.pinning);
        assert!(!config.features.stability_delays);
        assert!(config.features.smart_orphans);
    }

    #[test]
    fn test_features_explicit() {
        let toml_str = r#"
        [features]
        pinning = false
        stability_delays = true
        smart_orphans = false
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.features.pinning);
        assert!(config.features.stability_delays);
        assert!(!config.features.smart_orphans);
        assert!(!config.features.integrations);
    }

    #[test]
    fn test_integrations_config() {
        let toml_str = r#"
        [features]
        integrations = true

        [integrations]
        flatpak = true
        nix = true
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.features.integrations);
        assert!(config.integrations.flatpak);
        assert!(config.integrations.nix);
    }
}
