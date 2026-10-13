// pacpin - Declarative Package Resolver & Upgrade Engine for Arch Linux / CachyOS
// Copyright (C) 2026 Gyan <330976822+gyanhavingsyntropy@users.noreply.github.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal, Write};
use std::process::Command;

use alpm::PackageReason;
use colored::Colorize;

use crate::config::{mutate_config, Config, PackageManifest};
use crate::db::AlpmManager;
use crate::pm::{self, InstallOptions};
use crate::resolver::ResolverEngine;
use crate::ui::print_banner;
use crate::utils::{is_safe_install_target, is_safe_pkg_name};

#[derive(Default)]
struct Snapshot {
    installed: BTreeSet<String>,
    repos: BTreeMap<String, BTreeSet<String>>,
    flatpak_user: BTreeMap<String, String>,
    flatpak_system: BTreeMap<String, String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RebuildPlan {
    install: Vec<String>,
    enforce_repo: Vec<String>,
    flatpak_user: Vec<String>,
    flatpak_system: Vec<String>,
}

impl RebuildPlan {
    fn is_empty(&self) -> bool {
        self.install.is_empty()
            && self.enforce_repo.is_empty()
            && self.flatpak_user.is_empty()
            && self.flatpak_system.is_empty()
    }

    fn count(&self) -> usize {
        self.install.len()
            + self.enforce_repo.len()
            + self.flatpak_user.len()
            + self.flatpak_system.len()
    }
}

#[derive(Default)]
struct RebuildOptions {
    dry_run: bool,
    noconfirm: bool,
    enforce_repos: bool,
    no_cache: bool,
}

fn parse_rebuild_options(args: &[String]) -> Result<RebuildOptions, String> {
    let mut options = RebuildOptions::default();
    for arg in args {
        match arg.as_str() {
            "-n" | "--dry-run" => options.dry_run = true,
            "--noconfirm" => options.noconfirm = true,
            "--enforce-repos" => options.enforce_repos = true,
            "--no-cache" | "--clean-cache" => options.no_cache = true,
            other => return Err(format!("Unknown rebuild option '{}'.", other)),
        }
    }
    Ok(options)
}

fn split_flatpak_target(value: &str) -> Result<(Option<&str>, &str), String> {
    if let Some((remote, app)) = value.split_once('/') {
        if !is_safe_pkg_name(remote) || !is_safe_pkg_name(app) {
            return Err(format!("Invalid Flatpak target '{}'.", value));
        }
        Ok((Some(remote), app))
    } else if is_safe_pkg_name(value) {
        Ok((None, value))
    } else {
        Err(format!("Invalid Flatpak target '{}'.", value))
    }
}

fn plan_flatpaks(
    desired: &[String],
    installed: &BTreeMap<String, String>,
    scope: &str,
) -> Result<Vec<String>, String> {
    let mut seen = BTreeSet::new();
    let mut missing = Vec::new();
    for target in desired {
        let (remote, app) = split_flatpak_target(target)?;
        if !seen.insert(app) {
            return Err(format!(
                "Flatpak '{}' appears more than once in {}.",
                app, scope
            ));
        }
        if let Some(origin) = installed.get(app) {
            if let Some(remote) = remote {
                if origin != remote {
                    return Err(format!(
                        "Flatpak '{}' is installed from '{}' in {}, but the manifest requests '{}'. Change its remote manually before rebuilding.",
                        app, origin, scope, remote
                    ));
                }
            }
        } else {
            missing.push(target.clone());
        }
    }
    Ok(missing)
}

fn make_plan(
    config: &Config,
    snapshot: &Snapshot,
    enforce_repos: bool,
) -> Result<RebuildPlan, String> {
    let manifest = &config.packages;
    if manifest.is_empty() {
        return Err(
            "No packages are declared in [packages]. Add entries or run 'pin capture'.".into(),
        );
    }

    let mut plan = RebuildPlan::default();
    let mut seen = BTreeSet::new();
    let glob_pins = ResolverEngine::compile_glob_pins(&config.pins);

    for target in &manifest.sources {
        if !is_safe_install_target(target) { return Err(format!("Invalid source target '{target}'")); }
        let (alias, name) = target.split_once('/').ok_or("Source manifest entries need alias/package")?;
        if !config.sources.get(alias).is_some_and(|s| s.packages.contains_key(name)) { return Err(format!("No source recipe for '{target}'")); }
        if !seen.insert(name.to_string()) { return Err(format!("Duplicate package '{name}'")); }
        if !snapshot.installed.contains(name) { plan.install.push(target.clone()); }
        else if enforce_repos && crate::sources::owner(config, name) != Some(alias) { plan.enforce_repo.push(target.clone()); }
    }

    for value in &manifest.pacman {
        if !is_safe_install_target(value) {
            return Err(format!("Invalid pacman package target '{}'.", value));
        }
        let (requested_repo, name) = match value.split_once('/') {
            Some((repo, name)) => (Some(repo.to_string()), name),
            None => (None, value.as_str()),
        };
        if !seen.insert(name.to_string()) {
            return Err(format!(
                "Package '{}' appears more than once in [packages].",
                name
            ));
        }

        let pinned_repo = if requested_repo.is_none() && config.features.pinning {
            ResolverEngine::find_matching_pin_rule(name, &config.pins, &glob_pins)
                .map(|(_, repo, _)| repo)
        } else {
            None
        };
        let repo = requested_repo.or(pinned_repo);
        let target = if let Some(repo) = repo.as_deref() {
            if repo == "aur" {
                return Err(format!(
                    "Package '{}' is pinned to AUR. Put it in packages.aur instead.",
                    name
                ));
            }
            let Some(packages) = snapshot.repos.get(repo) else {
                return Err(format!(
                    "Repository '{}' for '{}' is not configured on this system.",
                    repo, name
                ));
            };
            if !packages.contains(name) {
                return Err(format!(
                    "Package '{}' is not present in repository '{}'.",
                    name, repo
                ));
            }
            format!("{}/{}", repo, name)
        } else {
            if !snapshot
                .repos
                .values()
                .any(|packages| packages.contains(name))
            {
                return Err(format!(
                    "Package '{}' is not present in any configured sync repository.",
                    name
                ));
            }
            name.to_string()
        };

        if !snapshot.installed.contains(name) {
            plan.install.push(target);
        } else if enforce_repos && repo.is_some() {
            plan.enforce_repo.push(target);
        }
    }

    for name in &manifest.aur {
        if !is_safe_pkg_name(name) {
            return Err(format!("Invalid AUR package name '{}'.", name));
        }
        if !seen.insert(name.clone()) {
            return Err(format!(
                "Package '{}' appears more than once in [packages].",
                name
            ));
        }
        if !snapshot.installed.contains(name) {
            plan.install.push(format!("aur/{}", name));
        }
    }

    plan.flatpak_user = plan_flatpaks(
        &manifest.flatpak_user,
        &snapshot.flatpak_user,
        "flatpak_user",
    )?;
    plan.flatpak_system = plan_flatpaks(
        &manifest.flatpak_system,
        &snapshot.flatpak_system,
        "flatpak_system",
    )?;
    Ok(plan)
}

fn parse_flatpak_list(stdout: &str) -> Result<BTreeMap<String, String>, String> {
    let mut installed = BTreeMap::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let (app, origin) = line
            .split_once('\t')
            .ok_or_else(|| format!("Unexpected Flatpak list output: '{}'", line))?;
        if !is_safe_pkg_name(app) || (!origin.is_empty() && !is_safe_pkg_name(origin)) {
            return Err(format!(
                "Unexpected Flatpak app or remote in list: '{}'",
                line
            ));
        }
        if installed
            .insert(app.to_string(), origin.to_string())
            .is_some()
        {
            return Err(format!(
                "Flatpak '{}' is installed more than once in the same scope.",
                app
            ));
        }
    }
    Ok(installed)
}

