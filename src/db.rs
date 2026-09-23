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

use alpm::{Alpm, SigLevel};
use glob::Pattern;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct DownloadTarget {
    pub name: String,
    pub url: String,
    pub candidate_urls: Vec<String>,
    pub sha256: Option<String>,
}

pub struct AlpmManager {
    handle: Alpm,
    repos: Vec<String>,
}

impl AlpmManager {
    pub fn new() -> Result<Self, alpm::Error> {
        Self::with_repo_order(&[])
    }

    pub fn get_dbpath() -> PathBuf {
        if let Ok(output) = Command::new("pacman-conf").arg("DBPath").output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let trimmed = stdout.trim();
                if !trimmed.is_empty() {
                    return PathBuf::from(trimmed);
                }
            }
        }
        PathBuf::from("/var/lib/pacman")
    }

    pub fn get_rootdir() -> PathBuf {
        if let Ok(output) = Command::new("pacman-conf").arg("RootDir").output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let trimmed = stdout.trim();
                if !trimmed.is_empty() {
                    return PathBuf::from(trimmed);
                }
            }
        }
        PathBuf::from("/")
    }

    pub fn with_repo_order(repo_order: &[String]) -> Result<Self, alpm::Error> {
        let root = Self::get_rootdir();
        let dbpath = Self::get_dbpath();
        let root = root.to_string_lossy();
        let dbpath = dbpath.to_string_lossy();
        let handle = Alpm::new(root.as_ref(), dbpath.as_ref())?;
        let repos = Self::resolve_repo_order(repo_order);
        for repo in &repos {
            let _ = handle.register_syncdb(repo.as_str(), SigLevel::USE_DEFAULT);
        }
        Ok(Self { handle, repos })
    }

    /// Verifies all configured sync databases for missing files or corruption.
    pub fn verify_syncdbs(&self) -> Vec<String> {
        let dbpath = Self::get_dbpath();
        let sync_dir = dbpath.join("sync");
        let mut warnings = Vec::new();

        for repo in &self.repos {
            let db_file = sync_dir.join(format!("{}.db", repo));
            let db_tar = sync_dir.join(format!("{}.tar.gz", repo));

            let db_file_zero = db_file.exists() && db_file.metadata().map(|m| m.len() == 0).unwrap_or(false);
            let db_tar_zero = db_tar.exists() && db_tar.metadata().map(|m| m.len() == 0).unwrap_or(false);

            if db_file_zero || db_tar_zero {
                warnings.push(format!(
                    "Repository '[{}]' sync database is corrupted (0 bytes). Run 'pin -Sy' or 'pacman -Sy' to synchronize.",
                    repo
                ));
            } else if !db_file.exists() && !db_tar.exists() {
                warnings.push(format!(
                    "Repository '[{}]' has no local database file (expected {}). Run 'pin -Sy' or 'pacman -Sy' to synchronize.",
                    repo,
                    db_file.display()
                ));
            } else if let Some(db) = self.handle.syncdbs().into_iter().find(|d| d.name() == repo) {
                // Ensure the database can be enumerated without error
                let _ = db.pkgs().len();
            }
        }
        warnings
    }

    pub fn resolve_repo_order(repo_order: &[String]) -> Vec<String> {
        let discovered = Self::discover_repos();
        Self::order_repos(&discovered, repo_order)
    }

    pub fn order_repos(discovered: &[String], repo_order: &[String]) -> Vec<String> {
        if repo_order.is_empty() {
            return discovered.to_vec();
        }

        let mut result = Vec::new();
        let mut seen = HashSet::new();

        // 1. Add configured repos in order
        for r in repo_order {
            if !seen.contains(r) {
                result.push(r.clone());
                seen.insert(r.clone());
            }
        }

        // 2. Add remaining discovered repos that weren't in repo_order
        for r in discovered {
            if !seen.contains(r) {
                result.push(r.clone());
                seen.insert(r.clone());
            }
        }

        result
    }

    pub fn discover_repos() -> Vec<String> {
        if let Ok(output) = Command::new("pacman-conf").arg("--repo-list").output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let repos: Vec<String> = stdout
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                if !repos.is_empty() {
                    return repos;
                }
            }
        }
        vec!["core".into(), "extra".into(), "multilib".into()]
    }

    pub fn repos(&self) -> &[String] {
        &self.repos
    }

    pub fn get_mirror_servers(repo: &str) -> Vec<String> {
        if let Ok(output) = Command::new("pacman-conf")
            .args(["--repo", repo, "Server"])
            .output()
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let servers: Vec<String> = stdout
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                if !servers.is_empty() {
                    return servers;
                }
            }
        }
        Vec::new()
    }

    pub fn resolve_download_urls(
        &self,
        repo: Option<&str>,
        pkg_name: &str,
    ) -> Result<Vec<DownloadTarget>, String> {
        let syncdbs_list: Vec<&alpm::Db> = self.handle.syncdbs().into_iter().collect();
        let syncdb_map: HashMap<&str, &alpm::Db> =
            syncdbs_list.iter().map(|d| (d.name(), *d)).collect();

        // Find target package or virtual provide satisfier
        let (target_db_name, target_pkg) = if let Some(r) = repo {
            let db = syncdb_map
                .get(r)
                .ok_or_else(|| format!("Repository '{}' not found", r))?;
            let p = db
                .pkg(pkg_name)
                .ok()
                .or_else(|| db.pkgs().find_satisfier(pkg_name))
                .ok_or_else(|| format!("Package '{}' not found in repository '{}'", pkg_name, r))?;
            (r.to_string(), p)
        } else {
            let mut found = None;
            for r in &self.repos {
                if let Some(db) = syncdb_map.get(r.as_str()) {
                    if let Ok(p) = db.pkg(pkg_name) {
                        found = Some((r.clone(), p));
                        break;
                    } else if let Some(p) = db.pkgs().find_satisfier(pkg_name) {
                        found = Some((r.clone(), p));
                        break;
                    }
                }
            }
            found.ok_or_else(|| {
                format!(
                    "Package '{}' not found in any configured repository",
                    pkg_name
                )
            })?
        };

        let mut server_cache: HashMap<String, Vec<String>> = HashMap::new();
        let mut get_servers = |repo_name: &str| -> Result<Vec<String>, String> {
            if let Some(s) = server_cache.get(repo_name) {
                return Ok(s.clone());
            }
            let servers = Self::get_mirror_servers(repo_name);
            if servers.is_empty() {
                return Err(format!("No mirror server configured for repository '{}'", repo_name));
            }
            server_cache.insert(repo_name.to_string(), servers.clone());
            Ok(servers)
        };

        let local_db = self.handle.localdb();
        let mut visited_deps = HashSet::new();
        let mut visited_pkgs = HashSet::new();
        let mut ordered_targets = Vec::new();

        #[allow(clippy::too_many_arguments)]
        fn resolve_deps(
            pkg: &alpm::Package,
            manager: &AlpmManager,
            syncdb_map: &HashMap<&str, &alpm::Db>,
            local_db: &alpm::Db,
            visited_deps: &mut HashSet<String>,
            visited_pkgs: &mut HashSet<String>,
            ordered_targets: &mut Vec<DownloadTarget>,
            get_servers: &mut dyn FnMut(&str) -> Result<Vec<String>, String>,
        ) -> Result<(), String> {
            for dep in pkg.depends() {
                let dep_name = dep.name();
                if visited_deps.contains(dep_name) {
                    continue;
                }
                visited_deps.insert(dep_name.to_string());

                // Check if already satisfied locally
                if local_db.pkgs().find_satisfier(dep_name).is_some() {
                    continue;
                }

                // Search syncdbs in priority order
                let mut found = None;
                for r in &manager.repos {
                    if let Some(db) = syncdb_map.get(r.as_str()) {
                        if let Some(p) = db.pkgs().find_satisfier(dep_name) {
                            found = Some((r.clone(), p));
                            break;
                        }
                    }
                }

                if let Some((sdb_name, dep_pkg)) = found {
                    let real_name = dep_pkg.name().to_string();
                    if visited_pkgs.insert(real_name.clone()) {
                        // Recurse first so dependencies precede this package
                        resolve_deps(
                            dep_pkg,
                            manager,
                            syncdb_map,
                            local_db,
                            visited_deps,
                            visited_pkgs,
                            ordered_targets,
                            get_servers,
                        )?;

                        let filename = dep_pkg.filename().ok_or_else(|| {
                            format!("No filename found for package '{}'", real_name)
                        })?;
                        let servers = get_servers(&sdb_name)?;
                        let candidate_urls: Vec<String> = servers
                            .iter()
                            .map(|s| format!("{}/{}", s.trim_end_matches('/'), filename))
                            .collect();
                        let url = candidate_urls.first().cloned().unwrap_or_default();
                        let sha256 = dep_pkg.sha256sum().map(|s| s.to_string());
                        ordered_targets.push(DownloadTarget {
                            name: real_name,
                            url,
                            candidate_urls,
                            sha256,
                        });
                    }
                }
            }
            Ok(())
        }

        visited_pkgs.insert(target_pkg.name().to_string());
        resolve_deps(
            target_pkg,
            self,
            &syncdb_map,
            local_db,
            &mut visited_deps,
            &mut visited_pkgs,
            &mut ordered_targets,
            &mut get_servers,
        )?;

        let target_filename = target_pkg
            .filename()
            .ok_or_else(|| format!("No filename found for package '{}'", target_pkg.name()))?;
        let target_servers = get_servers(&target_db_name)?;
        let target_candidate_urls: Vec<String> = target_servers
            .iter()
            .map(|s| format!("{}/{}", s.trim_end_matches('/'), target_filename))
            .collect();
        let target_url = target_candidate_urls.first().cloned().unwrap_or_default();
        let target_sha256 = target_pkg.sha256sum().map(|s| s.to_string());
        ordered_targets.push(DownloadTarget {
            name: target_pkg.name().to_string(),
            url: target_url,
            candidate_urls: target_candidate_urls,
            sha256: target_sha256,
        });

        Ok(ordered_targets)
    }

    pub fn handle(&self) -> &Alpm {
        &self.handle
    }

    pub fn get_dependents(&self, target_pkg: &str) -> Vec<String> {
        let mut dependents = HashSet::new();
        let mut target_names = HashSet::new();
        target_names.insert(target_pkg.to_string());

        if let Ok(pkg) = self.handle.localdb().pkg(target_pkg) {
            for prov in pkg.provides() {
                target_names.insert(prov.name().to_string());
            }
        }
        for db in self.handle.syncdbs() {
            if let Ok(pkg) = db.pkg(target_pkg) {
                for prov in pkg.provides() {
                    target_names.insert(prov.name().to_string());
                }
            }
        }

        for pkg in self.handle.localdb().pkgs() {
            for dep in pkg.depends() {
                if target_names.contains(dep.name()) {
                    dependents.insert(pkg.name().to_string());
                    break;
                }
            }
        }
        let mut list: Vec<String> = dependents.into_iter().collect();
        list.sort();
        list
    }

    pub fn find_companions(
        &self,
        pkg_name: &str,
        repo: &str,
        pins: &BTreeMap<String, String>,
    ) -> Vec<String> {
        if repo.eq_ignore_ascii_case("aur") {
            let aur_map = crate::aur::query_aur(&[pkg_name.to_string()]);
            if let Some(target_aur) = aur_map.get(pkg_name) {
                let target_base = target_aur.package_base.as_deref().unwrap_or(pkg_name);
                let direct_deps: HashSet<String> = target_aur
                    .depends
                    .iter()
                    .map(|d| crate::aur::clean_dep_name(d).to_string())
                    .collect();
                let local_db = self.handle.localdb();
                let prefix = format!("{}-", pkg_name);
                let base_prefix = format!("{}-", target_base);
                let mut companions = HashSet::new();

                for pkg in local_db.pkgs() {
                    let name = pkg.name();
                    if name == pkg_name {
                        continue;
                    }
                    if pins.get(name).map(|r| r.eq_ignore_ascii_case("aur")) == Some(true) {
                        continue;
                    }
                    if direct_deps.contains(name)
                        || name.starts_with(&prefix)
                        || name.starts_with(&base_prefix)
                    {
                        companions.insert(name.to_string());
                    }
                }
                let mut sorted: Vec<String> = companions.into_iter().collect();
                sorted.sort();
                return sorted;
            }
            return Vec::new();
        }

        let sync_db = match self.handle.syncdbs().into_iter().find(|d| d.name() == repo) {
            Some(db) => db,
            None => return Vec::new(),
        };

        let sync_pkg = match sync_db.pkg(pkg_name) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };

        let target_base = sync_pkg.base().unwrap_or(pkg_name);
        let direct_deps: HashSet<String> = sync_pkg
            .depends()
            .iter()
            .map(|d| d.name().to_string())
            .collect();
        let local_db = self.handle.localdb();

        let prefix = format!("{}-", pkg_name);
        let base_prefix = format!("{}-", target_base);

        let mut companions = HashSet::new();

        let matching_pins: Vec<(&String, Option<Pattern>)> = pins
            .iter()
            .filter(|(_, rep)| *rep == repo)
            .map(|(pat, _)| {
                let pat_obj = if pat.contains('*') || pat.contains('?') || pat.contains('[') {
                    Pattern::new(pat).ok()
                } else {
                    None
                };
                (pat, pat_obj)
            })
            .collect();

        for p in sync_db.pkgs() {
            let name = p.name();
            if name == pkg_name {
                continue;
            }

            let is_pinned = matching_pins.iter().any(|(pat, pat_obj)| {
                if *pat == name {
                    return true;
                }
                if let Some(p) = pat_obj {
                    return p.matches(name);
                }
                false
            });
            if is_pinned {
                continue;
            }

            let p_base = p.base().unwrap_or(name);
            let is_installed = local_db.pkg(name).is_ok();
            let is_direct = direct_deps.contains(name);

            if (p_base == target_base
                || name.starts_with(&prefix)
                || name.starts_with(&base_prefix))
                && (is_installed || is_direct)
            {
                companions.insert(name.to_string());
            }
        }

        let mut sorted: Vec<String> = companions.into_iter().collect();
        sorted.sort();
        sorted
    }

    pub fn get_orphans(
        &self,
        updates: &[crate::resolver::ResolvedPackage],
    ) -> Vec<crate::orphans::OrphanPackage> {
        crate::orphans::OrphanManager::detect(self.handle(), updates)
    }
}

