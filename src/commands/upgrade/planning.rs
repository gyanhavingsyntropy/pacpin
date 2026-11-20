//! Explicit target planning, ignores, delay policy and result-record conversion.
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use alpm::vercmp;

use crate::aur;
use crate::config::Config;
use crate::db::AlpmManager;
use crate::preview::{RequestedItem, RequestedStatus, strip_repo};
use crate::resolver::{ResolveResult, ResolvedPackage, ResolverEngine};

pub(super) fn validate_upgrade_targets(pacman_targets: &[String], aur_targets: &[String]) -> Result<(), String> {
    if let Some(target) = pacman_targets.iter().find(|t| !crate::utils::is_safe_install_target(t)) {
        return Err(format!("Refusing unsafe pacman upgrade target '{}'.", target));
    }
    if let Some(target) = aur_targets.iter().find(|t| !crate::utils::is_safe_pkg_name(t)) {
        return Err(format!("Refusing unsafe AUR upgrade target '{}'.", target));
    }
    Ok(())
}

/// Builds the journal/restart record for an explicitly requested native
/// package from its approved preview entry.
pub(super) fn requested_resolved_package(
    item: &RequestedItem,
    entry: &crate::preview::PreviewPackage,
) -> ResolvedPackage {
    ResolvedPackage {
        name: item.name.clone(),
        state: "explicit".to_string(),
        pinned_repo: None,
        installed_ver: item.installed_version.clone().unwrap_or_default(),
        installed_size: entry.old_isize,
        installed_db: String::new(),
        candidate: Some(crate::resolver::CandidatePackage {
            name: item.name.clone(),
            version: entry.version.clone(),
            repo: entry.repo.clone(),
            base: item.name.clone(),
            csize: 0,
            isize: entry.new_isize,
            desc: String::new(),
            builddate: 0,
            is_aur: false,
            depends: Vec::new(),
        }),
        natural_top_repo: None,
        diverted_from_top: false,
        needs_update: true,
        update_type: "explicit".to_string(),
        is_downgrade: item.status == RequestedStatus::Downgrade,
        net_delta: Some(entry.net_delta()),
        held: false,
        hold_reason: String::new(),
    }
}

/// Builds the journal record for an explicitly requested AUR package. Build
/// sizes are unknown until the helper builds them.
pub(super) fn requested_aur_resolved_package(item: &RequestedItem) -> ResolvedPackage {
    ResolvedPackage {
        name: item.name.clone(),
        state: "explicit".to_string(),
        pinned_repo: None,
        installed_ver: item.installed_version.clone().unwrap_or_default(),
        installed_size: 0,
        installed_db: String::new(),
        candidate: Some(crate::resolver::CandidatePackage {
            name: item.name.clone(),
            version: item.candidate_version.clone(),
            repo: "aur".to_string(),
            base: item.name.clone(),
            csize: 0,
            isize: 0,
            desc: String::new(),
            builddate: 0,
            is_aur: true,
            depends: Vec::new(),
        }),
        natural_top_repo: None,
        diverted_from_top: false,
        needs_update: true,
        update_type: "explicit".to_string(),
        is_downgrade: item.status == RequestedStatus::Downgrade,
        net_delta: None,
        held: false,
        hold_reason: String::new(),
    }
}

pub(super) struct RequestedTargets {
    pub(super) items: Vec<RequestedItem>,
    pub(super) native: Vec<String>,
    pub(super) aur: Vec<String>,
}

pub(super) fn requested_status(installed: Option<&str>, candidate: &str) -> RequestedStatus {
    match installed {
        None => RequestedStatus::NewInstall,
        Some(current) => match vercmp(current, candidate) {
            Ordering::Less => RequestedStatus::Upgrade,
            Ordering::Greater => RequestedStatus::Downgrade,
            Ordering::Equal => RequestedStatus::AlreadyCurrent,
        },
    }
}