fn list_flatpaks(scope: &str) -> Result<BTreeMap<String, String>, String> {
    let output = Command::new("flatpak")
        .args(["list", scope, "--app", "--columns=application,origin"])
        .output()
        .map_err(|e| format!("Cannot list {} Flatpaks: {}", scope, e))?;
    if !output.status.success() {
        return Err(format!(
            "Cannot list {} Flatpaks (exit {}).",
            scope,
            output.status.code().unwrap_or(1)
        ));
    }
    parse_flatpak_list(&String::from_utf8_lossy(&output.stdout))
}

fn collect_snapshot(manager: &AlpmManager, manifest: &PackageManifest) -> Result<Snapshot, String> {
    let mut snapshot = Snapshot::default();
    for pkg in manager.handle().localdb().pkgs() {
        snapshot.installed.insert(pkg.name().to_string());
    }

    let wanted: BTreeSet<&str> = manifest
        .pacman
        .iter()
        .map(|target| target.rsplit('/').next().unwrap_or(target))
        .collect();
    for db in manager.handle().syncdbs() {
        let mut found = BTreeSet::new();
        for name in &wanted {
            if db.pkg(*name).is_ok() {
                found.insert((*name).to_string());
            }
        }
        snapshot.repos.insert(db.name().to_string(), found);
    }

    let wants_flatpak = !manifest.flatpak_user.is_empty() || !manifest.flatpak_system.is_empty();
    if wants_flatpak {
        if crate::is_command_available("flatpak") {
            snapshot.flatpak_user = list_flatpaks("--user")?;
            snapshot.flatpak_system = list_flatpaks("--system")?;
        } else if !manifest
            .pacman
            .iter()
            .any(|target| target == "flatpak" || target.ends_with("/flatpak"))
        {
            return Err("Flatpak is not installed. Add 'flatpak' to packages.pacman first.".into());
        }
    }
    Ok(snapshot)
}

