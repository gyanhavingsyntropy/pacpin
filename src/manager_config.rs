//! User-owned manager definitions. Core adapters keep their typed boundaries.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::Path;

pub const DEFAULT_MANAGERS: &str = include_str!("../docs/managers.toml");
pub const DEFAULT_SOURCES: &str = "# Release and source-build recipes; same [sources.alias] schema as config.toml.\n# Existing recipes in config.toml remain supported. Duplicate aliases are errors.\n# format='source' uses a per-package [sources.ALIAS.packages.PACKAGE.build] table.\n# build.optimization defaults to inherited makepkg settings; native CPU, level\n# and LTO overrides are opt-in. See docs/sources.md for complete recipes.\n[sources]\n";
pub const DEFAULT_HOOKS: &str = "# Post-update actions; same [hooks.name] schema as config.toml.\n# Runs only after verified installed version changes in a successful upgrade.\n# [hooks.example]\n# watch = [\"mutter\", \"gnome-shell\"]\n# action = \"script\"\n# command = [\"/absolute/path/rebuild-blur\"]\n[hooks]\n";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagerFile {
    #[serde(default = "schema_version")]
    pub version: u32,
    #[serde(default)]
    pub managers: BTreeMap<String, Definition>,
}
fn schema_version() -> u32 { 1 }
impl Default for ManagerFile { fn default() -> Self { Self { version: 1, managers: BTreeMap::new() } } }

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Definition {
    pub adapter: String,
    /// None preserves the legacy [features]/[integrations] selection.
    pub enabled: Option<bool>,
    pub check: Vec<String>,
    pub verify: Vec<String>,
    pub upgrade: Vec<String>,
    pub upgrade_ram: Vec<String>,
    pub upgrade_storage: Vec<String>,
    /// Appended literal arguments; composes with the selected scratch variant.
    pub no_cache_args: Vec<String>,
    /// Removes caches only; separate from explicit unused-package pruning.
    pub cache_cleanup: Vec<String>,
    pub clean: Vec<String>,
    pub refresh: Vec<String>,
    /// Optional literal download-only command, run before this manager's stage.
    /// Automatic scratch policy only; never silently relax forced RAM/storage.
    pub prepare: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub scratch_mib: Option<u64>,
    pub helper: Option<String>,
    pub privilege_tool: Option<String>,
    pub flags: BTreeMap<String, UserFlag>,
    #[serde(skip)]
    pub selected_flags: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct UserFlag {
    pub args: Vec<String>,
    /// Explicit additional action, run after this manager's successful upgrade.
    pub command: Vec<String>,
}

pub fn read_optional<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
        Err(error) => return Err(format!("Cannot read {}: {error}", path.display())),
    };
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(format!("{} must be a regular file", path.display()));
    }
    let mut body = String::new();
    file.take(1_048_577).read_to_string(&mut body).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    if body.len() > 1_048_576 { return Err(format!("{} exceeds the 1 MiB configuration limit", path.display())); }
    toml::from_str(&body).map_err(|e| format!("Invalid {}: {e}", path.display()))
}

