//! Retained unprivileged applications, independently inventoried from ALPM.
mod storage;
#[cfg(test)]
mod tests;
use super::{Config, Format, Plan, Recipe};
use std::{fs, os::unix::fs::MetadataExt, path::Path};
pub(crate) use storage::Receipt;

pub(super) fn validate(recipe: &Recipe) -> Result<(), String> {
    if recipe.version_scheme == super::VersionScheme::Arch
        && (recipe.install_dir.is_none()
            || !matches!(
                recipe.channel,
                super::Channel::Stable | super::Channel::Prerelease
            ))
    {
        return Err("version_scheme='arch' requires install_dir and a stable/prerelease channel; use tag_pattern to select vendor variants".into());
    }
    let Some(path) = &recipe.install_dir else {
        return Ok(());
    };
    storage::validate_path(path)?;
    if recipe.format == Format::Native {
        return Err("install_dir supports archive, binary and source recipes; native package relocation is not declared".into());
    }
    Ok(())
}

pub(crate) fn installed(
    config: &Config,
    alias: &str,
    name: &str,
) -> Result<Option<Receipt>, String> {
    let Some(path) = config
        .sources
        .get(alias)
        .and_then(|source| source.packages.get(name))
        .and_then(|recipe| recipe.install_dir.as_ref())
    else {
        return Ok(None);
    };
    let receipt = storage::read(path, alias, name)?;
    if receipt.is_none() && config.pins.get(name).is_some_and(|owner| owner == alias) {
        return Err(format!("Managed directory {alias}/{name} is unavailable or missing its receipt at {}; check the mounted drive before reinstalling", path.display()));
    }
    Ok(receipt)
}

pub(crate) fn inventory(config: &Config) -> Result<Vec<Receipt>, String> {
    let mut receipts = Vec::new();
    for (alias, source) in &config.sources {
        for name in source.packages.keys() {
            if let Some(receipt) = installed(config, alias, name)? {
                receipts.push(receipt);
            }
        }
    }
    Ok(receipts)
}

/// Missing unmanaged directories are not adopted implicitly, even --noconfirm.
pub(super) fn preflight(
    config: &Config,
    alias: &str,
    name: &str,
    recipe: &Recipe,
) -> Result<(), String> {
    let Some(path) = &recipe.install_dir else {
        return Ok(());
    };
    if unsafe { libc::geteuid() } == 0 {
        return Err("Directory Sources must start as a regular user".into());
    }
    storage::parent(path)?;
    let previous = storage::read(path, alias, name)?;
    if fs::symlink_metadata(path).is_ok() && previous.is_none() {
        return Err(format!("{} already exists without a matching ownership record; choose a new install_dir and use 'pin source adopt {alias}/{name} --from EXISTING_APP --version VERSION' to reference it without moving its files", path.display()));
    }
    let manager =
        crate::db::AlpmManager::with_repo_order(&config.repo_order).map_err(|e| e.to_string())?;
    if manager.handle().localdb().pkg(name).is_ok() {
        return Err(format!("'{name}' is installed through pacman; directory installation cannot silently change its ownership"));
    }
    let missing: Vec<_> = recipe
        .depends
        .iter()
        .filter(|dep| {
            manager
                .handle()
                .localdb()
                .pkgs()
                .find_satisfier(dep.as_str())
                .is_none()
        })
        .collect();
    if !missing.is_empty() {
        return Err(format!("Directory applications require existing runtime dependencies: {}. Install them explicitly first.", missing.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(" ")));
    }
    Ok(())
}

fn verify_payload(plan: &Plan, root: &Path) -> Result<(), String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    crate::download::validate_symlink_traversal(&root, 32)?;
    for relative in plan.recipe.binaries.values() {
        let path = root
            .join(relative)
            .canonicalize()
            .map_err(|e| format!("Declared executable is missing: {e}"))?;
        if !path.starts_with(&root) || !path.is_file() {
            return Err("Directory executable escapes its payload".into());
        }
        super::install::verify_elf(&path, plan.recipe.format == Format::Binary)?;
        if !crate::utils::is_executable_file(&path) {
            return Err(format!("Declared executable is not executable: {relative}"));
        }
    }
    Ok(())
}

