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

use std::collections::HashMap;
use std::process::exit;
use std::thread;

use colored::Colorize;

use crate::config::Config;
use crate::db::AlpmManager;
use crate::integrations::IntegrationsManager;
use crate::resolver::ResolverEngine;
use crate::ui::{self, print_banner, render_transaction_view};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckScope {
    All,
    Repo,
    Aur,
    Sources,
    External,
}

impl CheckScope {
    pub fn resolve(
        runtime_scope: crate::runtime::UpgradeScope,
        repo_flag: bool,
        aur_flag: bool,
    ) -> Result<Self, String> {
        if repo_flag && (aur_flag || runtime_scope != crate::runtime::UpgradeScope::All) {
            return Err("--repo cannot be combined with other scope filters".into());
        }
        if aur_flag
            && !matches!(
                runtime_scope,
                crate::runtime::UpgradeScope::All | crate::runtime::UpgradeScope::Aur
            )
        {
            return Err("--aur cannot be combined with --sources-only or --external-only".into());
        }
        Ok(match runtime_scope {
            crate::runtime::UpgradeScope::Sources => Self::Sources,
            crate::runtime::UpgradeScope::External => Self::External,
            crate::runtime::UpgradeScope::Aur => Self::Aur,
            crate::runtime::UpgradeScope::All if repo_flag => Self::Repo,
            crate::runtime::UpgradeScope::All if aur_flag => Self::Aur,
            crate::runtime::UpgradeScope::All => Self::All,
        })
    }

    pub fn needs_alpm(self) -> bool {
        matches!(self, Self::All | Self::Repo | Self::Aur)
    }

    pub fn needs_integrations(self) -> bool {
        matches!(self, Self::All | Self::Sources | Self::External)
    }

    pub fn includes_runner(self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Sources => name == "Sources",
            Self::External => name != "Sources",
            Self::Repo | Self::Aur => false,
        }
    }
}

