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

use std::process::exit;

use colored::Colorize;

use crate::commands::check_pacman_lock;
use crate::db::{AlpmManager, OrphanPackage};
use crate::orphans::OrphanManager;
use crate::ui::{self, print_banner};

pub fn cmd_orphans(config: &crate::config::Config, clean: bool, noconfirm: bool) {
    print_banner();
    let manager = match AlpmManager::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    let orphans = manager.get_orphans(&[]);
    if orphans.is_empty() {
        if clean && !OrphanManager::finish_review(&[]) { exit(1); }
        println!(
            "\n{}",
            "✔ No orphaned packages found on your system.".green()
        );
        return;
    }

    ui::render_orphans_summary(&orphans);

    if clean {
        let selected = if !noconfirm {
            ui::prompt_orphan_selection(&orphans, false)
        } else {
            orphans
                .iter()
                .filter(|o| o.is_pure())
                .map(|o| o.name.clone())
                .collect()
        };
        if let Err(error) = OrphanManager::record_selection(&selected) {
            eprintln!(":: {error}; cleanup aborted before removal.");
            exit(1);
        }
        let result = if !selected.is_empty() {
            check_pacman_lock();
            OrphanManager::execute_removal(&selected, &config.options.privilege_tool)
        } else { Ok(Vec::new()) };
        let removal_ok = result.is_ok();
        let removed = result.unwrap_or_else(|error| { eprintln!(":: Warning: {error}"); Vec::new() });
        let remaining: Vec<OrphanPackage> = orphans
            .iter()
            .filter(|o| !removed.contains(&o.name))
            .cloned()
            .collect();
        OrphanManager::save_known(&remaining);
        let recorded = OrphanManager::finish_review(&remaining);
        if !removal_ok || !recorded { exit(1); }
    } else {
        OrphanManager::save_known(&orphans);
        println!(
            "\n{}",
            "Use 'pacpin orphans -c' (or 'pin autoremove') to remove selected unneeded dependencies.".dimmed()
        );
    }
}