pub fn validate(file: &ManagerFile) -> Result<(), String> {
    if file.version != 1 { return Err("Unsupported managers.toml version; expected 1".into()); }
    if file.managers.len() > 64 { return Err("At most 64 managers can be configured".into()); }
    for (name, definition) in &file.managers {
        // Configuration keys choose typed adapters; display aliases must not
        // introduce a second external provider with a built-in identity.
        if matches!(name.as_str(), "pacman" | "aur" | "flatpak" | "nix" | "pipx")
            && definition.adapter != *name
        {
            return Err(format!("{name}: this reserved manager key must use adapter '{name}'"));
        }
        if definition.adapter == "command" && matches!(name.as_str(), "Flatpak" | "Nix" | "Pipx") {
            return Err(format!("{name}: this external manager display name is reserved"));
        }
        if name.len() > 64 || !crate::utils::is_safe_pkg_name(name) || name == "Sources" {
            return Err(format!("Invalid or reserved manager name '{name}'"));
        }
        let core = matches!(definition.adapter.as_str(), "pacman" | "aur");
        if core {
            if name != &definition.adapter || definition.enabled == Some(false) {
                return Err(format!("Core adapter '{}' must keep its canonical name and cannot be disabled", definition.adapter));
            }
            if !definition.check.is_empty() || !definition.verify.is_empty() || !definition.upgrade.is_empty() || !definition.upgrade_ram.is_empty()
                || !definition.upgrade_storage.is_empty() || !definition.no_cache_args.is_empty()
                || !definition.cache_cleanup.is_empty() || !definition.clean.is_empty() || !definition.refresh.is_empty() || !definition.prepare.is_empty()
                || !definition.env.is_empty() || definition.scratch_mib.is_some() {
                return Err(format!("{name}: core commands are owned by the typed adapter; configure helper/privilege_tool instead"));
            }
        } else if !matches!(definition.adapter.as_str(), "flatpak" | "nix" | "pipx" | "command") {
            return Err(format!("{name}: unknown adapter '{}'", definition.adapter));
        }
        if matches!(definition.adapter.as_str(), "flatpak" | "nix" | "pipx") && name != &definition.adapter {
            return Err(format!("Built-in adapter '{}' must use its canonical manager name", definition.adapter));
        }
        if definition.helper.is_some() && definition.adapter != "aur"
            || definition.privilege_tool.is_some() && definition.adapter != "pacman" {
            return Err(format!("{name}: helper belongs to AUR and privilege_tool belongs to pacman"));
        }
        if let Some(helper) = &definition.helper { crate::config::validate_helper(helper)?; }
        if let Some(tool) = &definition.privilege_tool { crate::config::validate_privilege_tool(tool)?; }
        if definition.adapter == "command" && (definition.check.is_empty() || definition.upgrade.is_empty() || definition.verify.is_empty()) {
            return Err(format!("{name}: command adapters require check, upgrade, and verify argument arrays"));
        }
        for (operation, command, placeholders) in [
            ("check", &definition.check, false), ("refresh", &definition.refresh, false),
            ("verify", &definition.verify, true),
            ("prepare", &definition.prepare, true),
            ("upgrade", &definition.upgrade, true), ("upgrade_ram", &definition.upgrade_ram, true),
            ("upgrade_storage", &definition.upgrade_storage, true), ("clean", &definition.clean, false),
            ("cache_cleanup", &definition.cache_cleanup, false),
        ] {
            if command.is_empty() { continue; }
            validate_command(command, placeholders).map_err(|e| format!("{name}.{operation}: {e}"))?;
            if placeholders && !command.iter().any(|arg| arg.contains("{id}")) {
                return Err(format!("{name}.{operation} must contain {{id}} to restrict execution to approved targets"));
            }
        }
        for arg in &definition.no_cache_args { validate_argument(arg, false)?; }
        for (flag, action) in &definition.flags {
            if core || !crate::utils::is_safe_pkg_name(flag) || flag.contains('.') || flag.len() > 64
                || action.args.is_empty() && action.command.is_empty() {
                return Err(format!("{name}: invalid or empty custom flag '{flag}'"));
            }
            for arg in &action.args { validate_argument(arg, false)?; }
            if !action.command.is_empty() { validate_command(&action.command, false)?; }
        }
        for (key, value) in &definition.env {
            if key.is_empty() || !key.bytes().enumerate().all(|(i, c)| c == b'_' || c.is_ascii_alphabetic() || i > 0 && c.is_ascii_digit())
                || matches!(key.as_str(), "HOME" | "PATH" | "LD_PRELOAD" | "LD_LIBRARY_PATH" | "TMPDIR" | "CODEX_HOME")
                || value.contains('\0') {
                return Err(format!("{name}: invalid or reserved environment key '{key}'"));
            }
        }
        if (!definition.upgrade_ram.is_empty() || !definition.upgrade_storage.is_empty())
            && definition.scratch_mib.is_none_or(|size| size == 0 || size > 1_048_576) {
            return Err(format!("{name}: scratch variants require scratch_mib (1–1048576) for capacity checks"));
        }
    }
    Ok(())
}

pub fn select_flags(config: &crate::config::Config, flags: &[String]) -> Result<crate::config::Config, String> {
    let mut selected = config.clone();
    for argument in crate::runtime::custom_flags(flags) {
        let (manager, flag) = argument.trim_start_matches("--").split_once('.').ok_or("Malformed custom manager flag")?;
        let definition = selected.manager_file.managers.get_mut(manager)
            .ok_or_else(|| format!("Unknown manager in {argument}; configure it in managers.toml"))?;
        if !definition.flags.contains_key(flag) { return Err(format!("Unknown custom flag {argument}")); }
        let legacy = match manager { "flatpak" => config.integrations.flatpak, "nix" => config.integrations.nix, "pipx" => config.integrations.pipx, _ => false };
        if !definition.enabled.unwrap_or(selected.features.integrations && legacy) {
            return Err(format!("{argument}: enable integrations and this manager before using its flags"));
        }
        if !definition.selected_flags.iter().any(|value| value == flag) { definition.selected_flags.push(flag.into()); }
    }
    Ok(selected)
}

fn validate_command(command: &[String], placeholders: bool) -> Result<(), String> {
    let executable = &command[0];
    if executable.is_empty() || executable.starts_with('-') || executable.contains(['{', '}']) {
        return Err("Executable must be a literal program name or absolute path".into());
    }
    for arg in command { validate_argument(arg, placeholders)?; }
    Ok(())
}
fn validate_argument(arg: &str, placeholders: bool) -> Result<(), String> {
    if arg.chars().any(char::is_control) { return Err("Command arguments cannot contain control characters".into()); }
    let mut remaining = arg.to_string();
    if placeholders {
        for key in ["{id}", "{version}", "{revision}", "{scope}"] { remaining = remaining.replace(key, ""); }
    }
    if remaining.contains(['{', '}']) { return Err("Unknown or inappropriate command placeholder".into()); }
    Ok(())
}

