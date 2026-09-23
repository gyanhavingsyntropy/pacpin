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

pub fn cmd_check(config: &Config) {
    print_banner();

    // Spawn external integrations check concurrently with ALPM & AUR resolution
    let ext_config = config.clone();
    let ext_handle = thread::spawn(move || {
        let ext_providers = IntegrationsManager::get_active_providers(&ext_config);
        IntegrationsManager::check_updates_parallel(&ext_providers)
    });

    let manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };
    for warn in manager.verify_syncdbs() {
        eprintln!("{}", format!(":: Warning: {}", warn).yellow());
    }
    let resolver = ResolverEngine::new(&manager);

    let alpm = manager.handle();
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

    let res = resolver.resolve_all(config);
    let external_updates = ext_handle.join().unwrap_or_default();
    render_transaction_view(&res, &config.options.helper, &external_updates);

    if config.features.smart_orphans {
        let orphans = manager.get_orphans(&[]);
        if !orphans.is_empty() {
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
}
