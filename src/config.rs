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
    let forbidden = [
        ';', '&', '|', '`', '$', ' ', '\t', '\n', '\r', '(', ')', '{', '}', '<', '>', '~',
    ];
    if trimmed.chars().any(|c| forbidden.contains(&c)) {
        return Err(format!(
            "AUR helper '{}' contains illegal shell characters.",
            trimmed
        ));
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Options {
    #[serde(default = "default_helper")]
    pub helper: String,
    #[serde(default = "default_privilege_tool")]
    pub privilege_tool: String,
    #[serde(default = "default_false")]
    pub no_cache: bool,
}

fn default_helper() -> String {
    "paru".to_string()
}

fn default_privilege_tool() -> String {
    "sudo".to_string()
}

pub fn validate_privilege_tool(tool: &str) -> Result<&str, String> {
    match tool {
        "sudo" | "doas" | "pkexec" => Ok(tool),
        _ => Err(format!(
            "Unsupported privilege tool '{tool}'; expected sudo, doas, or pkexec"
        )),
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            helper: default_helper(),
            privilege_tool: default_privilege_tool(),
            no_cache: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct IntegrationsConfig {
    #[serde(default = "default_false")]
    pub flatpak: bool,
    #[serde(default = "default_false")]
    pub nix: bool,
    #[serde(default = "default_false")]
    pub pipx: bool,
}

/// Packages the user wants present on every machine using this config.
/// Dependency packages are intentionally omitted: the native managers resolve them.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub pacman: Vec<String>,
    #[serde(default)]
    pub aur: Vec<String>,
    #[serde(default)]
    pub flatpak_user: Vec<String>,
    #[serde(default)]
    pub flatpak_system: Vec<String>,
}

impl PackageManifest {
    pub fn is_empty(&self) -> bool {
        self.pacman.is_empty()
            && self.sources.is_empty()
            && self.aur.is_empty()
            && self.flatpak_user.is_empty()
            && self.flatpak_system.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Config {
    #[serde(skip)]
    pub manager_file: crate::manager_config::ManagerFile,
    #[serde(skip)]
    pub manager_base_options: Option<Options>,
    #[serde(skip)]
    pub external_sources: BTreeMap<String, crate::sources::Source>,
    #[serde(skip)]
    pub external_hooks: BTreeMap<String, crate::hooks::Hook>,
    #[serde(default)]
    pub hooks: BTreeMap<String, crate::hooks::Hook>,
    #[serde(default)]
    pub sources: BTreeMap<String, crate::sources::Source>,
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
    #[serde(default)]
    pub packages: PackageManifest,
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
    if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg_config.trim().is_empty() {
            return Path::new(&xdg_config).join("pacpin").join("config.toml");
        }
    }
    let home = std::env::var("HOME").ok().filter(|h| !h.trim().is_empty());
    if let Some(h) = home {
        Path::new(&h)
            .join(".config")
            .join("pacpin")
            .join("config.toml")
    } else {
        eprintln!(
            "{}",
            "Error: Neither $XDG_CONFIG_HOME nor $HOME is set. Cannot securely locate configuration directory."
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
    let mut config: Config = if !path.exists() { Config::default() } else { match fs::read_to_string(&path) {
        Ok(contents) => match toml::from_str(&contents) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!(
                    "{}",
                    format!(
                        "Error: Failed to parse configuration file ({}):",
                        path.display()
                    )
                    .red()
                    .bold()
                );
                eprintln!("  {}", e);
                eprintln!("Please fix the syntax error in your config.toml before proceeding.");
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!(
                "{}",
                format!(
                    "Error: Failed to read configuration file ({}): {}",
                    path.display(),
                    e
                )
                .red()
                .bold()
            );
            std::process::exit(1);
        }
    }};

    if let Err(error) = load_supplements(&mut config, &path) {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }

    if let Err(e) = crate::hooks::validate(&config) {
        eprintln!("Error: Invalid post-update hook configuration: {e}");
        std::process::exit(1);
    }
    if let Err(e) = crate::sources::validate_config(&config) {
        eprintln!("Error: Invalid package source configuration: {e}");
        std::process::exit(1);
    }
    if let Err(e) = validate_helper(&config.options.helper) {
        eprintln!("{} Warning: {} Falling back to 'paru'.", "::".yellow(), e);
        config.options.helper = default_helper();
    }

    if let Err(e) = validate_privilege_tool(&config.options.privilege_tool) {
        eprintln!("{} Warning: {} Falling back to 'sudo'.", "::".yellow(), e);
        config.options.privilege_tool = default_privilege_tool();
    }

    config
}

fn load_supplements(config: &mut Config, path: &Path) -> Result<(), String> {
    #[derive(Default, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SourcesFile {
        #[serde(default)]
        sources: BTreeMap<String, crate::sources::Source>,
    }
    #[derive(Default, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct HooksFile {
        #[serde(default)]
        hooks: BTreeMap<String, crate::hooks::Hook>,
    }
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    config.manager_file = crate::manager_config::read_optional(&directory.join("managers.toml"))?;
    crate::manager_config::validate(&config.manager_file)?;
    let sources: SourcesFile = crate::manager_config::read_optional(&directory.join("sources.toml"))?;
    for (alias, source) in &sources.sources {
        if config.sources.contains_key(alias) { return Err(format!("Source alias '{alias}' is defined in both config.toml and sources.toml; keep one definition")); }
        config.sources.insert(alias.clone(), source.clone());
    }
    config.external_sources = sources.sources;
    let hooks: HooksFile = crate::manager_config::read_optional(&directory.join("hooks.toml"))?;
    for (name, hook) in &hooks.hooks {
        if config.hooks.contains_key(name) { return Err(format!("Hook '{name}' is defined in both config.toml and hooks.toml; keep one definition")); }
        config.hooks.insert(name.clone(), hook.clone());
    }
    config.external_hooks = hooks.hooks;
    config.manager_base_options = Some(config.options.clone());
    if let Some(definition) = config.manager_file.managers.get("pacman") {
        if let Some(tool) = &definition.privilege_tool { config.options.privilege_tool = tool.clone(); }
    }
    if let Some(definition) = config.manager_file.managers.get("aur") {
        if let Some(helper) = &definition.helper { config.options.helper = helper.clone(); }
    }
    Ok(())
}

pub fn supplemental_defaults() -> Result<Config, String> {
    let mut config = Config::default();
    load_supplements(&mut config, &get_config_path())?;
    crate::hooks::validate(&config)?;
    crate::sources::validate_config(&config)?;
    Ok(config)
}

pub fn save_config(config: &Config) -> Result<(), std::io::Error> {
    let path = get_config_path();
    save_config_to_path(config, &path)
}

pub fn mutate_config<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce(&mut Config) -> Result<R, String>,
{
    mutate_config_at(&get_config_path(), f)
}

fn mutate_config_at<F, R>(path: &Path, f: F) -> Result<R, String>
where
    F: FnOnce(&mut Config) -> Result<R, String>,
{
    let _lock = lock_config(path).map_err(|e| format!("Failed to lock configuration: {}", e))?;
    let mut cfg = match fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents)
            .map_err(|e| format!("Failed to parse configuration: {}", e))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => return Err(format!("Failed to read configuration: {}", e)),
    };
    load_supplements(&mut cfg, path)?;
    let res = f(&mut cfg)?;
    write_config_unlocked(&cfg, path)
        .map_err(|e| format!("Failed to save configuration: {}", e))?;
    Ok(res)
}

