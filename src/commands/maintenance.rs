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

use std::io::{self, IsTerminal, Write};
use std::process::exit;

use colored::Colorize;

use crate::config::{self, Config};
use crate::integrations::IntegrationsManager;
use crate::pm;

pub fn cmd_clean(config: &Config, extra_args: &[String]) {
    match pm::clean(config, extra_args) {
        Ok(code) => {
            if code != 0 {
                exit(code);
            }
        }
        Err(e) => {
            eprintln!("{}", format!("Cache clean failed: {}", e).red());
            exit(1);
        }
    }

    let ext_providers = IntegrationsManager::get_active_providers(config);
    if !ext_providers.is_empty() {
        IntegrationsManager::execute_cleanups(&ext_providers);
    }
}

pub fn cmd_remove(mut config: Config, args: &[String], is_friendly: bool) {
    let mut flags = Vec::new();
    let mut targets = Vec::new();

    if is_friendly {
        flags.push("-Rns".to_string());
    }
    for a in args {
        if a.starts_with('-') {
            flags.push(a.clone());
        } else {
            targets.push(a.clone());
        }
    }

    match pm::remove(&mut config, &targets, &flags) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{}", format!("Error: {}", e).red());
            exit(1);
        }
    }
}

pub fn cmd_reset(force: bool, noconfirm: bool) {
    let path = config::get_config_path();
    if !path.exists() {
        println!("{}", "No configuration file found to reset.".yellow());
        return;
    }

    if !force && !noconfirm {
        if !io::stdin().is_terminal() {
            eprintln!(
                "{}",
                "Error: Cannot prompt for confirmation in non-interactive environment. Use --noconfirm or -f/--force."
                    .red()
            );
            exit(1);
        }
        print!(
            "{}",
            "Are you sure you want to reset all configurations and pins to default? [y/N] ".bold()
        );
        io::stdout().flush().unwrap();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            println!("\nAborted.");
            return;
        }
        let r = input.trim().to_lowercase();
        if r != "y" && r != "yes" {
            println!("Aborted.");
            return;
        }
    }

    // Backup current configuration
    let now = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let bak_path = path.with_extension(format!("toml.bak.{}", now));
    if let Err(e) = std::fs::copy(&path, &bak_path) {
        eprintln!(
            "{}",
            format!("Warning: Could not create backup file: {}", e).yellow()
        );
    } else {
        println!(
            "  {}",
            format!("📦 Backup created at {}", bak_path.display()).dimmed()
        );
    }

    let default_cfg = Config::default();
    if let Err(e) = config::save_config(&default_cfg) {
        eprintln!("{}", format!("Error resetting configuration: {}", e).red());
        exit(1);
    }

    println!(
        "{}",
        "✔ All configurations, pins, exclusions, and delays have been reset to default."
            .green()
            .bold()
    );
    println!("  Run 'pacpin init' to configure preferences from scratch.\n");
}
