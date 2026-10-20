//! Repository-aware conflict and stability-delay checks before installation.
use std::collections::{HashMap, HashSet};
use std::io::{self, IsTerminal, Write};

use colored::Colorize;

use super::InstallOptions;
use crate::aur;
use crate::config::Config;
use crate::db::AlpmManager;
use crate::resolver::{ResolvedPackage, ResolverEngine};

/// Inspects candidate packages against installed packages to identify potential conflicts.
pub fn check_package_conflicts(manager: &AlpmManager, targets: &[String]) -> Vec<(String, String)> {
    let candidates: Vec<&alpm::Package> = targets.iter().filter_map(|target| {
        crate::db::find_sync_package(manager.handle(), target).ok()
    }).collect();
    check_candidate_conflicts(manager, &candidates)
}

/// Checks the exact candidates in the upgrade plan without resolving their names again.
pub fn check_resolved_package_conflicts(
    manager: &AlpmManager,
    updates: &[ResolvedPackage],
) -> Vec<(String, String)> {
    let candidates: Vec<&alpm::Package> = updates.iter().filter_map(|update| {
        let candidate = update.candidate.as_ref().filter(|candidate| !candidate.is_aur)?;
        manager.handle().syncdbs().into_iter()
            .find(|db| db.name() == candidate.repo)
            .and_then(|db| db.pkg(candidate.name.as_str()).ok())
    }).collect();
    check_candidate_conflicts(manager, &candidates)
}

fn check_candidate_conflicts(
    manager: &AlpmManager,
    candidates: &[&alpm::Package],
) -> Vec<(String, String)> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let candidate_names: HashSet<&str> = candidates.iter().map(|pkg| pkg.name()).collect();
    // Inspect the proposed versions, not old versions of packages being upgraded.
    let packages: Vec<&alpm::Package> = candidates.iter().copied().chain(
        manager.handle().localdb().pkgs().into_iter()
            .filter(|pkg| !candidate_names.contains(pkg.name()))
    ).collect();
    let mut satisfiers: HashMap<&str, Vec<&alpm::Package>> = HashMap::new();
    let mut reverse_conflicts: HashMap<&str, Vec<&alpm::Package>> = HashMap::new();
    for pkg in &packages {
        satisfiers.entry(pkg.name()).or_default().push(pkg);
        for provide in pkg.provides() {
            satisfiers.entry(provide.name()).or_default().push(pkg);
        }
        for conflict in pkg.conflicts() {
            reverse_conflicts.entry(conflict.name()).or_default().push(pkg);
        }
    }

    let mut checked = HashSet::new();
    let mut conflicts = Vec::new();
    for candidate in candidates {
        // Name indexes only narrow the possible pairs. ALPM alone decides whether
        // versions, epochs and virtual provides satisfy the actual conflict.
        let mut potential: HashMap<&str, &alpm::Package> = HashMap::new();
        for conflict in candidate.conflicts() {
            if let Some(packages) = satisfiers.get(conflict.name()) {
                for pkg in packages {
                    potential.insert(pkg.name(), pkg);
                }
            }
        }
        for name in std::iter::once(candidate.name()).chain(candidate.provides().into_iter().map(|p| p.name())) {
            if let Some(packages) = reverse_conflicts.get(name) {
                for pkg in packages {
                    potential.insert(pkg.name(), pkg);
                }
            }
        }
        for other in potential.values() {
            let name = candidate.name();
            let other_name = other.name();
            if name == other_name {
                continue;
            }
            let key = if name < other_name { (name, other_name) } else { (other_name, name) };
            if !checked.insert(key) {
                continue;
            }
            let pair = [*candidate, *other];
            if !manager.handle().check_conflicts(pair.iter()).is_empty() {
                let pair = if candidate_names.contains(other_name) {
                    key
                } else {
                    (name, other_name)
                };
                conflicts.push((pair.0.to_string(), pair.1.to_string()));
            }
        }
    }
    conflicts.sort();
    conflicts
}

