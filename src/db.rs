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
use std::collections::{BTreeMap, HashSet};

use std::process::Command;

pub struct AlpmManager {
    handle: Alpm,
    repos: Vec<String>,
}

impl AlpmManager {
    pub fn new() -> Result<Self, alpm::Error> {
        Self::with_repo_order(&[])
    }

    pub fn with_repo_order(repo_order: &[String]) -> Result<Self, alpm::Error> {
        let handle = Alpm::new("/", "/var/lib/pacman")?;
        let repos = Self::resolve_repo_order(repo_order);
        for repo in &repos {
            let _ = handle.register_syncdb(repo.as_str(), SigLevel::USE_DEFAULT);
        }
        Ok(Self { handle, repos })
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

    pub fn handle(&self) -> &Alpm {
        &self.handle
    }

    pub fn get_dependents(&self, target_pkg: &str) -> Vec<String> {
        let mut dependents = HashSet::new();
        for pkg in self.handle.localdb().pkgs() {
            for dep in pkg.depends() {
                if dep.name() == target_pkg {

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
        let sync_db = match self.handle.syncdbs().into_iter().find(|d| d.name() == repo) {
            Some(db) => db,
            None => return Vec::new(),
        };

        let sync_pkg = match sync_db.pkg(pkg_name) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };

        let target_base = sync_pkg.base().unwrap_or(pkg_name);
        let direct_deps: HashSet<String> =
            sync_pkg.depends().iter().map(|d| d.name().to_string()).collect();
        let local_db = self.handle.localdb();

        let prefix = format!("{}-", pkg_name);
        let base_prefix = format!("{}-", target_base);

        let mut companions = HashSet::new();

        for p in sync_db.pkgs() {
            let name = p.name();
            if name == pkg_name {
                continue;
            }

            let is_pinned = pins.iter().any(|(pat, rep)| {
                if rep != repo {
                    return false;
                }
                if pat == name {
                    return true;
                }
                Pattern::new(pat).map(|pat_obj| pat_obj.matches(name)).unwrap_or(false)
            });
            if is_pinned {
                continue;
            }

            let p_base = p.base().unwrap_or(name);
            let is_installed = local_db.pkg(name).is_ok();
            let is_direct = direct_deps.contains(name);

            if p_base == target_base && (is_installed || is_direct) {
                companions.insert(name.to_string());
            } else if (name.starts_with(&prefix) || name.starts_with(&base_prefix))
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
        let discovered = vec!["cachyos".to_string(), "core".to_string(), "extra".to_string()];
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
}

