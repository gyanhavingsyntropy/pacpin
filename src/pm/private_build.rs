//! Install inspected artifacts, keeping build-only dependencies in private roots.
use crate::{
    config::Config,
    db::AlpmManager,
    preparation::{Options, PackageRoot},
};
use std::{collections::HashSet, path::PathBuf, process::ExitStatus};

pub(super) enum Failure {
    Build(String),
    Other(String),
}
impl From<String> for Failure {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Build(error) | Self::Other(error) => f.write_str(error),
        }
    }
}

pub(super) fn aur(
    config: &Config,
    targets: &[String],
    options: &super::InstallOptions,
) -> Result<ExitStatus, Failure> {
    if !options.forwarded_flags.is_empty() || options.refresh || options.sysupgrade {
        return Err(Failure::Other("--cleanup=temporary AUR builds cannot forward helper flags or perform a nested system upgrade".into()));
    }
    crate::progress::package_preparation(crate::progress::Manager::Aur, || {
        let preparation = Options {
            no_cache: options.no_cache,
            allow_unverified: false,
            noconfirm: options.noconfirm,
            prefix: "pacpin-build",
        };
        let mut root = PackageRoot::new(config, "aur", &preparation, 128 * 1024 * 1024)?;
        let mut main = Vec::new();
        let names: Vec<_> = targets
            .iter()
            .map(|target| target.rsplit('/').next().unwrap_or(target).to_owned())
            .collect();
        for name in &names {
            main.push(root.build_aur(name).map_err(|error| {
                if root.build_failed {
                    Failure::Build(error)
                } else {
                    Failure::Other(error)
                }
            })?);
        }
        let archives = root.installation_archives(&main)?;
        println!(":: Installing requested packages and runtime dependencies; build-only dependencies remain temporary.");
        let status = install_archives(
            config,
            &archives,
            &names,
            options.needed,
            options.noconfirm,
            crate::progress::Manager::Aur,
        )
        .map_err(Failure::Other)?;
        root.cleanup(crate::progress::Manager::Aur)?;
        Ok(status)
    })
}

/// The only privileged step in private preparation. ALPM validates the complete
/// runtime closure before approval reaches the native commit boundary.
pub(crate) fn install_archives(
    config: &Config,
    archives: &[PathBuf],
    explicit: &[String],
    needed: bool,
    noconfirm: bool,
    manager_kind: crate::progress::Manager,
) -> Result<ExitStatus, String> {
    super::check_lock()?;
    if archives.is_empty() || explicit.is_empty() {
        return Err("Empty private installation plan".into());
    }
    let mut manager =
        AlpmManager::with_repo_order(&config.repo_order).map_err(|e| e.to_string())?;
    let explicit: HashSet<_> = explicit.iter().map(String::as_str).collect();
    let mut expected = Vec::new();
    let mut dependencies = Vec::new();
    let mut installed_size = 0i64;
    let mut previous_size = 0i64;
    for archive in archives {
        let package = manager
            .handle()
            .pkg_load(
                archive.as_os_str().as_encoded_bytes().to_vec(),
                true,
                alpm::SigLevel::NONE,
            )
            .map_err(|e| e.to_string())?;
        let before = manager.handle().localdb().pkg(package.name()).ok();
        installed_size = installed_size.saturating_add(package.isize());
        previous_size = previous_size.saturating_add(before.map_or(0, |p| p.isize()));
        if !explicit.contains(package.name())
            && before.is_none_or(|p| p.reason() == alpm::PackageReason::Depend)
        {
            dependencies.push(package.name().to_owned());
        }
        expected.push((
            package.name().to_owned(),
            package.version().to_string(),
            before.map(|p| p.version().to_string()),
        ));
    }
    crate::preview::check_package_root_capacity(installed_size, previous_size)?;
    let handle = manager.handle_mut();
    handle
        .trans_init(alpm::TransFlag::NO_LOCK)
        .map_err(|e| e.to_string())?;
    let validation: Result<(), String> = (|| {
        for archive in archives {
            let package = handle
                .pkg_load(
                    archive.as_os_str().as_encoded_bytes().to_vec(),
                    true,
                    alpm::SigLevel::NONE,
                )
                .map_err(|e| e.to_string())?;
            handle.trans_add_pkg(package).map_err(|e| e.to_string())?;
        }
        handle
            .trans_prepare()
            .map_err(|e| format!("Private build runtime transaction cannot be prepared: {e}"))?;
        if !handle.trans_remove().is_empty() || handle.trans_add().len() > archives.len() {
            return Err(
                "Private installation would remove packages or add unprepared dependencies".into(),
            );
        }
        Ok(())
    })();
    handle.trans_release().map_err(|e| e.to_string())?;
    validation?;
    drop(manager);
    let mut command = crate::privilege::command(&config.options.privilege_tool);
    command
        .args(["pacman", "-U"])
        .args(crate::progress::pacman_progress_args());
    if needed {
        command.arg("--needed");
    }
    if noconfirm { command.arg("--noconfirm"); }
    command.arg("--").args(archives);
    let status = crate::progress::run(
        &mut command,
        manager_kind,
        "Installing prepared packages",
        || {},
    )
    .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("Native installation of prepared packages failed; installed files may have changed. Later work must stop.".into());
    }
    let fresh = AlpmManager::with_repo_order(&config.repo_order).map_err(|e| e.to_string())?;
    for (name, version, _) in &expected {
        if !fresh
            .handle()
            .localdb()
            .pkg(name.as_str())
            .is_ok_and(|p| p.version().as_str() == version)
        {
            return Err(format!(
                "Prepared package '{name}' did not reach its approved built version"
            ));
        }
    }
    drop(fresh);
    if !dependencies.is_empty() {
        let names: Vec<_> = dependencies.iter().map(String::as_str).collect();
        let result = super::mark_dependencies(config, &names).map_err(|e| e.to_string())?;
        if !result.success() {
            return Err(
                "Packages installed, but restoring runtime dependency reasons failed".into(),
            );
        }
    }
    let records = expected
        .into_iter()
        .map(|(name, version, before)| {
            let mut record =
                serde_json::json!({"name":name, "to_version":version, "state":"private-build"});
            if let Some(before) = before {
                record["from_version"] = before.into();
            }
            record
        })
        .collect();
    if let Err(error) = crate::journal::TransactionJournal::record_transaction(
        "install",
        records,
        "pacpin private build; pacman -U runtime closure",
    ) {
        eprintln!("Warning: Packages installed, but history could not be saved: {error}");
    }
    Ok(status)
}