fn receipt(plan: &Plan, root: &Path, adopted: bool) -> Result<Receipt, String> {
    let metadata = storage::owned_directory(root)?;
    Ok(Receipt {
        schema: 1,
        source: plan.alias.clone(),
        package: plan.name.clone(),
        directory: plan
            .recipe
            .install_dir
            .clone()
            .ok_or("Missing install_dir")?,
        version: plan.version.clone(),
        tag: plan.tag.clone(),
        sha256: if adopted {
            None
        } else {
            plan.asset.sha256.clone()
        },
        adopted,
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

pub(super) fn execute(
    config: &Config,
    plan: &Plan,
    needed: bool,
    permit: Option<&crate::pm::AurBuildFailurePermit>,
    noconfirm: bool,
) -> Result<(), String> {
    let path = plan
        .recipe
        .install_dir
        .as_deref()
        .ok_or("Missing install_dir")?;
    let _lock = storage::Lock::acquire(path)?;
    preflight(config, &plan.alias, &plan.name, &plan.recipe)?;
    let previous = storage::read(path, &plan.alias, &plan.name)?;
    if needed && previous.as_ref().is_some_and(|r| r.version == plan.version) {
        println!(
            ":: {}/{} {} is already installed in {}.",
            plan.alias,
            plan.name,
            plan.version,
            path.display()
        );
        return Ok(());
    }
    if let Some(permit) = permit {
        permit.validate()?;
        let manager = crate::db::AlpmManager::with_repo_order(&config.repo_order)
            .map_err(|e| e.to_string())?;
        crate::pm::ensure_independent(
            &manager,
            &plan.name,
            &plan.recipe.depends,
            permit.failed_packages(),
        )?;
    }
    let stage = super::install::Workdir::new_with_base(&storage::parent(path)?)?;
    let root = super::install::directory_payload(config, plan, &stage.0, noconfirm)?;
    verify_payload(plan, &root)?;
    let new_receipt = receipt(plan, &root, false)?;
    storage::write(&root, &new_receipt)?;
    if let Some(permit) = permit {
        permit.validate()?;
    }
    crate::progress::operation(
        crate::progress::Manager::Sources,
        "Publishing application directory",
        || storage::publish(&root, path, previous.as_ref()),
    )?;
    if storage::read(path, &plan.alias, &plan.name)?.as_ref() != Some(&new_receipt) {
        return Err("Published directory did not verify against its receipt".into());
    }
    println!(
        ":: Installed {}/{} {} in {}.",
        plan.alias,
        plan.name,
        plan.version,
        path.display()
    );
    save_owner(config, &plan.alias, &plan.name)?;
    record(
        &new_receipt,
        previous.as_ref().map(|r| r.version.as_str()),
        "directory-install",
    );
    crate::progress::operation(
        crate::progress::Manager::Sources,
        "Removing previous application versions",
        || storage::clean_previous(path, &plan.alias, &plan.name),
    )?;
    Ok(())
}

fn save_owner(config: &Config, alias: &str, name: &str) -> Result<(), String> {
    crate::config::mutate_config(|current| {
        if current.sources.get(alias) != config.sources.get(alias) {
            return Err("Source recipe changed during the directory operation".into());
        }
        current.pins.insert(name.into(), alias.into());
        Ok(())
    })
    .map_err(|e| format!("Directory operation completed, but saving ownership failed: {e}"))
}

fn record(receipt: &Receipt, before: Option<&str>, action: &str) {
    let mut entry = serde_json::json!({"name":receipt.package, "manager":"Sources-directory", "source":receipt.source, "directory":receipt.directory, "to_version":receipt.version, "tag":receipt.tag, "sha256":receipt.sha256, "adopted":receipt.adopted});
    if let Some(version) = before {
        entry["installed_version_before"] = version.into();
    }
    if let Err(error) = crate::journal::TransactionJournal::record_transaction(
        action,
        vec![entry],
        "pacpin retained source directory",
    ) {
        eprintln!(
            "Warning: Directory operation completed, but history could not be saved: {error}"
        );
    }
}

pub(crate) fn adopt(
    config: &Config,
    target: &str,
    from: &Path,
    version: &str,
    dry_run: bool,
    noconfirm: bool,
) -> Result<(), String> {
    if unsafe { libc::geteuid() } == 0 {
        return Err("Directory adoption must start as a regular user".into());
    }
    if !storage::valid_version(version) {
        return Err(
            "Adoption needs the exact installed version (no whitespace or control characters)"
                .into(),
        );
    }
    let (alias, name) = target
        .split_once('/')
        .ok_or("Adoption needs alias/package")?;
    let source = config.sources.get(alias).ok_or("Unknown source alias")?;
    let recipe = source
        .packages
        .get(name)
        .ok_or("No recipe for the requested package")?;
    validate(recipe)?;
    let path = recipe
        .install_dir
        .as_deref()
        .ok_or("Set install_dir in this package's source recipe first")?;
    storage::parent(path)?;
    if fs::symlink_metadata(path).is_ok() {
        return Err("Adoption requires a new dedicated install_dir; the existing application is referenced using --from and is preserved".into());
    }
    let from = from
        .canonicalize()
        .map_err(|e| format!("Cannot read existing application: {e}"))?;
    storage::owned_directory(&from)?;
    if from.starts_with(path) || path.starts_with(&from) {
        return Err("Existing application and install_dir cannot overlap".into());
    }
    let manager =
        crate::db::AlpmManager::with_repo_order(&config.repo_order).map_err(|e| e.to_string())?;
    if manager.handle().localdb().pkg(name).is_ok() {
        return Err("Cannot adopt a package already installed through pacman".into());
    }
    let plan = Plan {
        alias: alias.into(),
        name: name.into(),
        source: source.clone(),
        recipe: recipe.clone(),
        tag: "adopted".into(),
        version: version.into(),
        asset: super::Asset {
            name: String::new(),
            url: String::new(),
            sha256: None,
        },
        git: None,
        persist_source: false,
        no_cache: config.options.no_cache,
    };
    verify_payload(&plan, &from)?;
    println!(":: Reference existing {target} {version} from {} using {}/current. Upstream integrity is not established by adoption; original files will be preserved.", from.display(), path.display());
    if dry_run {
        return Ok(());
    }
    super::install::confirm(noconfirm)?;
    let _lock = storage::Lock::acquire(path)?;
    let new_receipt = receipt(&plan, &from, true)?;
    verify_payload(&plan, &from)?;
    let stage = super::install::Workdir::new_with_base(&storage::parent(path)?)?;
    storage::adopt(&stage.0, path, &from, &new_receipt)?;
    save_owner(config, alias, name)?;
    record(&new_receipt, None, "directory-adopt");
    Ok(())
}

pub(crate) fn target<'a>(config: &'a Config, value: &'a str) -> Option<(&'a str, &'a str)> {
    if let Some((alias, name)) = value.split_once('/') {
        return config
            .sources
            .get(alias)?
            .packages
            .get(name)?
            .install_dir
            .as_ref()
            .map(|_| (alias, name));
    }
    let matches: Vec<_> = config
        .sources
        .iter()
        .filter_map(|(alias, source)| {
            source
                .packages
                .get(value)?
                .install_dir
                .as_ref()
                .map(|_| (alias.as_str(), value))
        })
        .collect();
    if matches.len() == 1 {
        Some(matches[0])
    } else {
        None
    }
}

pub(crate) fn remove(config: &Config, targets: &[String], flags: &[String]) -> Result<(), String> {
    if flags.iter().any(|flag| {
        !matches!(
            flag.as_str(),
            "-R" | "-Rs" | "-Rns" | "-Rn" | "--noconfirm" | "--dry-run" | "-n"
        )
    }) {
        return Err("Directory removal supports -R/-Rns, --dry-run and --noconfirm; recursive native removal flags are not forwarded".into());
    }
    let receipts = targets
        .iter()
        .map(|value| {
            let (alias, name) = target(config, value)
                .ok_or("Remove directory Sources separately from native packages")?;
            installed(config, alias, name)?
                .ok_or_else(|| format!("{value} has no owned directory installation"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for r in &receipts {
        println!(
            ":: Remove managed application directory: {}/{} ({})",
            r.source,
            r.package,
            r.directory.display()
        );
    }
    if flags
        .iter()
        .any(|f| matches!(f.as_str(), "--dry-run" | "-n"))
    {
        return Ok(());
    }
    super::install::confirm(flags.iter().any(|f| f == "--noconfirm"))?;
    for r in receipts {
        let _lock = storage::Lock::acquire(&r.directory)?;
        if storage::read(&r.directory, &r.source, &r.package)?.as_ref() != Some(&r) {
            return Err("Directory changed before removal".into());
        }
        crate::progress::operation(
            crate::progress::Manager::Sources,
            "Removing application directory",
            || crate::execution::remove_owned_tree(&r.directory),
        )
        .map_err(|e| e.to_string())?;
        crate::config::mutate_config(|current| {
            if current.pins.get(&r.package) == Some(&r.source) {
                current.pins.remove(&r.package);
            }
            Ok(())
        })
        .map_err(|e| format!("Directory removed, but saving its removal failed: {e}"))?;
        record(&r, Some(&r.version), "directory-remove");
    }
    Ok(())
}
