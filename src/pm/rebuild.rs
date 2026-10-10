//! Rebuild installed AUR targets through the same helper execution boundary.
use super::{AlpmManager, AurOperation, Config, InstallOptions};

pub fn validate_rebuild(config: &Config, packages: &[String]) -> Result<(), String> {
    if !super::workspace::is_paru(&config.options.helper) {
        return Err("AUR rebuild hooks currently require Paru".into());
    }
    super::validate_aur_scratch(config, &[])?;
    let manager = AlpmManager::with_repo_order(&config.repo_order).map_err(|e| e.to_string())?;
    for name in packages {
        if !crate::utils::is_safe_pkg_name(name) || crate::sources::owner(config, name).is_some() {
            return Err(format!("Cannot rebuild '{name}' as an AUR package"));
        }
        if manager.handle().localdb().pkg(name.as_str()).is_err()
            || manager
                .handle()
                .syncdbs()
                .into_iter()
                .any(|db| db.pkg(name.as_str()).is_ok())
        {
            return Err(format!(
                "AUR rebuild target '{name}' must already be installed and absent from native repositories"
            ));
        }
    }
    Ok(())
}

pub struct RebuildOutcome {
    pub result: Result<bool, String>,
    pub cache: Option<bool>,
}

pub fn rebuild_aur(config: &Config, packages: &[String], noconfirm: bool) -> RebuildOutcome {
    let preflight = validate_rebuild(config, packages)
        .and_then(|()| crate::preview::check_package_root_capacity(0, 0));
    if let Err(error) = preflight {
        return RebuildOutcome {
            result: Err(error),
            cache: None,
        };
    }
    let result = execute_rebuild(config, packages, noconfirm);
    let cache = Some(result.as_ref().is_ok_and(|ok| *ok));
    RebuildOutcome { result, cache }
}

fn execute_rebuild(config: &Config, packages: &[String], noconfirm: bool) -> Result<bool, String> {
    let options = InstallOptions {
        needed: false,
        noconfirm,
        // Cache cleanup is deferred to the outer transaction after verification.
        no_cache: false,
        ..Default::default()
    };
    let execution = super::execute_aur(config, packages, &options, AurOperation::Rebuild, false)
        .map_err(|e| e.to_string())?;
    if !execution.status.success() {
        eprintln!(":: AUR rebuild failed with {}", execution.status);
        return Ok(false);
    }
    execution.verification?;
    crate::progress::completed(crate::progress::Manager::Aur);
    Ok(true)
}

pub(super) fn verify_installed(config: &Config, packages: &[String]) -> Result<(), String> {
    let manager = AlpmManager::with_repo_order(&config.repo_order).map_err(|e| e.to_string())?;
    for name in packages {
        let package = manager
            .handle()
            .localdb()
            .pkg(name.as_str())
            .map_err(|_| format!("Rebuild target '{name}' is no longer installed"))?;
        let dependencies: Vec<_> = package
            .depends()
            .iter()
            .map(|dep| dep.to_string())
            .collect();
        super::ensure_independent(&manager, name, &dependencies, &[])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preflight_failure_does_not_claim_an_aur_transaction() {
        let mut config = Config::default();
        config.options.helper = "yay".into();
        let outcome = rebuild_aur(&config, &["fixture".into()], true);
        assert!(outcome.result.is_err());
        assert_eq!(outcome.cache, None);
    }
}