fn print_plan(plan: &RebuildPlan) {
    println!("\n{}", ":: Package rebuild plan".cyan().bold());
    for (title, targets) in [
        ("Install with pacman/AUR", &plan.install),
        ("Reinstall from declared repository", &plan.enforce_repo),
        ("Install user Flatpaks", &plan.flatpak_user),
        ("Install system Flatpaks", &plan.flatpak_system),
    ] {
        if !targets.is_empty() {
            println!("  {}:", title);
            for target in targets {
                println!("    + {}", target);
            }
        }
    }
    if plan.is_empty() {
        println!("  All declared packages are already present.");
    }
    println!("  Existing packages absent from the manifest are left installed.");
}

fn install_flatpaks(targets: &[String], scope: &str) -> Result<(), String> {
    for target in targets {
        let (remote, app) = split_flatpak_target(target)?;
        let mut command = Command::new("flatpak");
        command.args(["install", scope, "--app", "--assumeyes", "--noninteractive"]);
        if let Some(remote) = remote {
            command.arg(remote);
        }
        let status = command.arg(app).status().map_err(|e| {
            format!(
                "Failed to start Flatpak installation for '{}': {}",
                target, e
            )
        })?;
        if !status.success() {
            return Err(format!(
                "Flatpak installation for '{}' failed with exit {}. Re-run 'pin rebuild' after fixing it.",
                target,
                status.code().unwrap_or(1)
            ));
        }
    }
    Ok(())
}

fn install_rebuild_targets(config: &mut Config, targets: &[String], options: &InstallOptions) -> Result<(), String> {
    let (sources, native): (Vec<_>, Vec<_>) = targets.iter().cloned().partition(|target| {
        target.split_once('/').is_some_and(|(alias, _)| config.sources.contains_key(alias))
            || crate::sources::owner(config, target).is_some()
    });
    if !native.is_empty() { pm::install(config, &native, options)?; }
    if !sources.is_empty() { pm::install(config, &sources, options)?; }
    Ok(())
}

pub fn cmd_rebuild(mut config: Config, args: &[String]) -> Result<(), String> {
    let options = parse_rebuild_options(args)?;
    print_banner();
    if config.packages.is_empty() {
        return Err(
            "No packages are declared in [packages]. Add entries or run 'pin capture'.".into(),
        );
    }

    let manager = AlpmManager::with_repo_order(&config.repo_order)
        .map_err(|e| format!("Cannot load pacman databases: {}", e))?;
    if !config.packages.pacman.is_empty() {
        let warnings = manager.verify_syncdbs();
        if !warnings.is_empty() {
            return Err(format!(
                "Sync databases need attention before rebuilding:\n{}",
                warnings.join("\n")
            ));
        }
    }
    let snapshot = collect_snapshot(&manager, &config.packages)?;
    let plan = make_plan(&config, &snapshot, options.enforce_repos)?;
    print_plan(&plan);
    if (!plan.flatpak_user.is_empty() || !plan.flatpak_system.is_empty())
        && (crate::scratch::policy() != crate::scratch::Policy::Auto || options.no_cache)
    {
        return Err("Manifest Flatpak installation does not support scratch/cache overrides. Apply the native/AUR/Sources manifest separately or omit these flags; no packages have been changed.".into());
    }
    if let Some(description) = crate::scratch::describe_plan(
        plan.install.iter().chain(&plan.enforce_repo).any(|target| crate::sources::owner(&config, target).is_some() || target.split_once('/').is_some_and(|(alias, _)| config.sources.contains_key(alias))),
        plan.install.iter().chain(&plan.enforce_repo).any(|target| target.starts_with("aur/")),
    ) { println!(":: {description}"); }
    if options.dry_run || plan.is_empty() {
        return Ok(());
    }

    if !options.noconfirm {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err("Non-interactive rebuild requires --noconfirm or --dry-run.".into());
        }
        print!(":: Apply {} package action(s)? [y/N] ", plan.count());
        io::stdout().flush().map_err(|e| e.to_string())?;
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .map_err(|e| e.to_string())?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            return Err("Rebuild cancelled.".into());
        }
    }

    drop(manager);
    if !plan.install.is_empty() {
        install_rebuild_targets(
            &mut config,
            &plan.install,
            &InstallOptions {
                noconfirm: true,
                no_cache: options.no_cache,
                ..InstallOptions::default()
            },
        )?;
    }
    if !plan.enforce_repo.is_empty() {
        install_rebuild_targets(
            &mut config,
            &plan.enforce_repo,
            &InstallOptions {
                needed: false,
                noconfirm: true,
                no_cache: options.no_cache,
                ..InstallOptions::default()
            },
        )?;
    }
    install_flatpaks(&plan.flatpak_user, "--user")?;
    install_flatpaks(&plan.flatpak_system, "--system")?;
    println!("{}", "✔ Declared package list applied.".green());
    Ok(())
}

