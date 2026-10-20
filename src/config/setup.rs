//! Setup snapshots and backups share the ordinary configuration lock/writer.
use super::{load_supplements, lock_config, write_config_unlocked, Config};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const FILES: [&str; 4] = ["config.toml", "managers.toml", "sources.toml", "hooks.toml"];

/// Retain exact bytes, including comments, to detect edits during interaction.
pub(crate) struct Snapshot {
    path: PathBuf,
    files: Vec<Option<Vec<u8>>>,
    pub(crate) config: Config,
}

impl Snapshot {
    pub(crate) fn load(path: &Path, reset: bool) -> Result<Self, String> {
        let files = read_files(path)?;
        let mut config = if reset {
            Config::default()
        } else if let Some(body) = &files[0] {
            toml::from_str(std::str::from_utf8(body).map_err(|e| e.to_string())?).map_err(|e| {
                format!(
                    "Invalid {}: {e}. Fix it or use init --reset (with backup).",
                    path.display()
                )
            })?
        } else {
            Config::default()
        };
        load_supplements(&mut config, path)?;
        validate(&config)?;
        if read_files(path)? != files {
            return Err("Configuration changed while loading setup; run init again.".into());
        }
        Ok(Self {
            path: path.into(),
            files,
            config,
        })
    }

    pub(crate) fn has_main(&self) -> bool {
        self.files[0].is_some()
    }

    /// Back up every replaced main file, including malformed files on reset.
    /// Supplemental files stay user-owned and are never rewritten by setup.
    pub(crate) fn save(&self, updated: &Config) -> Result<Option<PathBuf>, String> {
        let _lock = lock_config(&self.path).map_err(|e| e.to_string())?;
        if read_files(&self.path)? != self.files {
            return Err(
                "Configuration changed during setup. Run init again to include those changes."
                    .into(),
            );
        }
        if updated.manager_file != self.config.manager_file
            || updated.manager_base_options != self.config.manager_base_options
            || updated.external_sources != self.config.external_sources
            || updated.external_hooks != self.config.external_hooks
        {
            return Err(
                "Setup cannot rewrite supplemental definitions; edit their owning files.".into(),
            );
        }
        for (manager, owned, changed) in [
            (
                "pacman",
                self.config
                    .manager_file
                    .managers
                    .get("pacman")
                    .is_some_and(|d| d.privilege_tool.is_some()),
                updated.options.privilege_tool != self.config.options.privilege_tool,
            ),
            (
                "aur",
                self.config
                    .manager_file
                    .managers
                    .get("aur")
                    .is_some_and(|d| d.helper.is_some()),
                updated.options.helper != self.config.options.helper,
            ),
        ] {
            if owned && changed {
                return Err(format!(
                    "{manager} tool is owned by managers.toml; edit that file directly."
                ));
            }
        }
        validate(updated)?;
        let backup = if let Some(body) = &self.files[0] {
            let directory = self.path.parent().unwrap_or_else(|| Path::new("."));
            let path =
                crate::download::create_secure_temp_file(directory, "config.toml.backup", "toml")?;
            let result = (|| {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&path)?;
                file.write_all(body)?;
                file.sync_all()
            })();
            if let Err(error) = result {
                let _ = fs::remove_file(&path);
                return Err(format!("Cannot back up configuration: {error}"));
            }
            Some(path)
        } else {
            None
        };
        write_config_unlocked(updated, &self.path)
            .map_err(|e| format!("Cannot save configuration: {e}"))?;
        Ok(backup)
    }
}

fn validate(config: &Config) -> Result<(), String> {
    super::validate_helper(&config.options.helper)?;
    super::validate_privilege_tool(&config.options.privilege_tool)?;
    crate::manager_config::validate(&config.manager_file)?;
    crate::hooks::validate(config)?;
    crate::sources::validate_config(config)
}

