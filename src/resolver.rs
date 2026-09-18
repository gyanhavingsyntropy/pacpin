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

use crate::aur::{self, AurItem};
use crate::config::Config;
use crate::db::AlpmManager;
use alpm::vercmp;
use glob::Pattern;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::process::Command;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidatePackage {
    pub name: String,
    pub version: String,
    pub repo: String,
    pub base: String,
    pub csize: i64,
    pub isize: i64,
    pub desc: String,
    pub builddate: i64,
    pub is_aur: bool,
    #[serde(default)]
    pub depends: Vec<String>,
}


impl CandidatePackage {
    pub fn age_days(&self) -> f64 {
        if self.builddate <= 0 {
            return 9999.0;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let diff = (now - self.builddate).max(0);
        (diff as f64) / 86400.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedPackage {
    pub name: String,
    pub state: String,
    pub pinned_repo: Option<String>,
    pub installed_ver: String,
    pub installed_size: i64,
    pub installed_db: String,
    pub candidate: Option<CandidatePackage>,
    pub natural_top_repo: Option<String>,
    pub diverted_from_top: bool,
    pub needs_update: bool,
    pub update_type: String,
    pub is_downgrade: bool,
    pub net_delta: i64,
    pub held: bool,
    pub hold_reason: String,
}

#[derive(Debug, Clone)]
pub struct ResolveResult {
    pub installed_count: usize,
    pub repos: Vec<String>,
    pub packages: HashMap<String, ResolvedPackage>,
    pub updates: Vec<ResolvedPackage>,
    pub held_packages: Vec<ResolvedPackage>,
    pub unresolved_pins: Vec<(String, String)>,
}

pub struct ResolverEngine<'a> {
    manager: &'a AlpmManager,
}

impl<'a> ResolverEngine<'a> {
    pub fn new(manager: &'a AlpmManager) -> Self {
        Self { manager }
    }

    pub fn is_pinned(pkg_name: &str, pins: &BTreeMap<String, String>) -> Option<String> {
        if let Some(repo) = pins.get(pkg_name) {
            return Some(repo.clone());
        }
        let mut sorted_pins: Vec<(&String, &String)> = pins.iter().collect();
        sorted_pins.sort_by_key(|(pat, _)| std::cmp::Reverse(pat.len()));

        for (pat, repo) in sorted_pins {
            if Pattern::new(pat)
                .map(|p| p.matches(pkg_name))
                .unwrap_or(false)
            {
                return Some((*repo).clone());
            }
        }
        None
    }

    pub fn is_excluded(
        pkg_name: &str,
        repo: &str,
        excludes: &BTreeMap<String, Vec<String>>,
    ) -> bool {
        if let Some(patterns) = excludes.get(repo) {
            for pat in patterns {
                if pat == pkg_name {
                    return true;
                }
                if Pattern::new(pat)
                    .map(|p| p.matches(pkg_name))
                    .unwrap_or(false)
                {
                    return true;
                }
            }
        }
        false
    }

    pub fn resolve_all(&self, config: &Config) -> ResolveResult {
        let pins = if config.features.pinning {
            config.pins.clone()
        } else {
            BTreeMap::new()
        };
        let aur_handle = thread::spawn(move || -> HashMap<String, AurItem> {
            let mut foreign_pkgs = Vec::new();
            if let Ok(output) = Command::new("pacman").arg("-Qm").output() {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for line in stdout.lines() {
                        if let Some(name) = line.split_whitespace().next() {
                            foreign_pkgs.push(name.to_string());
                        }
                    }
                }
            }
            for (pat, rep) in &pins {
                if rep.eq_ignore_ascii_case("aur")
                    && !pat.contains('*')
                    && !pat.contains('?')
                    && !pat.contains('[')
                {
                    if !foreign_pkgs.contains(pat) {
                        foreign_pkgs.push(pat.clone());
                    }
                }
            }
            if !foreign_pkgs.is_empty() {
                aur::query_aur(&foreign_pkgs)
            } else {
                HashMap::new()
            }
        });

        let alpm = self.manager.handle();
        let repos = self.manager.repos();
        let local_pkgs = alpm.localdb().pkgs();
        let installed_count = local_pkgs.len();

        let aur_data = aur_handle.join().unwrap_or_default();

        let mut resolved: HashMap<String, ResolvedPackage> = HashMap::new();
        let mut unresolved_pins: Vec<(String, String)> = Vec::new();

        for inst_pkg in local_pkgs {
            let pkg_name = inst_pkg.name();
            let inst_ver = inst_pkg.version();
            let inst_size = inst_pkg.isize();

            let pinned_repo = if config.features.pinning {
                Self::is_pinned(pkg_name, &config.pins)
            } else {
                None
            };
            let state = if pinned_repo.is_some() {
                "custom".to_string()
            } else {
                "default".to_string()
            };

            let mut natural_top_repo = None;
            for r in repos {
                if let Some(db) = alpm.syncdbs().into_iter().find(|d| d.name() == r) {
                    if db.pkg(pkg_name).is_ok() {
                        natural_top_repo = Some(r.clone());
                        break;
                    }
                }
            }

            let mut candidate: Option<CandidatePackage> = None;

            if state == "custom" {
                let target_repo = pinned_repo.as_ref().unwrap();
                if target_repo.eq_ignore_ascii_case("aur") {
                    if let Some(aur_pkg) = aur_data.get(pkg_name) {
                        candidate = Some(CandidatePackage {
                            name: aur_pkg.name.clone(),
                            version: aur_pkg.version.clone(),
                            repo: "aur".to_string(),
                            base: aur_pkg.name.clone(),
                            csize: 0,
                            isize: 0,
                            desc: aur_pkg.description.clone().unwrap_or_default(),
                            builddate: 0,
                            is_aur: true,
                            depends: Vec::new(),
                        });
                    } else {
                        unresolved_pins.push((pkg_name.to_string(), target_repo.clone()));
                    }
                } else if let Some(db) = alpm.syncdbs().into_iter().find(|d| d.name() == target_repo) {
                    if let Ok(p) = db.pkg(pkg_name) {
                        candidate = Some(CandidatePackage {
                            name: p.name().to_string(),
                            version: p.version().to_string(),
                            repo: target_repo.clone(),
                            base: p.base().unwrap_or(p.name()).to_string(),
                            csize: p.size(),
                            isize: p.isize(),
                            desc: p.desc().unwrap_or("").to_string(),
                            builddate: p.build_date(),
                            is_aur: false,
                            depends: p.depends().iter().map(|d| d.name().to_string()).collect(),
                        });
                    } else {
                        unresolved_pins.push((pkg_name.to_string(), target_repo.clone()));
                    }
                } else {
                    unresolved_pins.push((pkg_name.to_string(), target_repo.clone()));
                }
            } else {
                for r in repos {
                    if config.features.pinning && Self::is_excluded(pkg_name, r, &config.exclude) {
                        continue;
                    }
                    if let Some(db) = alpm.syncdbs().into_iter().find(|d| d.name() == r) {
                        if let Ok(p) = db.pkg(pkg_name) {
                            candidate = Some(CandidatePackage {
                                name: p.name().to_string(),
                                version: p.version().to_string(),
                                repo: r.clone(),
                                base: p.base().unwrap_or(p.name()).to_string(),
                                csize: p.size(),
                                isize: p.isize(),
                                desc: p.desc().unwrap_or("").to_string(),
                                builddate: p.build_date(),
                                is_aur: false,
                                depends: p.depends().iter().map(|d| d.name().to_string()).collect(),
                            });
                            break;
                        }
                    }
                }

                if candidate.is_none() {
                    if let Some(aur_pkg) = aur_data.get(pkg_name) {
                        candidate = Some(CandidatePackage {
                            name: aur_pkg.name.clone(),
                            version: aur_pkg.version.clone(),
                            repo: "aur".to_string(),
                            base: aur_pkg.name.clone(),
                            csize: 0,
                            isize: 0,
                            desc: aur_pkg.description.clone().unwrap_or_default(),
                            builddate: 0,
                            is_aur: true,
                            depends: Vec::new(),
                        });
                    }
                }
            }


            let mut installed_db = String::new();
            let mut fallback_repo = None;
            for r in repos {
                if let Some(db) = alpm.syncdbs().into_iter().find(|d| d.name() == r) {
                    if let Ok(p) = db.pkg(pkg_name) {
                        if vercmp(p.version().as_str(), inst_ver) == Ordering::Equal {
                            installed_db = r.clone();
                            break;
                        } else if fallback_repo.is_none() {
                            fallback_repo = Some(r.clone());
                        }
                    }
                }
            }
            if installed_db.is_empty() {
                if let Some(r) = fallback_repo {
                    installed_db = r;
                }
            }

            let mut needs_update = false;
            let mut update_type = "up_to_date".to_string();
            let mut is_downgrade = false;

            if let Some(ref cand) = candidate {
                let cmp = vercmp(cand.version.as_str(), inst_ver);
                match cmp {
                    Ordering::Greater => {
                        needs_update = true;
                        update_type = "upgrade".to_string();
                    }
                    Ordering::Less => {
                        if state == "custom" {
                            needs_update = true;
                            update_type = "downgrade".to_string();
                            is_downgrade = true;
                        }
                    }
                    Ordering::Equal => {
                        if state == "custom" && cand.version != inst_ver.as_str() {
                            needs_update = true;
                            update_type = "pin_sync".to_string();
                        }
                    }

                }
            }

            let net_delta = if let Some(ref cand) = candidate {
                cand.isize - inst_size
            } else {
                0
            };

            let diverted_from_top = state == "default"
                && candidate.is_some()
                && natural_top_repo.is_some()
                && candidate.as_ref().unwrap().repo != *natural_top_repo.as_ref().unwrap();

            resolved.insert(
                pkg_name.to_string(),
                ResolvedPackage {
                    name: pkg_name.to_string(),
                    state,
                    pinned_repo,
                    installed_ver: inst_ver.to_string(),
                    installed_size: inst_size,
                    installed_db,
                    candidate,
                    natural_top_repo,
                    diverted_from_top,
                    needs_update,
                    update_type,
                    is_downgrade,
                    net_delta,
                    held: false,
                    hold_reason: String::new(),
                },
            );
        }

        // Evaluate stability delay buffers
        let mut held_parents = HashMap::new();
        if config.features.stability_delays {
            for (pkg_name, item) in resolved.iter_mut() {
                if !item.needs_update || item.candidate.is_none() {
                    continue;
                }
                if let Some(req_days) = config.delay.get(pkg_name) {
                    let cand = item.candidate.as_ref().unwrap();
                    let cand_age = cand.age_days();
                    let req_days_f = *req_days as f64;
                    if cand_age < req_days_f {
                        item.held = true;
                        let days_left = ((req_days_f - cand_age + 0.99) as u32).max(1);
                        item.hold_reason = format!("Delay buffer: {}d remaining", days_left);
                        held_parents.insert(pkg_name.clone(), item.hold_reason.clone());
                    }
                }
            }
        }

        // Cascade holds to downstream reverse dependents
        for parent_name in held_parents.keys() {
            let dependents = self.manager.get_dependents(parent_name);
            for dep_name in dependents {
                if let Some(dep_item) = resolved.get_mut(&dep_name) {
                    if dep_item.needs_update {
                        dep_item.held = true;
                        dep_item.hold_reason =
                            format!("Cascade hold: depends on newer {}", parent_name);
                    }
                }
            }
        }

        let mut updates: Vec<ResolvedPackage> = resolved
            .values()
            .filter(|p| p.needs_update && !p.held)
            .cloned()
            .collect();
        updates.sort_by(|a, b| {
            let a_cust = if a.state == "custom" { 0 } else { 1 };
            let b_cust = if b.state == "custom" { 0 } else { 1 };
            (a_cust, &a.name).cmp(&(b_cust, &b.name))
        });

        let mut held_packages: Vec<ResolvedPackage> = resolved
            .values()
            .filter(|p| p.needs_update && p.held)
            .cloned()
            .collect();
        held_packages.sort_by(|a, b| a.name.cmp(&b.name));

        ResolveResult {
            installed_count,
            repos: repos.to_vec(),
            packages: resolved,
            updates,
            held_packages,
            unresolved_pins,
        }
    }
}
