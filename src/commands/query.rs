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

use std::collections::{BTreeSet, HashSet};
use std::io::{self, Write};
use std::process::exit;

use colored::Colorize;

use crate::config::Config;
use crate::db::AlpmManager;
use crate::pm;
use crate::sandbox;

pub fn cmd_search(config: &Config, query_args: &[String]) {
    match pm::search(config, query_args) {
        Ok(code) => {
            if code != 0 {
                exit(code);
            }
        }
        Err(e) => {
            eprintln!("{}", format!("Search execution failed: {}", e).red());
            exit(1);
        }
    }
}

pub fn cmd_info(config: &Config, pkg_args: &[String]) {
    match pm::info(config, pkg_args) {
        Ok(code) => {
            if code != 0 {
                exit(code);
            }
        }
        Err(e) => {
            eprintln!("{}", format!("Info query failed: {}", e).red());
            exit(1);
        }
    }
}

pub fn cmd_print_uris(config: &Config, targets: &[String]) {
    if targets.is_empty() {
        eprintln!("{}", "Error: No targets specified.".red());
        eprintln!("Usage: pacpin -Sp [repo/]pkg ...");
        exit(1);
    }
    let manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    let mut stdout = io::stdout().lock();
    for target in targets {
        let (repo, pkg) = sandbox::parse_target(target);
        match manager.resolve_download_urls(repo, pkg) {
            Ok(urls) => {
                for target_pkg in urls {
                    if writeln!(stdout, "{}", target_pkg.url).is_err() {
                        return;
                    }
                }
            }
            Err(e) => {
                eprintln!("{}", format!("Error resolving '{}': {}", target, e).red());
                exit(1);
            }
        }
    }
}

pub fn cmd_sync_list(config: &Config, repos: &[String]) {
    let manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    let alpm = manager.handle();
    let local_db = alpm.localdb();

    let filter_repos: HashSet<&str> = repos.iter().map(|s| s.as_str()).collect();

    if !repos.is_empty() {
        for r in repos {
            if !alpm.syncdbs().iter().any(|d| d.name() == r) {
                eprintln!("error: repository '{}' was not found", r);
                exit(1);
            }
        }
    }

    let mut stdout = io::stdout().lock();

    for db in alpm.syncdbs() {
        if !filter_repos.is_empty() && !filter_repos.contains(db.name()) {
            continue;
        }
        for pkg in db.pkgs() {
            let res = if let Ok(local_pkg) = local_db.pkg(pkg.name()) {
                if local_pkg.version() == pkg.version() {
                    writeln!(
                        stdout,
                        "{} {} {} [installed]",
                        db.name(),
                        pkg.name(),
                        pkg.version()
                    )
                } else {
                    writeln!(
                        stdout,
                        "{} {} {} [installed: {}]",
                        db.name(),
                        pkg.name(),
                        pkg.version(),
                        local_pkg.version()
                    )
                }
            } else {
                writeln!(stdout, "{} {} {}", db.name(), pkg.name(), pkg.version())
            };
            if res.is_err() {
                return;
            }
        }
    }
}

pub fn cmd_sync_groups(config: &Config, groups: &[String]) {
    let manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    let alpm = manager.handle();
    let filter_groups: HashSet<&str> = groups.iter().map(|s| s.as_str()).collect();
    let mut stdout = io::stdout().lock();

    if filter_groups.is_empty() {
        let mut all_groups = BTreeSet::new();
        for db in alpm.syncdbs() {
            for pkg in db.pkgs() {
                for grp in pkg.groups() {
                    all_groups.insert(grp.to_string());
                }
            }
        }
        for grp in all_groups {
            if writeln!(stdout, "{}", grp).is_err() {
                return;
            }
        }
    } else {
        for db in alpm.syncdbs() {
            for pkg in db.pkgs() {
                for grp in pkg.groups() {
                    if filter_groups.contains(grp)
                        && writeln!(stdout, "{} {}", grp, pkg.name()).is_err()
                    {
                        return;
                    }
                }
            }
        }
    }
}