/// Explicit initialization only. Read-only checks never create configuration,
/// and existing user files are never overwritten.
pub fn initialize(directory: &Path) -> io::Result<Vec<String>> {
    std::fs::create_dir_all(directory)?;
    let mut created = Vec::new();
    for (name, body) in [("managers.toml", DEFAULT_MANAGERS), ("sources.toml", DEFAULT_SOURCES), ("hooks.toml", DEFAULT_HOOKS)] {
        let path = directory.join(name);
        let temporary = crate::download::create_secure_temp_file(directory, ".pacpin-config", "tmp").map_err(io::Error::other)?;
        let result = (|| {
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new().write(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&temporary)?;
            file.write_all(body.as_bytes())?; file.sync_all()?;
            match std::fs::hard_link(&temporary, &path) {
                Ok(()) => { created.push(path.display().to_string()); Ok(()) },
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
                Err(error) => Err(error),
            }
        })();
        let _ = std::fs::remove_file(temporary);
        result?;
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shipped_defaults_are_valid_and_do_not_enable_new_managers() {
        let file: ManagerFile = toml::from_str(DEFAULT_MANAGERS).unwrap();
        validate(&file).unwrap();
        assert_eq!(file.managers.len(), 5);
        assert!(file.managers.values().all(|d| d.enabled.is_none()));
    }
    #[test]
    fn rejects_core_command_overrides_and_unknown_fields() {
        for body in [
            "[managers.pacman]\nadapter='pacman'\nupgrade=['sh','{id}']",
            "[managers.aur]\nadapter='aur'\nenabled=false",
            "[managers.x]\nadapter='command'\ncheck=['x']\nupgrade=['x','--all']",
            "[managers.x]\nadapter='command'\ncheck=['x']\nupgrade=['x','{id}']\nupgrade_ram=['x','{id}']",
        ] { assert!(validate(&toml::from_str(body).unwrap()).is_err(), "{body}"); }
        assert!(toml::from_str::<ManagerFile>("[managers.x]\nadapter='pipx'\nupgarde=['x']").is_err());
    }
    #[test]
    fn accepts_literal_commands_and_composable_variants() {
        let body = "[managers.custom]\nadapter='command'\nenabled=true\ncheck=['/opt/custom/check']\nverify=['/opt/custom/verify','{id}','{version}']\nupgrade=['/opt/custom/update','--','{id}','{version}']\nupgrade_ram=['/opt/custom/update','--','{id}']\nscratch_mib=32\nno_cache_args=['--no-cache']\n";
        validate(&toml::from_str(body).unwrap()).unwrap();
    }
    #[test]
    fn preparation_requires_explicit_target_scope_and_cannot_override_core() {
        for body in [
            "[managers.pacman]\nadapter='pacman'\nprepare=['tool','{id}']",
            "[managers.aur]\nadapter='aur'\nprepare=['tool','{id}']",
            "[managers.nix]\nadapter='nix'\nprepare=['nix','copy','--all']",
            "[managers.nix]\nadapter='nix'\nprepare=['{id}']",
        ] { assert!(validate(&toml::from_str(body).unwrap()).is_err(), "{body}"); }
        let body = "[managers.nix]\nadapter='nix'\nprepare=['wrapper','download','--','{id}','{revision}']";
        validate(&toml::from_str(body).unwrap()).unwrap();
    }
    #[test]
    fn initialization_preserves_user_files_and_leaves_no_partial_files() {
        let directory = std::env::temp_dir().join(format!("pacpin-manager-init-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("hooks.toml"), "# user's hooks\n").unwrap();
        assert_eq!(initialize(&directory).unwrap().len(), 2);
        assert!(initialize(&directory).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(directory.join("hooks.toml")).unwrap(), "# user's hooks\n");
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 3);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn manager_keys_and_builtin_display_names_cannot_shadow_typed_adapters() {
        for name in ["pacman", "aur", "flatpak", "nix", "pipx", "Flatpak", "Nix", "Pipx"] {
            let body = format!("[managers.{name}]\nadapter='command'\ncheck=['tool','check']\nupgrade=['tool','upgrade','{{id}}']\nverify=['tool','verify','{{id}}']");
            let file: ManagerFile = toml::from_str(&body).unwrap();
            assert!(validate(&file).is_err(), "reserved identity {name} accepted");
        }
        // Core AUR and an external namesake have distinct stage identities.
        let file: ManagerFile = toml::from_str("[managers.AUR]\nadapter='command'\ncheck=['tool','check']\nupgrade=['tool','upgrade','{id}']\nverify=['tool','verify','{id}']").unwrap();
        assert!(validate(&file).is_ok());
    }
}
