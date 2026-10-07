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

use crate::config::mutate_config;
use crate::db::AlpmManager;
use crate::ui::print_banner;

pub fn cmd_delay(pkg: &str, days: u32) {
    print_banner();

    if !crate::utils::is_safe_pattern(pkg) {
        eprintln!(
            "{}",
            format!("Error: Invalid package or pattern '{}'.", pkg)
                .red()
                .bold()
        );
        exit(1);
    }

    let manager = match AlpmManager::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    if let Err(e) = mutate_config(|current| {
        current.delay.insert(pkg.to_string(), days);
        Ok(())
    }) {
        eprintln!("{}", format!("Failed to save config: {}", e).red());
        exit(1);
    }
    println!(
        "{}",
        format!("✔ Set {}-day stability delay on '{}'.", days, pkg).green()
    );

    if !pkg.contains('*') && !pkg.contains('?') && !pkg.contains('[') {
        let dependents = manager.get_dependents(pkg);
        if !dependents.is_empty() {
            println!(
                "{}",
                ":: Notice: The following installed package(s) will also be held during this window to prevent ABI breakage:".cyan()
            );
            for d in dependents {
                println!("    • {}", d.yellow());
            }
        }
    }
}

pub fn cmd_undelay(pkg: &str) {
    print_banner();
    let removed = match mutate_config(|current| Ok(current.delay.remove(pkg).is_some())) {
        Ok(removed) => removed,
        Err(e) => {
            eprintln!("{}", format!("Failed to save config: {}", e).red());
            exit(1);
        }
    };
    if removed {
        println!(
            "{}",
            format!("✔ Removed stability delay on '{}'.", pkg).green()
        );
    } else {
        println!(
            "{}",
            format!("Package '{}' did not have a delay rule.", pkg).yellow()
        );
    }
}