/// Verifies stability delay for the target package and its uninstalled dependency graph.
pub(super) fn verify_target_and_dependencies_delay(
    target_pkg: &str,
    repo: Option<&str>,
    config: &Config,
    glob_delays: &[(String, glob::Pattern, u32)],
    manager: &AlpmManager,
    now_epoch: i64,
    options: &InstallOptions,
) -> Result<(), String> {
    if !config.features.stability_delays {
        return Ok(());
    }

    // 1. Check target package itself
    check_single_pkg_delay(
        target_pkg,
        target_pkg,
        repo,
        false,
        config,
        glob_delays,
        manager,
        now_epoch,
        options,
    )?;

    // 2. Discover uninstalled direct dependencies and check their delay buffers
    let mut direct_deps = Vec::new();
    if let Some(r) = repo {
        if r.eq_ignore_ascii_case("aur") {
            let aur_map = aur::query_aur(&[target_pkg.to_string()]);
            if let Some(item) = aur_map.get(target_pkg) {
                for d in &item.depends {
                    direct_deps.push(d.to_string());
                }
            }
        } else if let Ok(pkg) = crate::db::find_sync_package(manager.handle(), &format!("{r}/{target_pkg}")) {
            for d in pkg.depends() {
                direct_deps.push(d.to_string());
            }
        }
    } else if let Ok(pkg) = crate::db::find_sync_package(manager.handle(), target_pkg) {
        for d in pkg.depends() {
            direct_deps.push(d.to_string());
        }
    }

    let local_db = manager.handle().localdb();
    let mut checked_deps = HashSet::new();

    for dep_name in direct_deps {
        if !checked_deps.insert(dep_name.clone()) {
            continue;
        }

        // If dependency is already satisfied locally, delay buffer is irrelevant
        if local_db.pkgs().find_satisfier(&*dep_name).is_some() {
            continue;
        }

        check_single_pkg_delay(
            &dep_name,
            target_pkg,
            None,
            true,
            config,
            glob_delays,
            manager,
            now_epoch,
            options,
        )?;
    }

    Ok(())
}

/// Evaluates a single package against delay rules.
#[allow(clippy::too_many_arguments)]
fn check_single_pkg_delay(
    pkg: &str,
    root_target: &str,
    repo: Option<&str>,
    is_dependency: bool,
    config: &Config,
    glob_delays: &[(String, glob::Pattern, u32)],
    manager: &AlpmManager,
    now_epoch: i64,
    options: &InstallOptions,
) -> Result<(), String> {
    // If already installed locally or satisfied by virtual provide, skip
    let local_db = manager.handle().localdb();
    if local_db.pkgs().find_satisfier(pkg).is_some() {
        return Ok(());
    }

    let package_name = aur::clean_dep_name(pkg);
    let req_days = match ResolverEngine::match_delay_days(package_name, &config.delay, glob_delays) {
        Some(d) => d,
        None => return Ok(()),
    };

    let build_date: Option<i64> = if let Some(r) = repo {
        if r.eq_ignore_ascii_case("aur") {
            let aur_map = aur::query_aur(&[package_name.to_string()]);
            aur_map.get(package_name).and_then(|item| item.last_modified)
        } else {
            crate::db::find_sync_package(manager.handle(), &format!("{r}/{pkg}"))
                .ok()
                .map(|p| p.build_date())
        }
    } else {
        crate::db::find_sync_package(manager.handle(), pkg)
            .ok()
            .map(|p| p.build_date())
    };

    if let Some(bdate) = build_date {
        let age_days = (now_epoch - bdate) as f64 / 86400.0;
        let req_days_f = req_days as f64;
        if age_days < req_days_f {
            let days_left = ((req_days_f - age_days + 0.99) as u32).max(1);
            let context_label = if is_dependency {
                format!("Dependency '{}' (required by '{}')", pkg, root_target)
            } else {
                format!("Package '{}'", pkg)
            };

            if options.noconfirm || !io::stdin().is_terminal() {
                println!(
                    "{} {} was released {:.1} days ago (within your {}d stability delay buffer, {}d remaining). Proceeding in non-interactive mode.",
                    ":: Warning:".yellow().bold(),
                    context_label,
                    age_days.max(0.0),
                    req_days,
                    days_left
                );
            } else {
                print!(
                    "\n{} {} was released {:.1} days ago (within your {}d stability delay buffer, {}d remaining).\n   Arch mirrors only host the latest version. Proceed with installation? [Y/n] ",
                    ":: Notice:".yellow().bold(),
                    context_label,
                    age_days.max(0.0),
                    req_days,
                    days_left
                );
                let _ = io::stdout().flush();
                let mut input = String::new();
                if io::stdin().read_line(&mut input).is_ok() {
                    let trimmed = input.trim().to_lowercase();
                    if trimmed == "n" || trimmed == "no" {
                        return Err(format!(
                            "Installation cancelled: {} violates active {}-day stability delay policy ({}d remaining).",
                            context_label, req_days, days_left
                        ));
                    }
                }
            }
        }
    }

    Ok(())
}