/// True when `--ignore <pkg>` / `--ignore=<pkg>` explicitly names this package.
/// All package names named by `--ignore <pkg>` / `--ignore=<pkg>` (comma
/// separated) plus `--ignoregroup` (expanded to current group members).
/// Pacman skips these at execution, so they must never be recorded as applied
/// afterwards.
pub(super) fn ignored_names(extra_flags: &[String]) -> HashSet<String> {
    let mut ignored = HashSet::new();
    let mut i = 0;
    while i < extra_flags.len() {
        let flag = &extra_flags[i];
        if flag == "--" {
            break;
        }
        if let Some(list) = flag.strip_prefix("--ignore=") {
            ignored.extend(list.split(',').map(str::to_string));
            i += 1;
            continue;
        }
        if flag == "--ignore" {
            if i + 1 < extra_flags.len() {
                ignored.extend(extra_flags[i + 1].split(',').map(str::to_string));
                i += 2;
                continue;
            } else {
                i += 1;
                continue;
            }
        }
        if crate::commands::cli::takes_value(flag)
            || (flag == "--wait"
                && i + 1 < extra_flags.len()
                && extra_flags[i + 1].parse::<u64>().is_ok())
        {
            i += 2;
            continue;
        }
        i += 1;
    }
    ignored.retain(|name| !name.is_empty());
    ignored
}

/// Expands `--ignoregroup` entries to current sync-group members. Ignored
/// updates are excluded from the plan entirely: pacman answers "Install
/// anyway?" affirmatively for explicitly listed targets under --noconfirm,
/// so merely forwarding the flags would silently void ignores.
pub(super) fn collect_ignored_names(manager: &AlpmManager, extra_flags: &[String]) -> HashSet<String> {
    let mut ignored = ignored_names(extra_flags);
    let mut groups = Vec::new();
    let mut i = 0;
    while i < extra_flags.len() {
        let flag = &extra_flags[i];
        if flag == "--" {
            break;
        }
        if let Some(list) = flag.strip_prefix("--ignoregroup=") {
            groups.extend(list.split(',').map(str::to_string));
            i += 1;
            continue;
        }
        if flag == "--ignoregroup" {
            if i + 1 < extra_flags.len() {
                groups.extend(extra_flags[i + 1].split(',').map(str::to_string));
                i += 2;
                continue;
            } else {
                i += 1;
                continue;
            }
        }
        if crate::commands::cli::takes_value(flag)
            || (flag == "--wait"
                && i + 1 < extra_flags.len()
                && extra_flags[i + 1].parse::<u64>().is_ok())
        {
            i += 2;
            continue;
        }
        i += 1;
    }
    for group in groups.iter().filter(|g| !g.is_empty()) {
        for db in manager.handle().syncdbs() {
            if let Ok(g) = db.group(group.as_str()) {
                ignored.extend(g.packages().into_iter().map(|p| p.name().to_string()));
            }
        }
    }
    ignored
}

fn check_requested_delay(name: &str, builddate: i64, config: &Config, now_epoch: i64) -> Result<(), String> {
    if !config.features.stability_delays || builddate <= 0 {
        return Ok(());
    }
    let glob_delays = ResolverEngine::compile_glob_delays(&config.delay);
    if let Some(days) = ResolverEngine::match_delay_days(name, &config.delay, &glob_delays) {
        let age_days = now_epoch.saturating_sub(builddate) / 86400;
        if age_days < days as i64 {
            return Err(format!(
                "'{name}' is held by a stability delay ({days} days; {} remaining); it cannot be pulled into this upgrade.",
                (days as i64 - age_days).max(0)
            ));
        }
    }
    Ok(())
}

