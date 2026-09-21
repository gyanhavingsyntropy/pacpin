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

pub const SAFE_AUR_HELPERS: &[&str] = &["paru", "yay", "pikaur", "aurman", "pakku", "trizen"];

pub fn validate_helper(helper: &str) -> Result<String, String> {
    let trimmed = helper.trim();
    if trimmed.is_empty() {
        return Err("AUR helper name cannot be empty.".to_string());
    }
    let forbidden = [';', '&', '|', '`', '$', ' ', '\t', '\n', '\r', '(', ')', '{', '}', '<', '>', '~'];
    if trimmed.chars().any(|c| forbidden.contains(&c)) {
        return Err(format!("AUR helper '{}' contains illegal shell characters.", trimmed));
    }
    if SAFE_AUR_HELPERS.contains(&trimmed) {
        return Ok(trimmed.to_string());
    }
    let p = Path::new(trimmed);
    if p.is_absolute() && p.is_file() {
        return Ok(trimmed.to_string());
    }
    Err(format!(
        "Untrusted AUR helper '{}'. Expected one of {:?} or an absolute path to an executable file.",
        trimmed, SAFE_AUR_HELPERS
    ))
}

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
    #[serde(default = "default_false")]
    pub vendor_stickiness: bool,
    #[serde(default = "default_true")]
    pub shell_aliases: bool,
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
            vendor_stickiness: false,
            shell_aliases: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IntegrationsConfig {
    #[serde(default = "default_false")]
    pub flatpak: bool,
    #[serde(default = "default_false")]
    pub nix: bool,
    #[serde(default = "default_false")]
    pub pipx: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub features: Features,
    #[serde(default)]
    pub options: Options,
    #[serde(default)]
    pub repo_order: Vec<String>,
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
    let home = std::env::var("HOME").ok().filter(|h| !h.trim().is_empty());
    if let Some(h) = home {
        Path::new(&h).join(".config").join("pacpin").join("config.toml")
    } else {
        eprintln!(
            "{}",
            "Error: $HOME environment variable is not set. Cannot securely locate configuration directory."
                .red()
                .bold()
        );
        std::process::exit(1);
    }
}

pub fn config_exists() -> bool {
    get_config_path().exists()
}

pub fn load_config() -> Config {
    let path = get_config_path();
    if !path.exists() {
        return Config::default();
    }
    let mut config: Config = match fs::read_to_string(&path) {
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
    };

    if let Err(e) = validate_helper(&config.options.helper) {
        eprintln!("{} Warning: {} Falling back to 'paru'.", "::".yellow(), e);
        config.options.helper = default_helper();
    }

    config
}

pub fn save_config(config: &Config) -> Result<(), std::io::Error> {
    let path = get_config_path();
    save_config_to_path(config, &path)
}

pub fn save_config_to_path(config: &Config, path: &Path) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let toml_str = toml::to_string_pretty(config)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    let temp_path = parent.join(format!(
        ".config.toml.tmp.{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::write(&temp_path, toml_str)?;
    fs::rename(&temp_path, path)?;
    Ok(())
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
        assert!(config.features.shell_aliases);

        let toml_opt_out = r#"
        [features]
        shell_aliases = false
        "#;
        let cfg_opt_out: Config = toml::from_str(toml_opt_out).unwrap();
        assert!(!cfg_opt_out.features.shell_aliases);
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

    #[test]
    fn test_repo_order_config() {
        let toml_str = r#"
        repo_order = ["core", "cachyos", "extra"]
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.repo_order, vec!["core", "cachyos", "extra"]);
    }

    #[test]
    fn test_vendor_stickiness_config() {
        let toml_str = r#"
        [features]
        vendor_stickiness = true
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.features.vendor_stickiness);

        let default_config = Config::default();
        assert!(!default_config.features.vendor_stickiness);
    }

    #[test]
    fn test_validate_helper() {
        assert_eq!(validate_helper("paru").unwrap(), "paru");
        assert_eq!(validate_helper("yay").unwrap(), "yay");
        assert_eq!(validate_helper("pikaur").unwrap(), "pikaur");

        assert!(validate_helper("").is_err());
        assert!(validate_helper("rm -rf /").is_err());
        assert!(validate_helper("paru; whoami").is_err());
        assert!(validate_helper("untrusted_helper_xyz").is_err());

        let sh_path = "/bin/sh";
        if std::path::Path::new(sh_path).exists() {
            assert_eq!(validate_helper(sh_path).unwrap(), sh_path);
        }
    }

    #[test]
    fn test_atomic_save_config() {
        let temp_dir = std::env::temp_dir().join(format!("pacpin_test_cfg_{}", std::process::id()));
        let cfg_path = temp_dir.join("config.toml");
        let mut config = Config::default();
        config.pins.insert("test-pkg".to_string(), "extra".to_string());

        assert!(save_config_to_path(&config, &cfg_path).is_ok());
        assert!(cfg_path.exists());

        let content = fs::read_to_string(&cfg_path).unwrap();
        assert!(content.contains("test-pkg"));
        assert!(content.contains("extra"));

        // Ensure no leftover temp files exist
        let leftover = fs::read_dir(&temp_dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with(".config.toml.tmp"));
        assert!(!leftover);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