pub fn cmd_capture(config: Config, args: &[String]) -> Result<(), String> {
    let mut dry_run = false;
    let mut force = false;
    for arg in args {
        match arg.as_str() {
            "-n" | "--dry-run" => dry_run = true,
            "-f" | "--force" => force = true,
            other => return Err(format!("Unknown capture option '{}'.", other)),
        }
    }
    if !config.packages.is_empty() && !force && !dry_run {
        return Err("[packages] already contains entries. Use 'pin capture --dry-run' to preview or '--force' to replace the list.".into());
    }

    let manager = AlpmManager::with_repo_order(&config.repo_order)
        .map_err(|e| format!("Cannot load pacman databases: {}", e))?;
    let warnings = manager.verify_syncdbs();
    if !warnings.is_empty() {
        return Err(format!(
            "Cannot classify package sources with invalid sync databases:\n{}",
            warnings.join("\n")
        ));
    }
    let glob_pins = ResolverEngine::compile_glob_pins(&config.pins);
    let mut captured = PackageManifest::default();
    let mut ambiguous = 0usize;

    for pkg in manager.handle().localdb().pkgs() {
        if pkg.reason() != PackageReason::Explicit {
            continue;
        }
        let name = pkg.name();
        if let Some(alias) = crate::sources::owner(&config, name) {
            captured.sources.push(format!("{alias}/{name}"));
            continue;
        }
        let matches: Vec<String> = manager
            .handle()
            .syncdbs()
            .into_iter()
            .filter(|db| db.pkg(name).is_ok())
            .map(|db| db.name().to_string())
            .collect();
        let pin = if config.features.pinning {
            ResolverEngine::find_matching_pin_rule(name, &config.pins, &glob_pins)
                .map(|(_, repo, _)| repo)
        } else {
            None
        };
        if pin.as_deref() == Some("aur") || matches.is_empty() {
            captured.aur.push(name.to_string());
        } else if let Some(repo) = pin.filter(|repo| matches.contains(repo)) {
            captured.pacman.push(format!("{}/{}", repo, name));
        } else if matches.len() == 1 {
            captured.pacman.push(format!("{}/{}", matches[0], name));
        } else {
            captured.pacman.push(name.to_string());
            ambiguous += 1;
        }
    }

    if crate::is_command_available("flatpak") {
        for (app, remote) in list_flatpaks("--user")? {
            captured.flatpak_user.push(if remote.is_empty() {
                app
            } else {
                format!("{}/{}", remote, app)
            });
        }
        for (app, remote) in list_flatpaks("--system")? {
            captured.flatpak_system.push(if remote.is_empty() {
                app
            } else {
                format!("{}/{}", remote, app)
            });
        }
    }

    captured.pacman.sort();
    captured.sources.sort();
    captured.aur.sort();
    captured.flatpak_user.sort();
    captured.flatpak_system.sort();
    print_banner();
    println!(
        ":: Captured {} pacman, {} foreign, {} user Flatpak, {} system Flatpak, and {} source packages.",
        captured.pacman.len(),
        captured.aur.len(),
        captured.flatpak_user.len(),
        captured.flatpak_system.len(),
        captured.sources.len()
    );
    if ambiguous > 0 {
        println!(":: {} package(s) exist in multiple repos and were left unqualified. Choose a repo in [packages] if source matters.", ambiguous);
    }
    if !captured.aur.is_empty() {
        println!(":: Review packages.aur: pacman cannot distinguish AUR packages from local package files.");
    }

    if dry_run {
        let toml = toml::to_string_pretty(&captured).map_err(|e| e.to_string())?;
        println!("\n[packages]\n{}", toml);
    } else {
        mutate_config(|current| {
            if !current.packages.is_empty() && !force {
                return Err("[packages] changed during capture. Preview it or use --force to replace it.".into());
            }
            current.packages = captured;
            Ok(())
        })?;
        println!(
            "✔ Package list saved to {}",
            crate::config::get_config_path().display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_cache_flag_composes_with_preview() {
        let options = parse_rebuild_options(&["--dry-run".into(), "--no-cache".into()]).unwrap();
        assert!(options.dry_run && options.no_cache);
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            installed: BTreeSet::from(["git".into(), "vim".into()]),
            repos: BTreeMap::from([
                ("core".into(), BTreeSet::from(["linux".into()])),
                ("extra".into(), BTreeSet::from(["git".into(), "vim".into()])),
            ]),
            flatpak_user: BTreeMap::from([("org.gnome.Calculator".into(), "flathub".into())]),
            ..Snapshot::default()
        }
    }

    #[test]
    fn plans_only_missing_without_touching_existing_packages() {
        let mut config = Config::default();
        config.packages.pacman = vec!["core/linux".into(), "vim".into()];
        config.packages.aur = vec!["paru-bin".into()];
        config.packages.flatpak_user = vec!["flathub/org.gnome.Calculator".into()];
        let plan = make_plan(&config, &snapshot(), false).unwrap();
        assert_eq!(plan.install, ["core/linux", "aur/paru-bin"]);
        assert!(plan.flatpak_user.is_empty());
    }

    #[test]
    fn source_manifest_is_additive_and_rejects_cross_manager_duplicates() {
        let mut config: Config = toml::from_str("[sources.company]\nkind='local'\nurl='/tmp/catalog.json'\n[sources.company.packages.git]\nasset='git'\nformat='binary'\n[sources.company.packages.git.binaries]\ngit='artifact'\n[packages]\nsources=['company/git']\n").unwrap();
        assert!(make_plan(&config, &snapshot(), false).unwrap().is_empty());
        assert_eq!(make_plan(&config, &snapshot(), true).unwrap().enforce_repo, ["company/git"]);
        config.packages.pacman.push("git".into());
        assert!(make_plan(&config, &snapshot(), false).is_err());
        config.packages.pacman.clear();
        let mut snapshot = snapshot(); snapshot.installed.clear();
        assert_eq!(make_plan(&config, &snapshot, false).unwrap().install, ["company/git"]);
    }

    #[test]
    fn repo_enforcement_is_explicit_and_idempotent_by_default() {
        let mut config = Config::default();
        config.packages.pacman = vec!["extra/git".into()];
        assert!(make_plan(&config, &snapshot(), false).unwrap().is_empty());
        assert_eq!(
            make_plan(&config, &snapshot(), true).unwrap().enforce_repo,
            ["extra/git"]
        );
    }

    #[test]
    fn rejects_unsafe_duplicate_and_unknown_targets_before_execution() {
        let mut config = Config::default();
        config.packages.pacman = vec!["extra/../bad".into()];
        assert!(make_plan(&config, &snapshot(), false).is_err());
        config.packages.pacman = vec!["unknown/git".into()];
        assert!(make_plan(&config, &snapshot(), false).is_err());
        config.packages.pacman = vec!["git".into()];
        config.packages.aur = vec!["git".into()];
        assert!(make_plan(&config, &snapshot(), false).is_err());
    }

    #[test]
    fn pin_source_and_flatpak_scope_are_respected() {
        let mut config = Config::default();
        config.pins.insert("linux".into(), "core".into());
        config.packages.pacman = vec!["linux".into()];
        config.packages.flatpak_system = vec!["flathub/org.gnome.Calculator".into()];
        let plan = make_plan(&config, &snapshot(), false).unwrap();
        assert_eq!(plan.install, ["core/linux"]);
        assert_eq!(plan.flatpak_system, ["flathub/org.gnome.Calculator"]);
        assert!(plan.flatpak_user.is_empty());
    }

    #[test]
    fn malformed_flatpak_rows_and_origin_mismatch_fail_closed() {
        assert!(parse_flatpak_list("bad-row").is_err());
        assert!(split_flatpak_target("flathub/../bad").is_err());
        let wanted = vec!["other/org.gnome.Calculator".to_string()];
        assert!(plan_flatpaks(&wanted, &snapshot().flatpak_user, "user").is_err());
    }
}
