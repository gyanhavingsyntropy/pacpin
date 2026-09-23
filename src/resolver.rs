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
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
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
    pub fn from_aur(aur_pkg: &crate::aur::AurItem) -> Self {
        let cleaned_deps: Vec<String> = aur_pkg
            .depends
            .iter()
            .map(|d| crate::aur::clean_dep_name(d).to_string())
            .collect();

        Self {
            name: aur_pkg.name.clone(),
            version: aur_pkg.version.clone(),
            repo: "aur".to_string(),
            base: aur_pkg
                .package_base
                .clone()
                .unwrap_or_else(|| aur_pkg.name.clone()),
            csize: 0,
            isize: 0,
            desc: aur_pkg.description.clone().unwrap_or_default(),
            builddate: aur_pkg.last_modified.unwrap_or(0),
            is_aur: true,
            depends: cleaned_deps,
        }
    }

    pub fn compute_net_delta(&self, installed_size: i64) -> Option<i64> {
        if self.isize > 0 {
            Some(self.isize - installed_size)
        } else {
            None
        }
    }

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
    pub net_delta: Option<i64>,
    pub held: bool,
    pub hold_reason: String,
}

#[derive(Debug, Clone)]
pub struct ResolveResult {
    pub installed_count: usize,
    pub repos: Vec<String>,
    #[allow(dead_code)]
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

    pub fn find_matching_pin_rule(
        pkg_name: &str,
        pins: &BTreeMap<String, String>,
        glob_pins: &[(String, Pattern, String)],
    ) -> Option<(String, String, bool)> {
        if let Some(repo) = pins.get(pkg_name) {
            return Some((pkg_name.to_string(), repo.clone(), true));
        }
        glob_pins
            .iter()
            .find(|(_, pattern, _)| pattern.matches(pkg_name))
            .map(|(pat, _, repo)| (pat.clone(), repo.clone(), false))
    }

    pub fn match_pinned_package(
        pkg_name: &str,
        pins: &BTreeMap<String, String>,
        glob_pins: &[(String, Pattern, String)],
    ) -> Option<String> {
        Self::find_matching_pin_rule(pkg_name, pins, glob_pins).map(|(_, repo, _)| repo)
    }

    pub fn compile_glob_pins(pins: &BTreeMap<String, String>) -> Vec<(String, Pattern, String)> {
        let mut glob_pins = Vec::new();
        for (pat, repo) in pins {
            if pat.contains('*') || pat.contains('?') || pat.contains('[') {
                if let Ok(p) = Pattern::new(pat) {
                    glob_pins.push((pat.clone(), p, repo.clone()));
                }
            }
        }
        glob_pins.sort_by_key(|(pat, _, _)| std::cmp::Reverse(pat.len()));
        glob_pins
    }

    #[allow(dead_code)]
    pub fn is_pinned(pkg_name: &str, pins: &BTreeMap<String, String>) -> Option<String> {
        let glob_pins = Self::compile_glob_pins(pins);
        Self::match_pinned_package(pkg_name, pins, &glob_pins)
    }

    pub fn compile_glob_delays(delays: &BTreeMap<String, u32>) -> Vec<(String, Pattern, u32)> {
        let mut glob_delays = Vec::new();
        for (pat, days) in delays {
            if pat.contains('*') || pat.contains('?') || pat.contains('[') {
                if let Ok(p) = Pattern::new(pat) {
                    glob_delays.push((pat.clone(), p, *days));
                }
            }
        }
        glob_delays.sort_by_key(|(pat, _, _)| std::cmp::Reverse(pat.len()));
        glob_delays
    }