pub fn cmd_check(config: &Config, args: &[String]) {
    let repo_flag = args.iter().any(|a| a == "--repo" || a == "-r");
    let aur_flag = args.iter().any(|a| a == "--aur" || a == "-a");
    let no_flatpak = args.iter().any(|a| a == "--no-flatpak");
    let quiet = args.iter().any(|a| a == "-q" || a == "--quiet")
        || args
            .iter()
            .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('q'));
    let keep_going = crate::runtime::has_control(args, "--keep-going");

    let runtime_scope = match crate::runtime::UpgradeScope::parse(args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: {e}");
            exit(2);
        }
    };

    let scope = match CheckScope::resolve(runtime_scope, repo_flag, aur_flag) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: {e}");
            exit(2);
        }
    };

    if !quiet {
        print_banner();
    }

    // Spawn external integrations check concurrently with ALPM & AUR resolution unless disabled
    let (ext_handle, ext_empty) = if scope.needs_integrations() {
        let ext_config = config.clone();
        let mut ext_providers = IntegrationsManager::get_active_providers(&ext_config);
        if no_flatpak {
            ext_providers.retain(|p| p.name() != "Flatpak");
        }
        ext_providers.retain(|p| scope.includes_runner(p.name()));
        let empty = ext_providers.is_empty();
        let handle = Some(thread::spawn(move || {
            IntegrationsManager::check_updates_parallel(&ext_providers)
        }));
        (handle, empty)
    } else {
        (None, false)
    };

    let mut manager = if scope.needs_alpm() {
        match AlpmManager::with_repo_order(&config.repo_order) {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
                if let Some(handle) = ext_handle {
                    let _ = handle.join();
                }
                exit(1);
            }
        }
    } else {
        None
    };

    if !quiet {
        if let Some(ref mgr) = manager {
            for warn in mgr.verify_syncdbs() {
                eprintln!("{}", format!(":: Warning: {}", warn).yellow());
            }

            let alpm = mgr.handle();
            let local_pkgs = alpm.localdb().pkgs();
            let glob_pins = ResolverEngine::compile_glob_pins(&config.pins);
            let syncdb_map: HashMap<&str, &alpm::Db> =
                alpm.syncdbs().into_iter().map(|d| (d.name(), d)).collect();

            let mut custom_pkgs = Vec::new();
            for p in local_pkgs {
                if let Some(target_repo) =
                    ResolverEngine::match_pinned_package(p.name(), &config.pins, &glob_pins)
                {
                    let mut cand_ver = "pinned".to_string();
                    if let Some(&db) = syncdb_map.get(target_repo.as_str()) {
                        if let Ok(cand) = db.pkg(p.name()) {
                            cand_ver = cand.version().to_string();
                        }
                    }
                    custom_pkgs.push((p.name().to_string(), target_repo, cand_ver));
                }
            }
            custom_pkgs.sort_by(|a, b| a.0.cmp(&b.0));

            if !custom_pkgs.is_empty() {
                println!(
                    "\n{}",
                    format!("Active Custom Pins ({} packages):", custom_pkgs.len()).bold()
                );
                for (name, repo, cand_ver) in custom_pkgs {
                    println!(
                        "  • {:<28} ➔  [{}] {:<18} {}",
                        name.cyan(),
                        repo.green(),
                        cand_ver.green(),
                        "(Shielded)".dimmed()
                    );
                }
            }

            if config.features.vendor_stickiness {
                println!(
                    "\n{}",
                    "ℹ Vendor Stickiness: ACTIVE (packages stay bound to originating repository unless pinned)".blue()
                );
            }
        }

        let check_scope = match scope {
            CheckScope::Repo => "official repositories",
            CheckScope::Aur => "AUR",
            CheckScope::Sources => "configured release sources",
            CheckScope::External if no_flatpak => "external managers (excluding Flatpak)",
            CheckScope::External => "external managers",
            CheckScope::All if no_flatpak => {
                "repositories, AUR, configured release sources, and external managers (excluding Flatpak)"
            }
            CheckScope::All => {
                "repositories, AUR, configured release sources, and external managers"
            }
        };
        println!(
            "\n{}",
            format!(":: Checking updates across {}...", check_scope).cyan()
        );
    }

    let res = if let Some(ref mgr) = manager {
        let resolver = ResolverEngine::new(mgr);
        let skip_aur = scope == CheckScope::Repo;
        let mut r = if quiet {
            resolver.resolve_with_options(config, skip_aur)
        } else {
            crate::progress::operation(
                crate::progress::Manager::Checks,
                "Checking package updates",
                || resolver.resolve_with_options(config, skip_aur),
            )
        };

        if scope == CheckScope::Aur {
            r.updates
                .retain(|u| u.candidate.as_ref().is_some_and(|c| c.is_aur));
            r.held_packages
                .retain(|u| u.candidate.as_ref().is_some_and(|c| c.is_aur));
            r.unresolved_pins.clear();
        }

        r
    } else {
        crate::resolver::ResolveResult {
            check_errors: Vec::new(),
            installed_count: 0,
            repos: Vec::new(),
            packages: HashMap::new(),
            updates: Vec::new(),
            held_packages: Vec::new(),
            unresolved_pins: Vec::new(),
        }
    };

    let mut incomplete = false;

    if !res.check_errors.is_empty() {
        if keep_going {
            incomplete = true;
            eprintln!(
                ":: Warning: Update check incomplete: {}",
                crate::discovery::describe(&res.check_errors)
            );
        } else {
            eprintln!(
                "Error: Update check incomplete: {}",
                crate::discovery::describe(&res.check_errors)
            );
            if let Some(handle) = ext_handle {
                let _ = handle.join();
            }
            exit(1);
        }
    }

    // Pre-calculate orphans immediately on CPU while ext_handle is running in the background
    let orphans = if config.features.smart_orphans && !quiet {
        if let Some(ref mut mgr) = manager {
            mgr.get_orphans(&[])
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let external_updates = if let Some(h) = ext_handle {
        let join = || {
            h.join()
                .map_err(|_| "External update worker failed".to_string())
        };
        let report = if quiet {
            join()
        } else {
            crate::progress::operation(
                crate::progress::Manager::Checks,
                "Checking external updates",
                join,
            )
        };
        match report {
            Ok(rep) => {
                if keep_going {
                    let (updates, failures) = rep.into_parts();
                    if !failures.is_empty() {
                        incomplete = true;
                        eprintln!(
                            ":: Warning: External check incomplete: {}",
                            crate::discovery::describe(&failures)
                        );
                    }
                    updates
                } else {
                    match rep.require_complete() {
                        Ok(updates) => updates,
                        Err(e) => {
                            eprintln!("Error: {e}");
                            exit(1);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Error: {e}");
                exit(1);
            }
        }
    } else {
        Vec::new()
    };

    if quiet {
        for p in &res.updates {
            println!("{}", p.name);
        }
        for ext in &external_updates {
            println!("{}", ext.name);
        }
    } else {
        if let Some(ref mut mgr) = manager {
            let targets: Vec<String> = res
                .updates
                .iter()
                .filter_map(|p| {
                    p.candidate
                        .as_ref()
                        .filter(|c| !c.is_aur)
                        .map(|c| format!("{}/{}", c.repo, p.name))
                })
                .collect();
            let names = res.updates.iter().map(|p| p.name.clone()).collect();
            if targets.is_empty() {
                ui::render_transaction_plan(
                    &res,
                    &config.options.helper,
                    &external_updates,
                    None,
                );
            } else {
                match crate::preview::prepare_native_preview(mgr, &targets) {
                    Ok(preview) => {
                        ui::render_transaction_plan(
                            &res,
                            &config.options.helper,
                            &external_updates,
                            Some(ui::PlanDetails {
                                preview: &preview,
                                explicit_names: &names,
                                requested: &[],
                                cache: None,
                            }),
                        );
                    }
                    Err(error) => {
                        eprintln!(":: Native dependency preview unavailable: {error}. Totals below cover listed updates only.");
                        render_transaction_view(&res, &config.options.helper, &external_updates);
                    }
                }
            }
        } else if ext_empty {
            if scope == CheckScope::External {
                println!(
                    "\n{}",
                    ":: No active external managers configured or enabled.".dimmed()
                );
            } else if scope == CheckScope::Sources {
                println!(
                    "\n{}",
                    ":: No release sources configured or enabled.".dimmed()
                );
            }
        } else {
            render_transaction_view(&res, &config.options.helper, &external_updates);
        }

        if config.features.smart_orphans && !orphans.is_empty() {
            let tot_size: i64 = orphans.iter().map(|o| o.isize).sum();
            println!(
                "\n{} {}",
                format!(
                    "🧹 {} orphaned package(s) detected ({})",
                    orphans.len(),
                    ui::format_size(tot_size)
                )
                .yellow()
                .bold(),
                "— run 'pacpin orphans -c' to remove unneeded dependencies".dimmed()
            );
        }
    }

    if keep_going && incomplete {
        exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::UpgradeScope;

    #[test]
    fn check_scope_resolution_and_conflicts() {
        assert_eq!(
            CheckScope::resolve(UpgradeScope::All, false, false).unwrap(),
            CheckScope::All
        );
        assert_eq!(
            CheckScope::resolve(UpgradeScope::Sources, false, false).unwrap(),
            CheckScope::Sources
        );
        assert_eq!(
            CheckScope::resolve(UpgradeScope::Aur, false, false).unwrap(),
            CheckScope::Aur
        );
        assert_eq!(
            CheckScope::resolve(UpgradeScope::External, false, false).unwrap(),
            CheckScope::External
        );
        assert_eq!(
            CheckScope::resolve(UpgradeScope::All, true, false).unwrap(),
            CheckScope::Repo
        );
        assert_eq!(
            CheckScope::resolve(UpgradeScope::All, false, true).unwrap(),
            CheckScope::Aur
        );
        assert_eq!(
            CheckScope::resolve(UpgradeScope::Aur, false, true).unwrap(),
            CheckScope::Aur
        );

        // Conflict errors
        assert!(CheckScope::resolve(UpgradeScope::Sources, true, false).is_err());
        assert!(CheckScope::resolve(UpgradeScope::Aur, true, false).is_err());
        assert!(CheckScope::resolve(UpgradeScope::External, true, false).is_err());
        assert!(CheckScope::resolve(UpgradeScope::All, true, true).is_err());
        assert!(CheckScope::resolve(UpgradeScope::Sources, false, true).is_err());
        assert!(CheckScope::resolve(UpgradeScope::External, false, true).is_err());
    }

    #[test]
    fn check_scope_capabilities() {
        assert!(CheckScope::All.needs_alpm());
        assert!(CheckScope::Repo.needs_alpm());
        assert!(CheckScope::Aur.needs_alpm());
        assert!(!CheckScope::Sources.needs_alpm());
        assert!(!CheckScope::External.needs_alpm());

        assert!(CheckScope::All.needs_integrations());
        assert!(CheckScope::Sources.needs_integrations());
        assert!(CheckScope::External.needs_integrations());
        assert!(!CheckScope::Repo.needs_integrations());
        assert!(!CheckScope::Aur.needs_integrations());

        assert!(CheckScope::Sources.includes_runner("Sources"));
        assert!(!CheckScope::Sources.includes_runner("Flatpak"));
        assert!(!CheckScope::External.includes_runner("Sources"));
        assert!(CheckScope::External.includes_runner("Flatpak"));
        assert!(CheckScope::All.includes_runner("Sources"));
        assert!(CheckScope::All.includes_runner("Flatpak"));
    }
}