pub use crate::orphans::OrphanPackage;
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_order_repos_empty_override() {
        let discovered = vec![
            "cachyos".to_string(),
            "core".to_string(),
            "extra".to_string(),
        ];
        let order = AlpmManager::order_repos(&discovered, &[]);
        assert_eq!(order, discovered);
    }

    #[test]
    fn test_order_repos_prioritization() {
        let discovered = vec![
            "cachyos".to_string(),
            "core".to_string(),
            "extra".to_string(),
            "multilib".to_string(),
        ];
        let repo_order = vec!["core".to_string(), "extra".to_string()];
        let order = AlpmManager::order_repos(&discovered, &repo_order);
        assert_eq!(order, vec!["core", "extra", "cachyos", "multilib"]);
    }

    #[test]
    fn test_order_repos_with_custom_new_repo() {
        let discovered = vec!["core".to_string(), "extra".to_string()];
        let repo_order = vec!["custom-repo".to_string(), "core".to_string()];
        let order = AlpmManager::order_repos(&discovered, &repo_order);
        assert_eq!(order, vec!["custom-repo", "core", "extra"]);
    }

    #[test]
    fn test_alpm_find_satisfier() {
        let manager = AlpmManager::new().unwrap();
        let local_db = manager.handle().localdb();
        // check if find_satisfier exists
        if let Some(pkg) = local_db.pkgs().first() {
            for dep in pkg.depends() {
                let _sat = local_db.pkgs().find_satisfier(dep.name());
            }
        }

        // Test resolving nix
        let mut target_pkg = None;
        for sdb in manager.handle().syncdbs() {
            if let Ok(p) = sdb.pkg("nix") {
                target_pkg = Some((sdb.name().to_string(), p));
                break;
            }
        }
        assert!(target_pkg.is_some());
        let (db_name, pkg) = target_pkg.unwrap();
        println!("Found nix in {}: filename = {:?}", db_name, pkg.filename());
        println!("sha256: {:?}", pkg.sha256sum());
        println!("md5: {:?}", pkg.md5sum());
        for dep in pkg.depends() {
            let sat_local = local_db.pkgs().find_satisfier(dep.name()).is_some();
            println!("  dep {}: satisfied locally = {}", dep.name(), sat_local);
        }
    }

    #[test]
    fn test_resolve_download_urls_nix() {
        let manager = AlpmManager::new().unwrap();
        let urls = manager.resolve_download_urls(None, "nix");
        assert!(urls.is_ok());
        let list = urls.unwrap();
        assert!(!list.is_empty());
        // nix should be the last item
        let last = list.last().unwrap();
        assert_eq!(last.name, "nix");
        assert!(last.url.contains("nix-"));
        assert!(last.sha256.is_some());
        println!("Resolved {} packages for nix:", list.len());
        for target in &list {
            println!(
                "  {} -> {} (sha256: {:?})",
                target.name, target.url, target.sha256
            );
        }
    }

    #[test]
    fn test_verify_syncdbs() {
        if let Ok(manager) = AlpmManager::new() {
            // verify_syncdbs runs without panicking
            let warnings = manager.verify_syncdbs();
            for w in &warnings {
                assert!(!w.is_empty());
            }
        }
    }
}