    pub fn match_delay_days(
        pkg_name: &str,
        delays: &BTreeMap<String, u32>,
        glob_delays: &[(String, Pattern, u32)],
    ) -> Option<u32> {
        if let Some(days) = delays.get(pkg_name) {
            return Some(*days);
        }
        glob_delays
            .iter()
            .find(|(_, pattern, _)| pattern.matches(pkg_name))
            .map(|(_, _, days)| *days)
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

    pub fn load_installed_dbs() -> HashMap<String, String> {
        use std::io::Read;
        let mut map = HashMap::new();
        let dbpath = crate::db::AlpmManager::get_dbpath();
        let local_dir = dbpath.join("local");
        let mut buf = [0u8; 1024];

        if let Ok(entries) = std::fs::read_dir(local_dir) {
            for entry in entries.flatten() {
                let desc_path = entry.path().join("desc");
                if let Ok(mut file) = std::fs::File::open(&desc_path) {
                    if let Ok(n) = file.read(&mut buf) {
                        if let Ok(content) = std::str::from_utf8(&buf[..n]) {
                            let mut name = None;
                            let mut db = None;
                            let mut lines = content.lines();
                            while let Some(line) = lines.next() {
                                if line == "%NAME%" {
                                    if let Some(val) = lines.next() {
                                        name = Some(val.trim().to_string());
                                    }
                                } else if line == "%INSTALLED_DB%" {
                                    if let Some(val) = lines.next() {
                                        db = Some(val.trim().to_string());
                                    }
                                }
                                if name.is_some() && db.is_some() {
                                    break;
                                }
                            }
                            if let (Some(n), Some(d)) = (name, db) {
                                map.insert(n, d);
                            }
                        }
                    }
                }
            }
        }
        map
    }

    pub fn resolve_all(&self, config: &Config) -> ResolveResult {
        let pins = if config.features.pinning {
            config.pins.clone()
        } else {
            BTreeMap::new()
        };
        let alpm = self.manager.handle();
        let syncdbs: Vec<&alpm::Db> = alpm.syncdbs().into_iter().collect();
        let mut foreign_pkgs_set = HashSet::new();
        let mut foreign_pkgs = Vec::new();
        for pkg in alpm.localdb().pkgs() {
            if !syncdbs.iter().any(|db| db.pkg(pkg.name()).is_ok()) {
                let name = pkg.name().to_string();
                if foreign_pkgs_set.insert(name.clone()) {
                    foreign_pkgs.push(name);
                }
            }
        }
        for (pattern, repo) in &pins {
            if !repo.eq_ignore_ascii_case("aur") {
                continue;
            }
            if let Ok(glob) = Pattern::new(pattern) {
                for pkg in alpm.localdb().pkgs() {
                    if glob.matches(pkg.name()) {
                        let name = pkg.name().to_string();
                        if foreign_pkgs_set.insert(name.clone()) {
                            foreign_pkgs.push(name);
                        }
                    }
                }
            }
            if !pattern.contains('*')
                && !pattern.contains('?')
                && !pattern.contains('[')
                && foreign_pkgs_set.insert(pattern.clone())
            {
                foreign_pkgs.push(pattern.clone());
            }
        }

        let aur_handle = thread::spawn(move || -> HashMap<String, AurItem> {
            if !foreign_pkgs.is_empty() {
                aur::query_aur(&foreign_pkgs)
            } else {
                HashMap::new()
            }
        });

        let installed_dbs_handle = thread::spawn(Self::load_installed_dbs);

        let alpm = self.manager.handle();
        let repos = self.manager.repos();
        let local_pkgs = alpm.localdb().pkgs();
        let installed_count = local_pkgs.len();

        let aur_data = aur_handle.join().unwrap_or_default();
        let installed_dbs = installed_dbs_handle.join().unwrap_or_default();

        let syncdbs_list: Vec<&alpm::Db> = alpm.syncdbs().into_iter().collect();
        let syncdb_map: HashMap<&str, &alpm::Db> =
            syncdbs_list.iter().map(|d| (d.name(), *d)).collect();

        let glob_pins = if config.features.pinning {
            Self::compile_glob_pins(&config.pins)
        } else {
            Vec::new()
        };

        let mut resolved: HashMap<String, ResolvedPackage> = HashMap::new();
        let mut unresolved_pins: Vec<(String, String)> = Vec::new();

        for inst_pkg in local_pkgs {
            let pkg_name = inst_pkg.name();
            let inst_ver = inst_pkg.version();
            let inst_size = inst_pkg.isize();

            // Determine originating repository
            let mut installed_db = installed_dbs.get(pkg_name).cloned().unwrap_or_default();
            if installed_db.is_empty() {
                let mut fallback_repo = None;
                for r in repos {
                    if let Some(&db) = syncdb_map.get(r.as_str()) {
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
            }

            let pinned_repo = if config.features.pinning {
                Self::match_pinned_package(pkg_name, &config.pins, &glob_pins)
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
                if let Some(&db) = syncdb_map.get(r.as_str()) {
                    if db.pkg(pkg_name).is_ok() {
                        natural_top_repo = Some(r.clone());
                        break;
                    }
                }
            }

            let mut candidate: Option<CandidatePackage> = None;
            let mut is_sticky = false;

            if state == "custom" {
                let target_repo = pinned_repo.as_ref().unwrap();
                if target_repo.eq_ignore_ascii_case("aur") {
                    if let Some(aur_pkg) = aur_data.get(pkg_name) {
                        candidate = Some(CandidatePackage::from_aur(aur_pkg));
                    } else {
                        unresolved_pins.push((pkg_name.to_string(), target_repo.clone()));
                    }
                } else if let Some(&db) = syncdb_map.get(target_repo.as_str()) {
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
                // Opt-in vendor stickiness: if enabled, stick to the originating repository
                if config.features.vendor_stickiness
                    && !installed_db.is_empty()
                    && !(config.features.pinning
                        && Self::is_excluded(pkg_name, &installed_db, &config.exclude))
                {
                    if installed_db.eq_ignore_ascii_case("aur") {
                        if let Some(aur_pkg) = aur_data.get(pkg_name) {
                            candidate = Some(CandidatePackage::from_aur(aur_pkg));
                            is_sticky = true;
                        }
                    } else if let Some(&db) = syncdb_map.get(installed_db.as_str()) {
                        if let Ok(p) = db.pkg(pkg_name) {
                            candidate = Some(CandidatePackage {
                                name: p.name().to_string(),
                                version: p.version().to_string(),
                                repo: installed_db.clone(),
                                base: p.base().unwrap_or(p.name()).to_string(),
                                csize: p.size(),
                                isize: p.isize(),
                                desc: p.desc().unwrap_or("").to_string(),
                                builddate: p.build_date(),
                                is_aur: false,
                                depends: p.depends().iter().map(|d| d.name().to_string()).collect(),
                            });
                            is_sticky = true;
                        }
                    }
                }

                // If not resolved by vendor stickiness, search repos in priority order
                if candidate.is_none() {
                    for r in repos {
                        if config.features.pinning
                            && Self::is_excluded(pkg_name, r, &config.exclude)
                        {
                            continue;
                        }
                        if let Some(&db) = syncdb_map.get(r.as_str()) {
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
                                    depends: p
                                        .depends()
                                        .iter()
                                        .map(|d| d.name().to_string())
                                        .collect(),
                                });
                                break;
                            }
                        }
                    }

                    if candidate.is_none() {
                        if let Some(aur_pkg) = aur_data.get(pkg_name) {
                            candidate = Some(CandidatePackage::from_aur(aur_pkg));
                        }
                    }
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

            let net_delta = candidate
                .as_ref()
                .and_then(|cand| cand.compute_net_delta(inst_size));

            let final_state = if state == "custom" {
                "custom".to_string()
            } else if is_sticky && candidate.is_some() {
                "sticky".to_string()
            } else {
                "default".to_string()
            };

            let diverted_from_top = final_state == "default"
                && candidate.is_some()
                && natural_top_repo.is_some()
                && candidate.as_ref().unwrap().repo != *natural_top_repo.as_ref().unwrap();

            resolved.insert(
                pkg_name.to_string(),
                ResolvedPackage {
                    name: pkg_name.to_string(),
                    state: final_state,
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
            let glob_delays = Self::compile_glob_delays(&config.delay);
            for (pkg_name, item) in resolved.iter_mut() {
                if !item.needs_update || item.candidate.is_none() {
                    continue;
                }
                if let Some(req_days) =
                    Self::match_delay_days(pkg_name, &config.delay, &glob_delays)
                {
                    let cand = item.candidate.as_ref().unwrap();
                    let cand_age = cand.age_days();
                    let req_days_f = req_days as f64;
                    if cand_age < req_days_f {
                        item.held = true;
                        let days_left = ((req_days_f - cand_age + 0.99) as u32).max(1);
                        item.hold_reason = format!("Delay buffer: {}d remaining", days_left);
                        held_parents.insert(pkg_name.clone(), item.hold_reason.clone());
                    }
                }
            }
        }

        // Cascade holds to downstream reverse dependents (transitive BFS)
        let mut queue: VecDeque<String> = held_parents.keys().cloned().collect();
        let mut visited: HashSet<String> = held_parents.keys().cloned().collect();

        while let Some(current_held) = queue.pop_front() {
            let dependents = self.manager.get_dependents(&current_held);
            for dep_name in dependents {
                if let Some(dep_item) = resolved.get_mut(&dep_name) {
                    if dep_item.needs_update && !dep_item.held {
                        dep_item.held = true;
                        dep_item.hold_reason =
                            format!("Cascade hold: depends on held {}", current_held);
                    }
                }
                if visited.insert(dep_name.clone()) {
                    queue.push_back(dep_name);
                }
            }
        }

        let mut updates: Vec<ResolvedPackage> = resolved
            .values()
            .filter(|p| p.needs_update && !p.held)
            .cloned()
            .collect();
        updates.sort_by(|a, b| {
            let a_rank = match a.state.as_str() {
                "custom" => 0,
                "sticky" => 1,
                _ => 2,
            };
            let b_rank = match b.state.as_str() {
                "custom" => 0,
                "sticky" => 1,
                _ => 2,
            };
            (a_rank, &a.name).cmp(&(b_rank, &b.name))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_pinned_exact_and_glob() {
        let mut pins = BTreeMap::new();
        pins.insert("mesa".to_string(), "core".to_string());
        pins.insert("linux-firmware*".to_string(), "cachyos".to_string());

        assert_eq!(
            ResolverEngine::is_pinned("mesa", &pins),
            Some("core".to_string())
        );
        assert_eq!(
            ResolverEngine::is_pinned("linux-firmware-whence", &pins),
            Some("cachyos".to_string())
        );
        assert_eq!(ResolverEngine::is_pinned("git", &pins), None);
    }

    #[test]
    fn test_is_excluded() {
        let mut excludes = BTreeMap::new();
        excludes.insert(
            "cachyos".to_string(),
            vec!["linux-firmware*".to_string(), "systemd".to_string()],
        );

        assert!(ResolverEngine::is_excluded(
            "linux-firmware-intel",
            "cachyos",
            &excludes
        ));
        assert!(ResolverEngine::is_excluded("systemd", "cachyos", &excludes));
        assert!(!ResolverEngine::is_excluded("linux", "cachyos", &excludes));
        assert!(!ResolverEngine::is_excluded("systemd", "core", &excludes));
    }

    #[test]
    fn test_load_installed_dbs_executes() {
        let dbs = ResolverEngine::load_installed_dbs();
        let dbpath = crate::db::AlpmManager::get_dbpath();
        if dbpath.join("local").exists() {
            assert!(!dbs.is_empty());
        }
    }

    #[test]
    fn test_candidate_package_age_days() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let cand_recent = CandidatePackage {
            name: "test-pkg".to_string(),
            version: "1.0".to_string(),
            repo: "aur".to_string(),
            base: "test-pkg".to_string(),
            csize: 0,
            isize: 0,
            desc: "".to_string(),
            builddate: now - 86400 * 2, // 2 days ago
            is_aur: true,
            depends: Vec::new(),
        };
        let age = cand_recent.age_days();
        assert!((1.9..=2.1).contains(&age));

        let cand_zero = CandidatePackage {
            builddate: 0,
            ..cand_recent
        };
        assert_eq!(cand_zero.age_days(), 9999.0);
    }

    #[test]
    fn test_match_delay_days_exact_and_glob() {
        let mut delays = BTreeMap::new();
        delays.insert("linux".to_string(), 5);
        delays.insert("linux-*".to_string(), 3);
        delays.insert("*nvidia*".to_string(), 7);

        let glob_delays = ResolverEngine::compile_glob_delays(&delays);

        // Exact match takes precedence
        assert_eq!(
            ResolverEngine::match_delay_days("linux", &delays, &glob_delays),
            Some(5)
        );
        // Glob pattern matches
        assert_eq!(
            ResolverEngine::match_delay_days("linux-zen", &delays, &glob_delays),
            Some(3)
        );
        assert_eq!(
            ResolverEngine::match_delay_days("linux-cachyos-bore", &delays, &glob_delays),
            Some(3)
        );
        assert_eq!(
            ResolverEngine::match_delay_days("nvidia-utils", &delays, &glob_delays),
            Some(7)
        );
        // Non-matching
        assert_eq!(
            ResolverEngine::match_delay_days("mesa", &delays, &glob_delays),
            None
        );
    }

    #[test]
    fn test_candidate_package_from_aur() {
        let aur_item = crate::aur::AurItem {
            name: "mcpelauncher-ui".to_string(),
            package_base: Some("mcpelauncher".to_string()),
            version: "1.0.0-1".to_string(),
            description: Some("Launcher UI".to_string()),
            last_modified: Some(1700000000),
            depends: vec![
                "mcpelauncher-linux".to_string(),
                "qt6-base>=6.5".to_string(),
            ],
            make_depends: vec!["cmake".to_string()],
            conflicts: vec![],
        };
        let cand = CandidatePackage::from_aur(&aur_item);
        assert_eq!(cand.name, "mcpelauncher-ui");
        assert_eq!(cand.base, "mcpelauncher");
        assert_eq!(cand.repo, "aur");
        assert_eq!(cand.depends, vec!["mcpelauncher-linux", "qt6-base"]);
        assert!(cand.is_aur);
    }

    #[test]
    fn test_compute_net_delta() {
        // AUR candidate: size is unbuilt/unknown, must never falsely report -installed_size
        let aur_cand = CandidatePackage {
            name: "antigravity".to_string(),
            version: "2.15.0-1".to_string(),
            repo: "aur".to_string(),
            base: "antigravity".to_string(),
            csize: 0,
            isize: 0,
            desc: "Google Antigravity".to_string(),
            builddate: 1000,
            is_aur: true,
            depends: Vec::new(),
        };
        let antigravity_inst_size = 507 * 1024 * 1024;
        assert_eq!(aur_cand.compute_net_delta(antigravity_inst_size), None);

        // Built/cached AUR candidate: has real isize, computes exact delta
        let aur_cand_cached = CandidatePackage {
            name: "antigravity".to_string(),
            version: "2.16.0-1".to_string(),
            repo: "aur".to_string(),
            base: "antigravity".to_string(),
            csize: 156 * 1024 * 1024,
            isize: 495 * 1024 * 1024,
            desc: "Google Antigravity".to_string(),
            builddate: 1000,
            is_aur: true,
            depends: Vec::new(),
        };
        assert_eq!(
            aur_cand_cached.compute_net_delta(507 * 1024 * 1024),
            Some(-12 * 1024 * 1024)
        );

        // Repo candidate: size increases
        let repo_cand_growth = CandidatePackage {
            name: "glibc".to_string(),
            version: "2.40-1".to_string(),
            repo: "core".to_string(),
            base: "glibc".to_string(),
            csize: 10 * 1024 * 1024,
            isize: 50 * 1024 * 1024,
            desc: "GNU C Library".to_string(),
            builddate: 1000,
            is_aur: false,
            depends: Vec::new(),
        };
        assert_eq!(
            repo_cand_growth.compute_net_delta(45 * 1024 * 1024),
            Some(5 * 1024 * 1024)
        );

        // Repo candidate: size decreases
        assert_eq!(
            repo_cand_growth.compute_net_delta(55 * 1024 * 1024),
            Some(-5 * 1024 * 1024)
        );

        // Missing metadata candidate (both csize and isize are 0)
        let missing_meta_cand = CandidatePackage {
            name: "empty-meta".to_string(),
            version: "1.0-1".to_string(),
            repo: "extra".to_string(),
            base: "empty-meta".to_string(),
            csize: 0,
            isize: 0,
            desc: "".to_string(),
            builddate: 1000,
            is_aur: false,
            depends: Vec::new(),
        };
        assert_eq!(missing_meta_cand.compute_net_delta(1024), None);
    }

    #[test]
    fn test_complex_versioning_and_epochs() {
        use alpm::vercmp;
        use std::cmp::Ordering;

        // 1. Epoch superiority: 1:2.0 is newer than 2.1 (epoch 1 > epoch 0)
        assert_eq!(vercmp("1:2.0", "2.1"), Ordering::Greater);
        assert_eq!(vercmp("2:1.0", "1:9.9"), Ordering::Greater);
        assert_eq!(vercmp("1:1.0", "1:1.0"), Ordering::Equal);
        assert_eq!(vercmp("0:2.0", "2.0"), Ordering::Equal);
        assert_eq!(vercmp("1:0.1", "9.9"), Ordering::Greater);

        // 2. Pkgrel bumps
        assert_eq!(vercmp("1.2.3-2", "1.2.3-1"), Ordering::Greater);
        assert_eq!(vercmp("1.2.3-1.1", "1.2.3-1"), Ordering::Greater);
        assert_eq!(vercmp("1.2.3-10", "1.2.3-2"), Ordering::Greater);

        // 3. VCS Git tags and revisions
        assert_eq!(
            vercmp("0.1.r123.g456-1", "0.1.r120.g123-1"),
            Ordering::Greater
        );
        assert_eq!(vercmp("1.0.0.r5.ga1b2c3d-1", "1.0.0-1"), Ordering::Greater);

        // 4. Pre-releases and alphabetic ordering
        assert_eq!(vercmp("2.0.0", "2.0.0rc1"), Ordering::Greater);
        assert_eq!(vercmp("2.0.0beta2", "2.0.0beta1"), Ordering::Greater);
        assert_eq!(vercmp("2.0.0alpha", "2.0.0beta"), Ordering::Less);
    }
}