/// Interactive setup must not overwrite edits made while its prompts were open.
pub fn save_config_if_unchanged(previous: &Config, updated: &Config) -> Result<(), String> {
    save_config_if_unchanged_at(&get_config_path(), previous, updated)
}

fn save_config_if_unchanged_at(path: &Path, previous: &Config, updated: &Config) -> Result<(), String> {
    mutate_config_at(path, |current| {
        if current != previous {
            return Err("Configuration changed during setup. Run setup again to include those changes.".into());
        }
        *current = updated.clone();
        Ok(())
    })
}

// The returned file owns the flock; closing it releases the lock on every path.
fn lock_config(path: &Path) -> std::io::Result<fs::File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("lock");
    let mut options = fs::OpenOptions::new();
    options.create(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let lock_file = options.open(lock_path)?;
    if !lock_file.metadata()?.is_file() {
        return Err(std::io::Error::other("Configuration lock is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        loop {
            if unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    Ok(lock_file)
}

pub fn save_config_to_path(config: &Config, path: &Path) -> Result<(), std::io::Error> {
    let _lock = lock_config(path)?;
    write_config_unlocked(config, path)
}

fn write_config_unlocked(config: &Config, path: &Path) -> Result<(), std::io::Error> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));

    let mut persistent = config.clone();
    if let Some(base) = &config.manager_base_options {
        if config.manager_file.managers.get("pacman").is_some_and(|d| d.privilege_tool.is_some()) { persistent.options.privilege_tool = base.privilege_tool.clone(); }
        if config.manager_file.managers.get("aur").is_some_and(|d| d.helper.is_some()) { persistent.options.helper = base.helper.clone(); }
    }
    for (alias, original) in &config.external_sources {
        if persistent.sources.remove(alias).as_ref() != Some(original) {
            return Err(std::io::Error::other(format!("Source '{alias}' is owned by sources.toml; edit that file directly")));
        }
    }
    for (name, original) in &config.external_hooks {
        if persistent.hooks.remove(name).as_ref() != Some(original) {
            return Err(std::io::Error::other(format!("Hook '{name}' is owned by hooks.toml; edit that file directly")));
        }
    }
    let toml_str = toml::to_string_pretty(&persistent)
        .map_err(std::io::Error::other)?;

    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let mut temp_path = PathBuf::new();
    let mut temp_file = None;

    // Retry with random suffix to avoid collisions and prevent symlink following
    for _ in 0..100 {
        let mut random_bytes = [0u8; 8];
        if let Ok(mut f) = fs::File::open("/dev/urandom") {
            use std::io::Read;
            let _ = f.read_exact(&mut random_bytes);
        } else {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            random_bytes = (nanos as u64 ^ (std::process::id() as u64)).to_le_bytes();
        }
        let hex = random_bytes
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>();
        let candidate = parent.join(format!(".config.toml.tmp.{}_{}", std::process::id(), hex));

        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600);

        if let Ok(f) = opts.open(&candidate) {
            temp_path = candidate;
            temp_file = Some(f);
            break;
        }
    }

    let mut file = temp_file.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "Could not create exclusive temporary config file",
        )
    })?;

    let result = file.write_all(toml_str.as_bytes()).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = result.and_then(|()| fs::rename(&temp_path, path)) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    // Durably sync directory entry on Unix
    if let Ok(parent_dir) = fs::File::open(parent) {
        let _ = parent_dir.sync_all();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_config_mutations_preserve_every_update() {
        let dir = std::env::temp_dir().join(format!("pacpin_config_race_{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut initial = Config::default();
        initial.packages.pacman.push("git".into());
        save_config_to_path(&initial, &path).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let workers: Vec<_> = (0..8).map(|i| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..5 {
                    mutate_config_at(&path, |cfg| {
                        cfg.pins.insert(format!("pkg-{i}"), "extra".into());
                        *cfg.delay.entry("counter".into()).or_default() += 1;
                        Ok(())
                    }).unwrap();
                }
            })
        }).collect();
        for worker in workers { worker.join().unwrap(); }
        let result: Config = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(result.pins.len(), 8);
        assert_eq!(result.delay["counter"], 40);
        assert_eq!(result.packages, initial.packages);
        // An interactive editor with an old snapshot must not lose these changes.
        assert!(save_config_if_unchanged_at(&path, &initial, &Config::default()).is_err());
        let unchanged: Config = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(unchanged, result);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn config_mutation_failure_releases_lock_and_preserves_data() {
        let dir = std::env::temp_dir().join(format!("pacpin_config_error_{}", std::process::id()));
        let path = dir.join("config.toml");
        save_config_to_path(&Config::default(), &path).unwrap();
        let before = fs::read(&path).unwrap();
        let result: Result<(), String> = mutate_config_at(&path, |cfg| {
            cfg.pins.insert("git".into(), "extra".into());
            Err("cancelled".into())
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        mutate_config_at(&path, |cfg| { cfg.pins.insert("vim".into(), "extra".into()); Ok(()) }).unwrap();
        fs::write(&path, "malformed = [").unwrap();
        assert!(mutate_config_at(&path, |_| Ok(())).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "malformed = [");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn config_lock_rejects_symlinks() {
        let dir = std::env::temp_dir().join(format!("pacpin_config_link_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("victim");
        fs::write(&target, "keep").unwrap();
        let path = dir.join("config.toml");
        std::os::unix::fs::symlink(&target, path.with_extension("lock")).unwrap();
        assert!(save_config_to_path(&Config::default(), &path).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "keep");
        fs::remove_dir_all(dir).unwrap();
    }

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
        assert_eq!(config.options.privilege_tool, "sudo");
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
    fn test_package_manifest_round_trip_and_legacy_default() {
        let legacy: Config = toml::from_str("[options]\nhelper = 'paru'\n").unwrap();
        assert!(legacy.packages.is_empty());

        let manifest: Config = toml::from_str(
            "[packages]\npacman = ['git', 'core/linux']\naur = ['paru-bin']\nflatpak_user = ['flathub/org.gnome.Calculator']\n",
        )
        .unwrap();
        assert_eq!(manifest.packages.pacman, ["git", "core/linux"]);
        assert_eq!(manifest.packages.aur, ["paru-bin"]);
        assert_eq!(manifest.packages.flatpak_user.len(), 1);
        let encoded = toml::to_string(&manifest).unwrap();
        let decoded: Config = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded.packages, manifest.packages);
        assert!(toml::from_str::<Config>("[packages]\nnix = ['ripgrep']\n").is_err());
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
        config
            .pins
            .insert("test-pkg".to_string(), "extra".to_string());

        assert!(save_config_to_path(&config, &cfg_path).is_ok());
        assert!(cfg_path.exists());

        let content = fs::read_to_string(&cfg_path).unwrap();
        assert!(content.contains("test-pkg"));
        assert!(content.contains("extra"));

        // Ensure no leftover temp files exist
        let leftover = fs::read_dir(&temp_dir).unwrap().flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".config.toml.tmp")
        });
        assert!(!leftover);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn supplements_merge_without_being_copied_into_the_main_config() {
        let directory = std::env::temp_dir().join(format!("pacpin-supplements-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.toml");
        fs::write(&path, "[pins]\napp='local'\n").unwrap();
        let source = "[sources.local]\nkind='local'\nurl='/tmp/catalog.json'\npackages={}\n";
        let hook = "[hooks.example]\nwatch=['mutter']\naction='script'\ncommand=['/usr/bin/true']\n";
        fs::write(directory.join("sources.toml"), source).unwrap();
        fs::write(directory.join("hooks.toml"), hook).unwrap();
        fs::write(directory.join("managers.toml"), "[managers.pacman]\nadapter='pacman'\nprivilege_tool='pkexec'\n[managers.aur]\nadapter='aur'\nhelper='yay'\n").unwrap();
        let mut config: Config = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        load_supplements(&mut config, &path).unwrap();
        assert_eq!(config.sources.len(), 1); assert_eq!(config.hooks.len(), 1);
        assert_eq!(config.options.privilege_tool, "pkexec"); assert_eq!(config.options.helper, "yay");
        mutate_config_at(&path, |config| { config.pins.insert("another".into(), "extra".into()); Ok(()) }).unwrap();
        let persisted: Config = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(persisted.sources.is_empty()); assert!(persisted.hooks.is_empty());
        assert_eq!(persisted.options.privilege_tool, "sudo"); assert_eq!(persisted.options.helper, "paru");
        assert_eq!(fs::read_to_string(directory.join("sources.toml")).unwrap(), source);
        assert_eq!(fs::read_to_string(directory.join("hooks.toml")).unwrap(), hook);
        let mut duplicate = Config { sources: config.sources.clone(), ..Config::default() };
        assert!(load_supplements(&mut duplicate, &path).is_err());
        let mut changed = config; changed.sources.remove("local");
        assert!(save_config_to_path(&changed, &path).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
}
