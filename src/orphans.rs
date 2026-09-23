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

impl OrphanPackage {
    #[inline]
    pub fn is_pure(&self) -> bool {
        self.optional_for.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownOrphan {
    pub is_pure: bool,
    #[serde(default)]
    pub optional_for: Vec<String>,
}

pub struct OrphanManager;

impl OrphanManager {
    pub fn state_file() -> Option<PathBuf> {
        TransactionJournal::state_dir().ok().map(|d| d.join("known_orphans.json"))
    }

    pub fn load_known() -> Option<HashMap<String, KnownOrphan>> {
        let path = Self::state_file()?;
        if !path.exists() {
            return None;
        }
        let content = fs::read_to_string(&path).ok()?;

        // 1. Modern detailed format: Map of package name -> KnownOrphan
        if let Ok(map) = serde_json::from_str::<HashMap<String, KnownOrphan>>(&content) {
            return Some(map);
        }

        // 2. Backward compatibility: legacy flat array of package name strings
        if let Ok(list) = serde_json::from_str::<Vec<String>>(&content) {
            let map = list
                .into_iter()
                .map(|name| {
                    (
                        name,
                        KnownOrphan {
                            is_pure: false, // Default to false so any pure orphan alerts the user
                            optional_for: Vec::new(),
                        },
                    )
                })
                .collect();
            return Some(map);
        }

        None
    }

    pub fn save_known(orphans: &[OrphanPackage]) {
        let path = match Self::state_file() {
            Some(p) => p,
            None => return,
        };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let mut map: std::collections::BTreeMap<String, KnownOrphan> = std::collections::BTreeMap::new();
        for o in orphans {
            map.insert(
                o.name.clone(),
                KnownOrphan {
                    is_pure: o.is_pure(),
                    optional_for: o.optional_for.clone(),
                },
            );
        }
        if let Ok(json) = serde_json::to_string_pretty(&map) {
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
            Some(known) => current.iter().any(|o| match known.get(&o.name) {
                None => true,
                Some(record) => record.is_pure != o.is_pure(),
            }),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orphan_is_pure() {
        let pure = OrphanPackage {
            name: "pure-pkg".to_string(),
            version: "1.0".to_string(),
            isize: 100,
            desc: "pure orphan".to_string(),
            is_projected: false,
            dropped_by: Vec::new(),
            optional_for: Vec::new(),
        };
        assert!(pure.is_pure());

        let opt = OrphanPackage {
            name: "opt-pkg".to_string(),
            version: "1.0".to_string(),
            isize: 200,
            desc: "optional orphan".to_string(),
            is_projected: false,
            dropped_by: Vec::new(),
            optional_for: vec!["parent-app".to_string()],
        };
        assert!(!opt.is_pure());
    }

    #[test]
    fn test_has_changed_logic() {
        let mut known = HashMap::new();
        known.insert(
            "ladspa".to_string(),
            KnownOrphan {
                is_pure: false,
                optional_for: vec!["ffmpeg".to_string()],
            },
        );
        known.insert(
            "meson".to_string(),
            KnownOrphan {
                is_pure: true,
                optional_for: Vec::new(),
            },
        );

        // 1. Identical current state -> no change
        let current_identical = [
            OrphanPackage {
                name: "ladspa".to_string(),
                version: "1.0".to_string(),
                isize: 100,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: vec!["ffmpeg".to_string()],
            },
            OrphanPackage {
                name: "meson".to_string(),
                version: "1.0".to_string(),
                isize: 200,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: Vec::new(),
            },
        ];
        let has_change = current_identical.iter().any(|o| match known.get(&o.name) {
            None => true,
            Some(r) => r.is_pure != o.is_pure(),
        });
        assert!(!has_change);

        // 2. 'ladspa' converts from optional to pure -> MUST detect change!
        let current_converted = [
            OrphanPackage {
                name: "ladspa".to_string(),
                version: "1.0".to_string(),
                isize: 100,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: Vec::new(), // Now pure!
            },
            OrphanPackage {
                name: "meson".to_string(),
                version: "1.0".to_string(),
                isize: 200,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: Vec::new(),
            },
        ];
        let has_change_converted = current_converted.iter().any(|o| match known.get(&o.name) {
            None => true,
            Some(r) => r.is_pure != o.is_pure(),
        });
        assert!(has_change_converted);

        // 3. Brand new orphan appears -> MUST detect change!
        let current_new_orphan = [
            OrphanPackage {
                name: "new-orphan".to_string(),
                version: "1.0".to_string(),
                isize: 50,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: Vec::new(),
            },
        ];
        let has_change_new = current_new_orphan.iter().any(|o| match known.get(&o.name) {
            None => true,
            Some(r) => r.is_pure != o.is_pure(),
        });
        assert!(has_change_new);
    }
}