/// Classifies explicitly requested upgrade targets (`pin -Syu <pkg...>`)
/// into native repo targets, AUR targets, and display items. Source-owned and
/// direct upstream targets are rejected with guidance instead of being
/// silently ignored: they are already tracked by `pin -Syu`.
pub(super) fn resolve_requested_targets(
    config: &Config,
    manager: &AlpmManager,
    res: &ResolveResult,
    requested: &[String],
    ignored: &HashSet<String>,
) -> Result<RequestedTargets, String> {
    let mut out = RequestedTargets {
        items: Vec::new(),
        native: Vec::new(),
        aur: Vec::new(),
    };
    if requested.is_empty() {
        return Ok(out);
    }
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let held_by_name = |name: &str| res.held_packages.iter().any(|held| held.name == name);
    let installed_version = |name: &str| {
        manager
            .handle()
            .localdb()
            .pkg(name)
            .ok()
            .map(|pkg| pkg.version().to_string())
    };
    let pins = if config.features.pinning {
        config.pins.clone()
    } else {
        std::collections::BTreeMap::new()
    };
    let glob_pins = ResolverEngine::compile_glob_pins(&pins);

    let mut aur_candidates = Vec::new();
    for target in requested {
        if let Some((head, tail)) = target.split_once('/') {
            if head.eq_ignore_ascii_case("aur") && crate::utils::is_safe_pkg_name(tail) {
                aur_candidates.push(tail.to_string());
            }
        } else if crate::utils::is_safe_pkg_name(target) {
            let pinned_repo = ResolverEngine::match_pinned_package(target, &pins, &glob_pins);
            if pinned_repo.as_deref() == Some("aur")
                || crate::db::find_sync_package(manager.handle(), target).is_err()
            {
                aur_candidates.push(target.to_string());
            }
        }
    }
    let aur_batch = if !aur_candidates.is_empty() {
        aur::query_aur(&aur_candidates)
    } else {
        HashMap::new()
    };

    for target in requested {
        // Silently repeated targets are shown and executed once; the first
        // spelling wins so the Requested box never lists duplicates.
        if out.items.iter().any(|item| item.name == strip_repo(target)) {
            continue;
        }
        if crate::sources::is_direct_syntax(target) {            return Err(format!(
                "Direct upstream source '{target}' cannot be combined with a system upgrade; it is already tracked by 'pin -Syu', or reinstall it separately with 'pin -S {target}'."
            ));
        }
        if let Some((head, tail)) = target.split_once('/') {
            if config.sources.contains_key(head) {
                return Err(format!(
                    "Upstream source '{target}' cannot be combined with a system upgrade; it is already tracked by 'pin -Syu', or reinstall it separately with 'pin -S {target}'."
                ));
            }
            if !crate::utils::is_safe_install_target(target) {
                return Err(format!("Refusing unsafe package target '{target}'."));
            }
            if head.eq_ignore_ascii_case("aur") {
                if !crate::utils::is_safe_pkg_name(tail) {
                    return Err(format!("Refusing unsafe AUR upgrade target '{tail}'."));
                }
                let item = aur_batch.get(tail).ok_or_else(|| {
                    format!("'{target}' was not found in the AUR.")
                })?;
                if held_by_name(tail) {
                    return Err(format!("'{tail}' is currently held back and cannot be pulled into this upgrade."));
                }
                if ignored.contains(tail) {
                    return Err(format!("'{tail}' is explicitly ignored; remove the matching --ignore/--ignoregroup entry to include it in this upgrade."));
                }
                let status = requested_status(installed_version(tail).as_deref(), &item.version);
                out.aur.push(tail.to_string());
                out.items.push(RequestedItem {
                    name: tail.to_string(),
                    manager: "aur".to_string(),
                    repo: Some("aur".to_string()),
                    installed_version: installed_version(tail),
                    candidate_version: item.version.clone(),
                    status,
                });
            } else if manager.repos().iter().any(|repo| repo == head) {
                let db = manager
                    .handle()
                    .syncdbs()
                    .into_iter()
                    .find(|db| db.name() == head)
                    .ok_or_else(|| {
                        format!("Repository '[{head}]' is configured but has no loaded database; refresh with 'pin -Sy'.")
                    })?;
                let pkg = db.pkg(tail).map_err(|_| {
                    format!("Package '{tail}' was not found in repository '[{head}]'.")
                })?;
                if held_by_name(tail) {
                    return Err(format!("'{tail}' is currently held back and cannot be pulled into this upgrade."));
                }
                if ignored.contains(tail) {
                    return Err(format!("'{tail}' is explicitly ignored; remove the matching --ignore/--ignoregroup entry to include it in this upgrade."));
                }
                if ResolverEngine::is_excluded(tail, head, &config.exclude) {
                    return Err(format!("'{tail}' is excluded from [{head}] by configuration; remove the exclusion to include it in this upgrade."));
                }
                check_requested_delay(tail, pkg.build_date(), config, now_epoch)?;
                let installed = installed_version(tail);
                let candidate = pkg.version().to_string();
                let status = requested_status(installed.as_deref(), &candidate);
                out.native.push(target.clone());
                out.items.push(RequestedItem {
                    name: tail.to_string(),
                    manager: "repo".to_string(),
                    repo: Some(head.to_string()),
                    installed_version: installed.clone(),
                    candidate_version: candidate.clone(),
                    status,
                });
            } else {
                return Err(format!(
                    "Repository '[{head}]' is not recognized. Configured repositories: {}",
                    manager.repos().join(", ")
                ));
            }
            continue;
        }

        if !crate::utils::is_safe_install_target(target) {
            return Err(format!("Refusing unsafe package target '{target}'."));
        }
        if let Some(alias) = crate::sources::owner(config, target) {
            return Err(format!(
                "'{target}' is owned by upstream source '{alias}'; it is already tracked by 'pin -Syu'. To force a reinstall, run 'pin -S {alias}/{target}'."
            ));
        }
        if ignored.contains(target) {
            return Err(format!("'{target}' is explicitly ignored; remove the matching --ignore/--ignoregroup entry to include it in this upgrade."));
        }
        // Repository pins are policy: a bare requested target resolves from
        // its pinned repository, exactly like a pinned update would. Without
        // a pin, plain repository priority applies.
        let pinned = ResolverEngine::match_pinned_package(target, &pins, &glob_pins);
        if let Some(pinned_repo) = pinned.as_deref() {
            if pinned_repo != "aur" {
                let db = manager
                    .handle()
                    .syncdbs()
                    .into_iter()
                    .find(|db| db.name() == pinned_repo)
                    .ok_or_else(|| {
                        format!("'{target}' is pinned to '[{pinned_repo}]', which is not a registered repository.")
                    })?;
                let pkg = db.pkg(target.as_str()).map_err(|_| {
                    format!("'{target}' is pinned to '[{pinned_repo}]', which does not carry it; adjust the pin or qualify another repository explicitly.")
                })?;
                if held_by_name(target) {
                    return Err(format!("'{target}' is currently held back and cannot be pulled into this upgrade."));
                }
                if ResolverEngine::is_excluded(target, pinned_repo, &config.exclude) {
                    return Err(format!("'{target}' is excluded from [{pinned_repo}] by configuration; remove the exclusion to include it in this upgrade."));
                }
                check_requested_delay(target, pkg.build_date(), config, now_epoch)?;
                let installed = installed_version(target);
                let candidate = pkg.version().to_string();
                let status = requested_status(installed.as_deref(), &candidate);
                out.native.push(format!("{pinned_repo}/{target}"));
                out.items.push(RequestedItem {
                    name: target.clone(),
                    manager: "repo".to_string(),
                    repo: Some(pinned_repo.to_string()),
                    installed_version: installed.clone(),
                    candidate_version: candidate.clone(),
                    status,
                });
                continue;
            }
            // Pinned to the AUR: skip the sync databases and use the AUR
            // lookup below, mirroring resolver policy for pinned packages.
        }
        // A pin to the AUR skips the sync databases (resolver policy);
        // otherwise fall back to the AUR only when no sync package matches.
        let mut handled_native = false;
        if pinned.as_deref() != Some("aur") {
            if let Ok(pkg) = crate::db::find_sync_package(manager.handle(), target) {
                let repo = pkg.db().map(|d| d.name().to_string()).unwrap_or_default();
                let real_name = pkg.name().to_string();
                if out.items.iter().any(|item| item.name == real_name) {
                    continue;
                }
                if ignored.contains(&real_name) {
                    return Err(format!("'{real_name}' is explicitly ignored; remove the matching --ignore/--ignoregroup entry to include it in this upgrade."));
                }
                let candidate = pkg.version().to_string();
                let builddate = pkg.build_date();
                if held_by_name(&real_name) {
                    return Err(format!("'{real_name}' is currently held back and cannot be pulled into this upgrade."));
                }
                if ResolverEngine::is_excluded(&real_name, &repo, &config.exclude) {
                    return Err(format!("'{real_name}' is excluded from [{repo}] by configuration; remove the exclusion to include it in this upgrade."));
                }
                check_requested_delay(&real_name, builddate, config, now_epoch)?;
                let installed = installed_version(&real_name);
                let status = requested_status(installed.as_deref(), &candidate);
                out.native.push(if repo.is_empty() { real_name.clone() } else { format!("{repo}/{real_name}") });
                out.items.push(RequestedItem {
                    name: real_name,
                    manager: "repo".to_string(),
                    repo: Some(repo),
                    installed_version: installed.clone(),
                    candidate_version: candidate.clone(),
                    status,
                });
                handled_native = true;
            }
        }
        if !handled_native {
            let item = aur_batch.get(target).ok_or_else(|| {
                format!("'{target}' was not found in the sync databases or the AUR.")
            })?;
            if held_by_name(target) {
                return Err(format!("'{target}' is currently held back and cannot be pulled into this upgrade."));
            }
            let status = requested_status(installed_version(target).as_deref(), &item.version);
            out.aur.push(target.clone());
            out.items.push(RequestedItem {
                name: target.clone(),
                manager: "aur".to_string(),
                repo: Some("aur".to_string()),
                installed_version: installed_version(target),
                candidate_version: item.version.clone(),
                status,
            });
        }
    }
    Ok(out)
}