fn read_files(path: &Path) -> Result<Vec<Option<Vec<u8>>>, String> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    FILES
        .iter()
        .map(|name| {
            let file_path = if *name == "config.toml" {
                path.to_path_buf()
            } else {
                directory.join(name)
            };
            let file = match fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&file_path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(format!("Cannot read {}: {error}", file_path.display())),
            };
            if !file.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err(format!("{} must be a regular file", file_path.display()));
            }
            let mut body = Vec::new();
            file.take(1_048_577)
                .read_to_end(&mut body)
                .map_err(|e| e.to_string())?;
            if body.len() > 1_048_576 {
                return Err(format!(
                    "{} exceeds the 1 MiB configuration limit",
                    file_path.display()
                ));
            }
            Ok(Some(body))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Directory(PathBuf);
    impl Directory {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn directory() -> Directory {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("pacpin-setup-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Directory(path)
    }

    #[test]
    fn saves_new_configuration_and_backs_up_exact_old_bytes() {
        let directory = directory();
        let path = directory.path().join("config.toml");
        let snapshot = Snapshot::load(&path, false).unwrap();
        assert!(!snapshot.has_main());
        assert!(snapshot.save(&snapshot.config).unwrap().is_none());
        fs::write(&path, "# preserve me\n[options]\nprivilege_tool='pkexec'\n").unwrap();
        let old = fs::read(&path).unwrap();
        let snapshot = Snapshot::load(&path, false).unwrap();
        let backup = snapshot.save(&snapshot.config).unwrap().unwrap();
        assert_eq!(fs::read(backup).unwrap(), old);
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn reset_can_back_up_malformed_main_but_never_ignores_invalid_supplements() {
        let directory = directory();
        let path = directory.path().join("config.toml");
        fs::write(&path, "broken = [").unwrap();
        assert!(Snapshot::load(&path, false).is_err());
        let snapshot = Snapshot::load(&path, true).unwrap();
        let backup = snapshot.save(&snapshot.config).unwrap().unwrap();
        assert_eq!(fs::read_to_string(backup).unwrap(), "broken = [");
        fs::write(directory.path().join("managers.toml"), "broken = [").unwrap();
        assert!(Snapshot::load(&path, true).is_err());
    }

    #[test]
    fn rejects_concurrent_edits_to_each_file_including_comment_only_changes() {
        for name in FILES {
            let directory = directory();
            let path = directory.path().join("config.toml");
            let snapshot = Snapshot::load(&path, true).unwrap();
            fs::write(directory.path().join(name), "# concurrent edit\n").unwrap();
            assert!(snapshot
                .save(&snapshot.config)
                .unwrap_err()
                .contains("changed during setup"));
            assert!(!fs::read_dir(directory.path()).unwrap().any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("backup")));
        }
    }

    #[test]
    fn preserves_supplement_bytes_and_does_not_duplicate_sources_or_hooks() {
        let directory = directory();
        let path = directory.path().join("config.toml");
        let managers =
            "# user commands\n[managers.pacman]\nadapter='pacman'\nprivilege_tool='pkexec'\n";
        fs::write(directory.path().join("managers.toml"), managers).unwrap();
        let hook = "# user hooks\n[hooks.example]\nwatch=['mutter']\naction='script'\ncommand=['/usr/bin/true']\n";
        let source =
            "# user recipe\n[sources.local]\nkind='local'\nurl='/tmp/catalog.json'\npackages={}\n";
        fs::write(directory.path().join("hooks.toml"), hook).unwrap();
        fs::write(directory.path().join("sources.toml"), source).unwrap();
        let snapshot = Snapshot::load(&path, false).unwrap();
        assert_eq!(snapshot.config.options.privilege_tool, "pkexec");
        assert_eq!(snapshot.config.hooks.len(), 1);
        assert_eq!(snapshot.config.sources.len(), 1);
        snapshot.save(&snapshot.config).unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("managers.toml")).unwrap(),
            managers
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("hooks.toml")).unwrap(),
            hook
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("sources.toml")).unwrap(),
            source
        );
        let main: Config = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(main.options.privilege_tool, "sudo");
        assert!(main.sources.is_empty() && main.hooks.is_empty());
    }

    #[test]
    fn refuses_symlinks_and_nonregular_configuration_files() {
        let directory = directory();
        let path = directory.path().join("config.toml");
        let sentinel = directory.path().join("sentinel");
        fs::write(&sentinel, "# keep\n").unwrap();
        std::os::unix::fs::symlink(&sentinel, &path).unwrap();
        assert!(Snapshot::load(&path, true).is_err());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(Snapshot::load(&path, true).is_err());
        assert_eq!(fs::read_to_string(sentinel).unwrap(), "# keep\n");
    }

    #[test]
    fn saving_cannot_bypass_manager_owned_tool_preferences() {
        let directory = directory();
        let path = directory.path().join("config.toml");
        fs::write(directory.path().join("managers.toml"), "[managers.pacman]\nadapter='pacman'\nprivilege_tool='pkexec'\n[managers.aur]\nadapter='aur'\nhelper='yay'\n").unwrap();
        let snapshot = Snapshot::load(&path, false).unwrap();
        for helper in [true, false] {
            let mut updated = snapshot.config.clone();
            if helper {
                updated.options.helper = "paru".into();
            } else {
                updated.options.privilege_tool = "sudo".into();
            }
            assert!(snapshot
                .save(&updated)
                .unwrap_err()
                .contains("owned by managers.toml"));
            assert!(!path.exists());
        }
    }
}
