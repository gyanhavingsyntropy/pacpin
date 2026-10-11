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
    fn pending_file() -> Result<PathBuf, String> {
        Ok(TransactionJournal::state_dir()?.join("pending_orphan_cleanup.json"))
    }

    fn read_pending(path: &std::path::Path) -> Result<Option<Vec<String>>, String> {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("Cannot read pending orphan cleanup: {error}")),
        };
        let selected: Vec<String> = serde_json::from_str(&content)
            .map_err(|error| format!("Invalid pending orphan cleanup: {error}"))?;
        if selected.iter().any(|name| !crate::utils::is_safe_pkg_name(name)) {
            return Err("Invalid package name in pending orphan cleanup".into());
        }
        Ok(Some(selected))
    }

    fn write_pending(path: &std::path::Path, selected: &[String]) -> Result<(), String> {
        use std::io::Write;
        use std::os::fd::FromRawFd;
        use std::os::unix::ffi::OsStrExt;
        let write = || -> Result<(), std::io::Error> {
            let data = serde_json::to_vec(selected).map_err(std::io::Error::other)?;
            let parent = path.parent().ok_or_else(|| std::io::Error::other("Missing state directory"))?;
            fs::create_dir_all(parent)?;
            let mut template = parent.join(".pending-orphans-XXXXXX").as_os_str().as_bytes().to_vec();
            template.push(0);
            let fd = unsafe { libc::mkstemp(template.as_mut_ptr().cast()) };
            if fd < 0 { return Err(std::io::Error::last_os_error()); }
            let temp_path = PathBuf::from(std::ffi::OsStr::from_bytes(&template[..template.len() - 1]));
            let mut file = unsafe { fs::File::from_raw_fd(fd) };
            let result = file.write_all(&data).and_then(|()| file.sync_all());
            drop(file);
            if let Err(error) = result.and_then(|()| fs::rename(&temp_path, path)) {
                let _ = fs::remove_file(&temp_path);
                return Err(error);
            }
            fs::File::open(parent)?.sync_all()
        };
        write().map_err(|error| format!("Cannot save pending orphan cleanup: {error}"))
    }

    fn pending_needs_review(current: &[OrphanPackage], pending: Option<&[String]>) -> bool {
        // Older versions remembered inventory but not unfinished cleanup. Offer
        // one fresh review when introducing the separate completion record.
        match pending {
            None => !current.is_empty(),
            Some(pending) => current.iter().any(|o| pending.contains(&o.name)),
        }
    }

    pub fn needs_review(current: &[OrphanPackage]) -> bool {
        let pending = match Self::pending_file().and_then(|path| Self::read_pending(&path)) {
            Ok(pending) => pending,
            Err(error) => {
                eprintln!(":: {error}; orphan cleanup needs a fresh review.");
                None
            }
        };
        Self::has_changed(current) || Self::pending_needs_review(current, pending.as_deref())
    }

    pub fn record_selection(selected: &[String]) -> Result<(), String> {
        if selected.iter().any(|name| !crate::utils::is_safe_pkg_name(name)) {
            return Err("Refusing unsafe pending orphan selection".into());
        }
        Self::write_pending(&Self::pending_file()?, selected)
    }

    fn finish_pending(path: &std::path::Path, remaining: &[OrphanPackage]) -> Result<(), String> {
        if let Some(mut pending) = Self::read_pending(path)? {
            pending.retain(|name| remaining.iter().any(|o| o.name == *name));
            Self::write_pending(path, &pending)?;
        }
        Ok(())
    }

    pub fn finish_review(remaining: &[OrphanPackage]) -> bool {
        match Self::pending_file().and_then(|path| Self::finish_pending(&path, remaining)) {
            Ok(()) => true,
            Err(error) => { eprintln!(":: {error}"); false }
        }
    }

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

        // 1. Identify all packages that are reachable from explicitly installed packages.
        // Packages installed as dependencies that cannot be reached from any explicit package
        // are orphaned (including cyclic required dependency clusters).
        let mut reachable_from_explicit: HashSet<String> = HashSet::new();
        let mut queue: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        let mut satisfier_cache: HashMap<String, Option<String>> = HashMap::new();

        for pkg in local.pkgs() {
            if pkg.reason() == alpm::PackageReason::Explicit {
                let name = pkg.name().to_string();
                if reachable_from_explicit.insert(name.clone()) {
                    queue.push_back(name);
                }
            }
        }

        while let Some(pkg_name) = queue.pop_front() {
            if let Ok(pkg) = local.pkg(pkg_name.as_str()) {
                for dep in pkg.depends() {
                    // ALPM checks version constraints and all virtual providers here.
                    let sat_name = satisfier_cache
                        .entry(dep.to_string())
                        .or_insert_with_key(|spec| {
                            local.pkgs().find_satisfier(spec.as_str()).map(|p| p.name().to_string())
                        })
                        .clone();
                    if let Some(sat_name) = sat_name {
                        if reachable_from_explicit.insert(sat_name.clone()) {
                            queue.push_back(sat_name);
                        }
                    }
                }
            }
        }

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

            if !reachable_from_explicit.contains(name) {
                // Not reachable by any explicitly installed package -> genuine orphan
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
                        let provides_matched = pkg
                            .provides()
                            .iter()
                            .any(|p| new_deps.contains(p.name()));
                        if new_deps.contains(name) || provides_matched {
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

        Self::prune_orphan_cycles(&mut orphans);

        orphans.sort_by(|a, b| {
            let a_opt = !a.optional_for.is_empty();
            let b_opt = !b.optional_for.is_empty();
            a_opt.cmp(&b_opt).then_with(|| a.name.cmp(&b.name))
        });

        orphans
    }

    /// Prunes optional parent references that are part of isolated cycles or dead references.
    /// Preserves transitive chains to active, non-orphan packages while breaking mutual
    /// cyclic optional dependencies (e.g. A <-> B or A -> B -> C -> A) so that orphans
    /// without active non-orphan parents become pure orphans.
    pub fn prune_orphan_cycles(orphans: &mut [OrphanPackage]) {
        let orphan_names: HashSet<String> = orphans.iter().map(|o| o.name.clone()).collect();
        let mut active_packages: HashSet<String> = HashSet::new();

        // 1. Any package listed in optional_for that is NOT in the orphan set is an active parent.
        for o in orphans.iter() {
            for p in &o.optional_for {
                if !orphan_names.contains(p) {
                    active_packages.insert(p.clone());
                }
            }
        }

        // 2. Transitively propagate active status through the orphan graph.
        // If orphan A is optional for orphan B, and B has an active parent path, A also has an active parent path.
        loop {
            let mut added = false;
            for o in orphans.iter() {
                if !active_packages.contains(&o.name)
                    && o.optional_for.iter().any(|p| active_packages.contains(p))
                {
                    active_packages.insert(o.name.clone());
                    added = true;
                }
            }
            if !added {
                break;
            }
        }

        // 3. Retain only parents that reach an active package.
        // Isolated cycles and dependencies on dead packages are pruned away.
        for o in orphans.iter_mut() {
            o.optional_for.retain(|p| active_packages.contains(p));
        }
    }

    pub fn has_changed(current: &[OrphanPackage]) -> bool {
        Self::has_changed_from_known(current, Self::load_known().as_ref())
    }

    fn has_changed_from_known(
        current: &[OrphanPackage],
        known: Option<&HashMap<String, KnownOrphan>>,
    ) -> bool {
        match known {
            Some(known) => current.iter().any(|o| match known.get(&o.name) {
                None => true,
                Some(record) => record.is_pure != o.is_pure(),
            }),
            None => !current.is_empty(),
        }
    }

    /// Retain providers needed by packages outside the removal set, including
    /// their transitive dependencies. ALPM owns version/provides semantics.
    fn safe_selection(handle: &Alpm, selected: &[String]) -> Result<Vec<String>, String> {
        let installed = handle.localdb().pkgs().iter().collect::<Vec<_>>();
        let mut safe = selected.iter().filter(|name| installed.iter().any(|pkg| pkg.name() == name.as_str()))
            .cloned().collect::<Vec<_>>();
        let mut seen = HashSet::new();
        safe.retain(|name| seen.insert(name.clone()));
        loop {
            let removing = installed.iter().filter(|pkg| safe.iter().any(|name| name == pkg.name())).copied().collect::<Vec<_>>();
            let missing = handle.check_deps(installed.iter(), removing.iter(), alpm::AlpmListMut::<&alpm::Pkg>::new(), true);
            if missing.is_empty() { return Ok(safe); }
            let mut retained = Vec::new();
            for dependency in missing.iter() {
                let Some(provider) = dependency.causing_pkg().filter(|name| safe.iter().any(|selected| selected == name)) else {
                    return Err(format!("Cannot safely prepare orphan removal: {} requires {}", dependency.target(), dependency.depend()));
                };
                for line in crate::ui::wrap_field(&format!(":: Keeping {provider}: required by retained {}", dependency.target()), "", "  ", crate::ui::get_terminal_width()) {
                    println!("{line}");
                }
                retained.push(provider.to_owned());
            }
            safe.retain(|name| !retained.contains(name));
        }
    }

    pub fn execute_removal(selected: &[String], privilege_tool: &str) -> Result<Vec<String>, String> {
        if selected.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(target) = selected.iter().find(|name| !crate::utils::is_safe_pkg_name(name)) {
            return Err(format!("Refusing unsafe orphan target '{target}'"));
        }

        let manager = crate::db::AlpmManager::new().map_err(|error| format!("Cannot recheck orphan dependencies: {error}"))?;
        let selected = crate::progress::operation(crate::progress::Manager::Checks, "Checking removal dependencies", || Self::safe_selection(manager.handle(), selected))?;
        Self::record_selection(&selected)?;
        if selected.is_empty() {
            println!(":: No selected orphans can be removed without removing a retained package.");
            return Ok(selected);
        }

        println!(
            "\n{} {}",
            ":: Removing selected orphaned package(s):".cyan(),
            selected.join(" ").bold()
        );

        let remove_cmd = format!("{privilege_tool} pacman -Rn --noconfirm -- {}", selected.join(" "));
        crate::pm::remove_orphans(&selected, privilege_tool)?;
        let tx_packages: Vec<serde_json::Value> = selected
            .iter()
            .map(|name| serde_json::json!({ "name": name, "removed": true }))
            .collect();
        let _ = TransactionJournal::record_transaction("remove_orphans", tx_packages, &remove_cmd);
        println!("\n{}", format!("✔ Removed {} orphaned package(s) successfully.", selected.len()).green());
        Ok(selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removal_keeps_dependencies_of_retained_optional_parent_and_their_dependencies() {
        use crate::db::test_support::{AlpmFixture, FixturePackage as P};
        let fixture = AlpmFixture::new(&[
            P::with_depends("evince", "1-1", &["gspell>=2", "libsynctex"]),
            P::with_depends("gspell", "2-1", &["spell-backend"]),
            P::new("libsynctex", "1-1"), P::new("spell-backend", "1-1"), P::new("ninja", "1-1"),
        ], &[]);
        let names = ["gspell", "libsynctex", "spell-backend", "ninja"].map(str::to_owned);
        assert_eq!(OrphanManager::safe_selection(fixture.manager.handle(), &names).unwrap(), ["ninja"]);
        let all = ["evince", "gspell", "libsynctex", "spell-backend", "ninja"].map(str::to_owned);
        assert_eq!(OrphanManager::safe_selection(fixture.manager.handle(), &all).unwrap(), all);
    }

    #[test]
    fn removal_respects_versioned_virtual_providers_and_remaining_provider() {
        use crate::db::test_support::{AlpmFixture, FixturePackage as P};
        let mut provider = P::new("virtual-spell", "1-1");
        provider.provides = &["gspell=2"];
        let fixture = AlpmFixture::new(&[P::with_depends("evince", "1-1", &["gspell>=2"]), provider, P::new("gspell", "2-1")], &[]);
        assert_eq!(OrphanManager::safe_selection(fixture.manager.handle(), &["gspell".into()]).unwrap(), ["gspell"]);
        let safe = OrphanManager::safe_selection(fixture.manager.handle(), &["gspell".into(), "virtual-spell".into()]).unwrap();
        assert_eq!(safe.len(), 1, "one provider can be removed when the other is retained");
        let mut too_old = P::new("old-spell", "1-1");
        too_old.provides = &["gspell=1"];
        let fixture = AlpmFixture::new(&[P::with_depends("evince", "1-1", &["gspell>=2"]), too_old, P::new("gspell", "2-1")], &[]);
        assert!(OrphanManager::safe_selection(fixture.manager.handle(), &["gspell".into()]).unwrap().is_empty());
    }

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
        let has_change = OrphanManager::has_changed_from_known(&current_identical, Some(&known));
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
        let has_change_converted = OrphanManager::has_changed_from_known(&current_converted, Some(&known));
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
        let has_change_new = OrphanManager::has_changed_from_known(&current_new_orphan, Some(&known));
        assert!(has_change_new);
    }

    #[test]
    fn test_multi_step_optional_parent_transitions() {
        let mut known = HashMap::from([(
            "plugin".to_string(),
            KnownOrphan { is_pure: false, optional_for: vec!["parent-a".into(), "parent-b".into()] },
        )]);
        let mut plugin = OrphanPackage {
            name: "plugin".into(), version: "1".into(), isize: 1, desc: String::new(),
            is_projected: false, dropped_by: Vec::new(),
            optional_for: vec!["parent-b".into()],
        };
        assert!(!OrphanManager::has_changed_from_known(&[plugin.clone()], Some(&known)));
        plugin.optional_for.clear();
        assert!(OrphanManager::has_changed_from_known(&[plugin.clone()], Some(&known)));
        known.get_mut("plugin").unwrap().is_pure = true;
        assert!(!OrphanManager::has_changed_from_known(&[plugin.clone()], Some(&known)));
        plugin.optional_for.push("parent-c".into());
        assert!(OrphanManager::has_changed_from_known(&[plugin], Some(&known)));
    }

    fn retry_orphan(name: &str) -> OrphanPackage {
        OrphanPackage {
            name: name.into(), version: "1".into(), isize: 1, desc: String::new(),
            is_projected: false, dropped_by: Vec::new(), optional_for: Vec::new(),
        }
    }

    #[test]
    fn pending_cleanup_requires_review_even_when_inventory_is_known() {
        let orphan = retry_orphan("meson");
        let known = HashMap::from([("meson".into(), KnownOrphan { is_pure: true, optional_for: Vec::new() })]);
        assert!(!OrphanManager::has_changed_from_known(std::slice::from_ref(&orphan), Some(&known)));
        assert!(OrphanManager::pending_needs_review(&[orphan], Some(&["meson".into()])));
    }

    #[test]
    fn legacy_inventory_gets_one_fresh_review() {
        assert!(OrphanManager::pending_needs_review(&[retry_orphan("meson")], None));
        assert!(!OrphanManager::pending_needs_review(&[], None));
    }

    #[test]
    fn explicit_decline_does_not_repeat_unchanged_review() {
        assert!(!OrphanManager::pending_needs_review(&[retry_orphan("meson")], Some(&[])));
    }

    #[test]
    fn stale_pending_names_do_not_prompt_or_authorize_removal() {
        assert!(!OrphanManager::pending_needs_review(&[retry_orphan("meson")], Some(&["no-longer-orphan".into()])));
    }

    #[test]
    fn pending_cleanup_is_atomic_and_kept_after_failed_removal() {
        let root = std::env::temp_dir().join(format!("pacpin-pending-orphans-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("pending.json");
        OrphanManager::write_pending(&path, &["meson".into(), "ninja".into()]).unwrap();
        let unchanged = [retry_orphan("meson"), retry_orphan("ninja")];
        OrphanManager::finish_pending(&path, &unchanged).unwrap();
        assert_eq!(OrphanManager::read_pending(&path).unwrap().unwrap(), ["meson", "ninja"]);
        OrphanManager::finish_pending(&path, &[retry_orphan("meson")]).unwrap();
        assert_eq!(OrphanManager::read_pending(&path).unwrap().unwrap(), ["meson"]);
        OrphanManager::finish_pending(&path, &[]).unwrap();
        assert_eq!(OrphanManager::read_pending(&path).unwrap(), Some(Vec::new()));
        assert!(OrphanManager::write_pending(&root, &["meson".into()]).is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1, "failed writes must clean their temporary file");
        fs::write(&path, "[\"../bad\"]").unwrap();
        assert!(OrphanManager::read_pending(&path).is_err());
        fs::write(&path, "truncated").unwrap();
        assert!(OrphanManager::read_pending(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_cyclic_optional_orphans() {
        // Mutual dependency: pkg A is optional for B, and pkg B is optional for A.
        // Both are orphans with no active parent, so both must be resolved to pure orphans.
        let mut orphans = vec![
            OrphanPackage {
                name: "pkg-a".to_string(),
                version: "1.0".to_string(),
                isize: 100,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: vec!["pkg-b".to_string()],
            },
            OrphanPackage {
                name: "pkg-b".to_string(),
                version: "1.0".to_string(),
                isize: 100,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: vec!["pkg-a".to_string()],
            },
        ];

        assert!(!orphans[0].is_pure());
        assert!(!orphans[1].is_pure());

        OrphanManager::prune_orphan_cycles(&mut orphans);

        assert!(orphans[0].is_pure());
        assert!(orphans[1].is_pure());
        assert!(orphans[0].optional_for.is_empty());
        assert!(orphans[1].optional_for.is_empty());
    }

    #[test]
    fn test_multi_parent_optional_retention() {
        // Package X is optional for ParentA and ParentB.
        // If ParentA is uninstalled or orphaned, but ParentB is a live installed package,
        // X must retain ParentB and must NOT be falsely promoted to pure.
        let mut orphans = vec![
            OrphanPackage {
                name: "plugin-x".to_string(),
                version: "1.0".to_string(),
                isize: 100,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: vec!["parent-b".to_string()],
            },
        ];

        OrphanManager::prune_orphan_cycles(&mut orphans);

        assert!(!orphans[0].is_pure());
        assert_eq!(orphans[0].optional_for, vec!["parent-b".to_string()]);
    }

    #[test]
    fn test_3_node_cyclic_optional_orphans() {
        // 3-node cycle: A -> B -> C -> A with no active parent outside the cycle.
        // All 3 must be detected and pruned into pure orphans.
        let mut orphans = vec![
            OrphanPackage {
                name: "pkg-a".to_string(),
                version: "1.0".to_string(),
                isize: 100,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: vec!["pkg-b".to_string()],
            },
            OrphanPackage {
                name: "pkg-b".to_string(),
                version: "1.0".to_string(),
                isize: 100,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: vec!["pkg-c".to_string()],
            },
            OrphanPackage {
                name: "pkg-c".to_string(),
                version: "1.0".to_string(),
                isize: 100,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: vec!["pkg-a".to_string()],
            },
        ];

        OrphanManager::prune_orphan_cycles(&mut orphans);

        assert!(orphans[0].is_pure());
        assert!(orphans[1].is_pure());
        assert!(orphans[2].is_pure());
        assert!(orphans[0].optional_for.is_empty());
        assert!(orphans[1].optional_for.is_empty());
        assert!(orphans[2].optional_for.is_empty());
    }

    #[test]
    fn test_transitive_active_optional_retention() {
        // Transitive chain: C is optional for B, and B is optional for active non-orphan A.
        // Both B and C are in the orphan set, but B is kept for A, and C is kept for B.
        // Neither must be falsely promoted to pure orphan!
        let mut orphans = vec![
            OrphanPackage {
                name: "plugin-b".to_string(),
                version: "1.0".to_string(),
                isize: 100,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: vec!["active-app-a".to_string()],
            },
            OrphanPackage {
                name: "plugin-c".to_string(),
                version: "1.0".to_string(),
                isize: 100,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: vec!["plugin-b".to_string()],
            },
        ];

        OrphanManager::prune_orphan_cycles(&mut orphans);

        assert!(!orphans[0].is_pure());
        assert!(!orphans[1].is_pure());
        assert_eq!(orphans[0].optional_for, vec!["active-app-a".to_string()]);
        assert_eq!(orphans[1].optional_for, vec!["plugin-b".to_string()]);
    }

    #[test]
    fn test_multi_parent_with_dead_cycle_and_live_parent() {
        // Package X is optional for dead cycle node 'cycle-a' AND live installed app 'vlc'.
        // 'cycle-a' should be pruned, while 'vlc' is retained.
        let mut orphans = vec![
            OrphanPackage {
                name: "cycle-a".to_string(),
                version: "1.0".to_string(),
                isize: 100,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: vec!["cycle-b".to_string()],
            },
            OrphanPackage {
                name: "cycle-b".to_string(),
                version: "1.0".to_string(),
                isize: 100,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: vec!["cycle-a".to_string()],
            },
            OrphanPackage {
                name: "plugin-x".to_string(),
                version: "1.0".to_string(),
                isize: 100,
                desc: "".to_string(),
                is_projected: false,
                dropped_by: Vec::new(),
                optional_for: vec!["cycle-a".to_string(), "vlc".to_string()],
            },
        ];

        OrphanManager::prune_orphan_cycles(&mut orphans);

        assert!(orphans[0].is_pure());
        assert!(orphans[1].is_pure());
        assert!(!orphans[2].is_pure());
        assert_eq!(orphans[2].optional_for, vec!["vlc".to_string()]);
    }
}
