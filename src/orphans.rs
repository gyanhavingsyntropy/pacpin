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

use crate::journal::TransactionJournal;
use crate::resolver::ResolvedPackage;
use alpm::Alpm;
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanPackage {
    pub name: String,
    pub version: String,
    pub isize: i64,
    pub desc: String,
    pub is_projected: bool,
    pub dropped_by: Vec<String>,
    pub optional_for: Vec<String>,
}

pub struct OrphanManager;

impl OrphanManager {
    pub fn state_file() -> Option<PathBuf> {
        TransactionJournal::state_dir().ok().map(|d| d.join("known_orphans.json"))
    }

    pub fn load_known() -> Option<HashSet<String>> {
        let path = Self::state_file()?;
        if !path.exists() {
            return None;
        }
        let content = fs::read_to_string(&path).ok()?;
        let list: Vec<String> = serde_json::from_str(&content).ok()?;
        Some(list.into_iter().collect())
    }

    pub fn save_known(orphans: &[String]) {
        let path = match Self::state_file() {
            Some(p) => p,
            None => return,
        };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let mut sorted = orphans.to_vec();
        sorted.sort();
        sorted.dedup();
        if let Ok(json) = serde_json::to_string_pretty(&sorted) {
            let _ = fs::write(path, json);
        }
    }

    pub fn detect(alpm: &Alpm, updates: &[ResolvedPackage]) -> Vec<OrphanPackage> {
        let local = alpm.localdb();
        let mut orphans = Vec::new();

        let mut update_cand_deps: HashMap<String, HashSet<String>> = HashMap::new();
        for u in updates {
            if let Some(ref cand) = u.candidate {
                let dep_set: HashSet<String> = cand.depends.iter().cloned().collect();
                update_cand_deps.insert(u.name.clone(), dep_set);
            }
        }

        for pkg in local.pkgs() {
            if pkg.reason() != alpm::PackageReason::Depend {
                continue;
            }

            let name = pkg.name();
            let current_parents: Vec<String> =
                pkg.required_by().iter().map(|s| s.to_string()).collect();
            let opt_for: Vec<String> = pkg.optional_for().iter().map(|s| s.to_string()).collect();

            if current_parents.is_empty() {
                orphans.push(OrphanPackage {
                    name: name.to_string(),
                    version: pkg.version().to_string(),
                    isize: pkg.isize(),
                    desc: pkg.desc().unwrap_or("").to_string(),
                    is_projected: false,
                    dropped_by: Vec::new(),
                    optional_for: opt_for,
                });
            } else if !update_cand_deps.is_empty() {
                let mut all_dropping = true;
                let mut dropped_by = Vec::new();

                for parent in &current_parents {
                    if let Some(new_deps) = update_cand_deps.get(parent) {
                        if new_deps.contains(name) {
                            all_dropping = false;
                            break;
                        } else {
                            dropped_by.push(parent.clone());
                        }
                    } else {
                        all_dropping = false;
                        break;
                    }
                }

                if all_dropping && !dropped_by.is_empty() {
                    orphans.push(OrphanPackage {
                        name: name.to_string(),
                        version: pkg.version().to_string(),
                        isize: pkg.isize(),
                        desc: pkg.desc().unwrap_or("").to_string(),
                        is_projected: true,
                        dropped_by,
                        optional_for: opt_for,
                    });
                }
            }
        }

        orphans.sort_by(|a, b| {
            let a_opt = !a.optional_for.is_empty();
            let b_opt = !b.optional_for.is_empty();
            a_opt.cmp(&b_opt).then_with(|| a.name.cmp(&b.name))
        });

        orphans
    }

    pub fn has_changed(current: &[OrphanPackage]) -> bool {
        match Self::load_known() {
            Some(known) => current.iter().any(|o| !known.contains(&o.name)),
            None => !current.is_empty(),
        }
    }

    pub fn execute_removal(selected: &[String]) {
        if selected.is_empty() {
            return;
        }

        println!(
            "\n{} {}",
            ":: Removing selected orphaned package(s):".cyan(),
            selected.join(" ").bold()
        );

        let mut args = vec!["pacman", "-Rns", "--noconfirm"];
        let refs: Vec<&str> = selected.iter().map(|s| s.as_str()).collect();
        args.extend(refs);

        let remove_cmd = format!("sudo {}", args.join(" "));
        let status = Command::new("sudo").args(&args).status();

        match status {
            Ok(s) if s.success() => {
                let tx_packages: Vec<serde_json::Value> = selected
                    .iter()
                    .map(|name| serde_json::json!({ "name": name, "removed": true }))
                    .collect();
                let _ = TransactionJournal::record_transaction(
                    "remove_orphans",
                    tx_packages,
                    &remove_cmd,
                );
                println!(
                    "\n{}",
                    format!(
                        "✔ Removed {} orphaned package(s) successfully.",
                        selected.len()
                    )
                    .green()
                );
            }
            Ok(s) => {
                eprintln!(
                    "{}",
                    format!(
                        "Warning: Orphan removal exited with status code {}.",
                        s.code().unwrap_or(1)
                    )
                    .yellow()
                );
            }
            Err(e) => {
                eprintln!("{}", format!("Failed to run sudo pacman: {}", e).red());
            }
        }
    }
}
