// pacpin - Native transaction preview for upgrade approvals.
// Copyright (C) 2026 Gyan <330976822+gyanhavingsyntropy@users.noreply.github.com>
//
// Prepares a non-committing ALPM transaction from the exact native target set
// pacpin is about to execute, so dependency-expanded additions, removals, and
// replacements are visible BEFORE approval instead of appearing as a surprise
// inside pacman's own summary under --noconfirm.

use crate::db::AlpmManager;
use crate::db::find_sync_package;
use alpm::TransFlag;
use std::collections::HashSet;

/// One package the prepared native transaction will add (upgrade or new install).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewPackage {
    pub name: String,
    pub version: String,
    pub repo: String,
    pub old_version: Option<String>,
    pub new_isize: i64,
    pub old_isize: i64,
    pub download_size: i64,
    pub filename: String,
}

impl PreviewPackage {
    pub fn is_new_install(&self) -> bool {
        self.old_version.is_none()
    }

    pub fn net_delta(&self) -> i64 {
        self.new_isize.saturating_sub(self.old_isize)
    }
}

/// Read-only result of preparing the native transaction: additions (upgrades,
/// explicit targets, and solver-pulled dependencies) plus removals.
#[derive(Debug, Clone, Default)]
pub struct TransactionPreview {
    pub adds: Vec<PreviewPackage>,
    pub removes: Vec<(String, String)>,
    pub removed_isize: i64,
}

impl TransactionPreview {
    /// Stable identity of the prepared plan, used to detect plan changes
    /// between approval and execution.
    pub fn signature(&self) -> Vec<(String, String)> {
        let mut sig: Vec<(String, String)> = self
            .adds
            .iter()
            .map(|p| (format!("add:{}/{}:{}:{}:{}", p.repo, p.name, p.filename, p.new_isize, p.download_size), p.version.clone()))
            .chain(self.removes.iter().map(|(name, version)| (format!("remove:{name}"), version.clone())))
            .collect();
        sig.sort();
        sig
    }
}

/// Explicitly requested package shown in its own approval box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedStatus {
    NewInstall,
    Upgrade,
    Downgrade,
    AlreadyCurrent,
}

#[derive(Debug, Clone)]
pub struct RequestedItem {
    pub name: String,
    pub manager: String,
    pub repo: Option<String>,
    pub installed_version: Option<String>,
    pub candidate_version: String,
    pub status: RequestedStatus,
}

/// Strips an optional `repo/` qualifier down to the bare package name.
pub fn strip_repo(target: &str) -> &str {
    target.split('/').next_back().unwrap_or(target)
}

/// Prepares (but never commits) the native transaction for `targets` and
/// returns the solver-expanded add/remove sets. Uses a lock-free transaction on
/// the already-registered databases: no refresh, no download, no install.
pub fn prepare_native_preview(
    manager: &mut AlpmManager,
    targets: &[String],
) -> Result<TransactionPreview, String> {
    prepare_native_preview_with_needed(manager, targets, true)
}

/// Match an install/rebuild's reinstall policy as well as its target identities.
pub(crate) fn prepare_native_preview_with_needed(
    manager: &mut AlpmManager,
    targets: &[String],
    needed: bool,
) -> Result<TransactionPreview, String> {
    let alpm = manager.handle_mut();
    let flags = if needed { TransFlag::NO_LOCK | TransFlag::NEEDED } else { TransFlag::NO_LOCK };
    alpm.trans_init(flags)
        .map_err(|e| format!("Cannot start transaction preview: {e}"))?;
    let result = (|| {
        let mut staged_names: HashSet<String> = HashSet::new();
        for target in targets {
            let pkg = find_sync_package(alpm, target)?;
            staged_names.insert(pkg.name().to_string());
            alpm.trans_add_pkg(pkg)
                .map_err(|e| format!("Cannot stage '{target}' for preview: {}", e.error))?;
        }
        // Mirror pacman's replacement handling: when a staged package
        // replaces an installed one that is not itself being upgraded, stage
        // its removal so prepare resolves the same way pacman would after
        // the user accepts the replacement. Anything still conflicting
        // after this fails closed below, exactly like a pacman abort.
        {
            let local = alpm.localdb();
            let mut replaced: Vec<String> = Vec::new();
            for target in targets {
                let pkg = find_sync_package(alpm, target)?;
                for dep in pkg.replaces() {
                    let name = dep.name().to_string();
                    if staged_names.contains(&name) || replaced.iter().any(|n| n == &name) {
                        continue;
                    }
                    if local.pkgs().find_satisfier(dep.to_string())
                        .is_some_and(|installed| installed.name() == name)
                    {
                        replaced.push(name);
                    }
                }
            }
            for name in &replaced {
                if let Ok(local_pkg) = local.pkg(name.as_str()) {
                    alpm.trans_remove_pkg(local_pkg).map_err(|e| {
                        format!("Cannot stage removal of '{name}' for preview: {e}")
                    })?;
                }
            }
        }
        alpm.trans_prepare().map_err(|error| {
            let details: Vec<String> = match error.data() {
                Some(alpm::PrepareData::ConflictingDeps(conflicts)) => conflicts
                    .into_iter()
                    .map(|conflict| format!("{} conflicts with {} ({})",
                        conflict.package1().name(), conflict.package2().name(), conflict.reason()))
                    .collect(),
                Some(alpm::PrepareData::UnsatisfiedDeps(dependencies)) => dependencies
                    .into_iter()
                    .map(|missing| {
                        let mut message = format!("{} requires {}", missing.target(), missing.depend());
                        if let Some(cause) = missing.causing_pkg() {
                            message.push_str(&format!(" (affected by {cause})"));
                        }
                        message
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let mut message = format!("Transaction preview failed: {error}");
            for detail in details { message.push_str(&format!("\n  {detail}")); }
            message
        })?;

        let local = alpm.localdb();
        let mut adds = Vec::new();
        for pkg in alpm.trans_add() {
            let name = pkg.name().to_string();
            let installed = local.pkg(name.as_str()).ok();
            adds.push(PreviewPackage {
                name,
                version: pkg.version().to_string(),
                repo: pkg.db().map(|db| db.name().to_string()).unwrap_or_default(),
                old_version: installed.map(|p| p.version().to_string()),
                new_isize: pkg.isize().max(0),
                old_isize: installed.map(|p| p.isize().max(0)).unwrap_or(0),
                download_size: pkg.size().max(0),
                filename: pkg.filename().unwrap_or_default().to_string(),
            });
        }
        let mut removes = Vec::new();
        let mut removed_isize: i64 = 0;
        for pkg in alpm.trans_remove() {
            removed_isize = removed_isize.saturating_add(pkg.isize().max(0));
            removes.push((pkg.name().to_string(), pkg.version().to_string()));
        }
        Ok(TransactionPreview { adds, removes, removed_isize })
    })();
    let _ = alpm.trans_release();
    result
}

/// Splits prepared additions into `(explicit_or_updates, new_dependencies)`:
/// anything already installed or explicitly requested stays out of the
/// "new dependencies" box.
pub fn partition_new_dependencies<'a>(
    preview: &'a TransactionPreview,
    explicit_names: &std::collections::HashSet<String>,
) -> Vec<&'a PreviewPackage> {
    preview
        .adds
        .iter()
        .filter(|p| p.is_new_install() && !explicit_names.contains(&p.name))
        .collect()
}

const NATIVE_RESERVE: u64 = 128 * 1024 * 1024;

// Positive growth can accumulate before a shrinking package is processed.
// Reserve an additional largest-package rewrite, rather than assuming files
// overwrite in place (notably on Btrfs). This is an estimate, not a guarantee
// about hooks, quotas, snapshots or separate /usr and /var mounts.
fn native_peak(preview: &TransactionPreview) -> u64 {
    let growth = preview.adds.iter().fold(0u64, |sum, p| sum.saturating_add(p.net_delta().max(0) as u64));
    let rewrite = preview.adds.iter().map(|p| p.new_isize.max(0) as u64).max().unwrap_or(0);
    growth.saturating_add(rewrite).saturating_add(NATIVE_RESERVE)
}

fn available(path: &std::path::Path) -> Result<(u64, u64), String> {
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    let mut existing = path;
    while !existing.exists() {
        existing = existing.parent().ok_or_else(|| format!("No accessible parent for {}", path.display()))?;
    }
    let name = std::ffi::CString::new(existing.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: name is NUL terminated and stat is a correctly sized output.
    if unsafe { libc::statvfs(name.as_ptr(), &mut stat) } != 0 {
        return Err(format!("Cannot inspect capacity of {}: {}", existing.display(), std::io::Error::last_os_error()));
    }
    if stat.f_flag & libc::ST_RDONLY != 0 {
        return Err(format!("Filesystem at {} is read-only", existing.display()));
    }
    let device = std::fs::metadata(existing).map_err(|e| e.to_string())?.dev();
    Ok((stat.f_bavail.saturating_mul(stat.f_frsize), device))
}

/// Source archives install directly with pacman -U; their RAM scratch policy
/// does not provide free space on the installation filesystem.
pub fn check_package_root_capacity(new_size: i64, old_size: i64) -> Result<(), String> {
    let root = AlpmManager::get_rootdir();
    let (free, _) = available(&root)?;
    let needed = new_size.max(0) as u64 + new_size.saturating_sub(old_size).max(0) as u64 + NATIVE_RESERVE;
    if free < needed {
        Err(format!("Insufficient installation space on {}: {} free; estimated need {} including growth, temporary rewrite space and 128 MiB reserve. RAM scratch and --no-cache do not provide installation space; hooks and snapshots may require more.", root.display(), crate::ui::format_size(free as i64), crate::ui::format_size(needed as i64)))
    } else { Ok(()) }
}

/// Reject insufficient estimated peak space before pacman downloads or writes
/// files. Recheck after any background download, using actual cached archives.
pub fn check_native_capacity(preview: &TransactionPreview) -> Result<(), String> {
    if preview.adds.is_empty() { return Ok(()); }
    let root = AlpmManager::get_rootdir();
    let output = std::process::Command::new("pacman-conf").arg("CacheDir").output()
        .map_err(|e| format!("Cannot inspect pacman cache directories: {e}"))?;
    if !output.status.success() { return Err("Cannot inspect pacman cache directories".into()); }
    let caches: Vec<std::path::PathBuf> = String::from_utf8_lossy(&output.stdout).lines().filter(|line| !line.trim().is_empty()).map(std::path::PathBuf::from).collect();
    let cache = caches.first().ok_or("Pacman has no configured cache directory")?;
    let missing = preview.adds.iter().filter(|p| {
        p.filename.is_empty() || !caches.iter().any(|dir| std::fs::metadata(dir.join(&p.filename)).is_ok_and(|m| m.is_file() && m.len() == p.download_size as u64))
    }).fold(0u64, |sum, p| sum.saturating_add(p.download_size.max(0) as u64));
    let (root_free, root_dev) = available(&root)?;
    let (cache_free, cache_dev) = available(cache)?;
    let peak = native_peak(preview);
    let root_need = peak.saturating_add(if root_dev == cache_dev { missing } else { 0 });
    if root_free < root_need || (root_dev != cache_dev && cache_free < missing.saturating_add(NATIVE_RESERVE)) {
        return Err(format!(
            "Insufficient native transaction space: {} has {} free; estimated peak need {} (package growth, temporary rewrite space, uncached downloads and 128 MiB reserve). Cache {} has {} free; uncached downloads {}. --no-cache removes archives after successful installation; it does not reduce installation peak space. Snapshots and hooks may require more space.",
            root.display(), crate::ui::format_size(root_free as i64), crate::ui::format_size(root_need as i64), cache.display(), crate::ui::format_size(cache_free as i64), crate::ui::format_size(missing as i64)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{AlpmFixture, FixturePackage};

    fn pkg(name: &'static str, version: &'static str) -> FixturePackage {
        FixturePackage::new(name, version)
    }

    #[test]
    fn preview_reports_upgrades_and_new_dependencies() {
        let local = [pkg("app", "1.0-1"), pkg("lib-old", "1.0-1")];
        let sync = [
            FixturePackage::with_depends("app", "2.0-1", &["lib-new"]),
            FixturePackage::with_size("lib-new", "1.0-1", 100, 10),
            pkg("lib-old", "1.0-1"),
        ];
        let mut fixture = AlpmFixture::new(&local[..], &[("extra", &sync[..])]);
        let manager = fixture.manager_mut();
        let preview =
            prepare_native_preview(manager, &["app".to_string()]).expect("preview works");
        let app = preview
            .adds
            .iter()
            .find(|p| p.name == "app")
            .expect("app in adds");
        assert_eq!(app.version, "2.0-1");
        assert_eq!(app.old_version.as_deref(), Some("1.0-1"));
        assert_eq!(app.repo, "extra");
        let dep = preview
            .adds
            .iter()
            .find(|p| p.name == "lib-new")
            .expect("new dep in adds");
        assert!(dep.is_new_install());
        assert_eq!(dep.new_isize, 100);
        assert_eq!(dep.download_size, 10);
        let explicit: std::collections::HashSet<String> =
            ["app".to_string()].into_iter().collect();
        let new_deps = partition_new_dependencies(&preview, &explicit);
        assert_eq!(new_deps.len(), 1);
        assert_eq!(new_deps[0].name, "lib-new");
    }

    #[test]
    fn space_estimate_reserves_rewrites_even_for_zero_net_growth() {
        let make = |new, old| PreviewPackage { name: "pkg".into(), version: "2".into(), repo: "extra".into(), old_version: Some("1".into()), new_isize: new, old_isize: old, download_size: 10, filename: String::new() };
        let mut preview = TransactionPreview { adds: vec![make(100, 100)], ..Default::default() };
        assert_eq!(native_peak(&preview), NATIVE_RESERVE + 100);
        preview.adds = vec![make(100, 50), make(50, 100)];
        assert_eq!(native_peak(&preview), NATIVE_RESERVE + 150);
    }

    #[test]
    fn preview_honors_repo_qualified_targets() {
        let repo_a = [pkg("dup", "1.0-1")];
        let repo_b = [pkg("dup", "2.0-1")];
        let mut fixture = AlpmFixture::new(
            &[],
            &[("repo-a", &repo_a[..]), ("repo-b", &repo_b[..])],
        );
        let manager = fixture.manager_mut();
        let preview = prepare_native_preview(manager, &["repo-b/dup".to_string()])
            .expect("preview works");
        assert_eq!(preview.adds.len(), 1);
        assert_eq!(preview.adds[0].repo, "repo-b");
        assert_eq!(preview.adds[0].version, "2.0-1");
    }

    #[test]
    fn preview_prefers_exact_names_across_repositories_before_providers() {
        let local = [pkg("firefox", "1.0-1")];
        let mut provider = pkg("firefox-pure", "1.0-1");
        provider.provides = &["firefox"];
        provider.conflicts = &["firefox"];
        let earlier = [provider];
        let later = [pkg("firefox", "2.0-1")];
        let mut fixture = AlpmFixture::new(&local, &[("earlier", &earlier), ("later", &later)]);
        let preview = prepare_native_preview(fixture.manager_mut(), &["firefox".into()]).unwrap();
        assert_eq!(preview.adds.len(), 1);
        assert_eq!(preview.adds[0].name, "firefox");
        assert_eq!(preview.adds[0].repo, "later");
        assert_eq!(preview.adds[0].version, "2.0-1");
        assert!(preview.removes.is_empty());
    }

    #[test]
    fn preview_still_resolves_virtual_targets_without_an_exact_package() {
        let mut provider = pkg("provider", "1.0-1");
        provider.provides = &["virtual-app=1"];
        let sync = [provider];
        let mut fixture = AlpmFixture::new(&[], &[("extra", &sync)]);
        let preview = prepare_native_preview(fixture.manager_mut(), &["virtual-app>=1".into()]).unwrap();
        assert_eq!(preview.adds[0].name, "provider");
        let qualified = prepare_native_preview(fixture.manager_mut(), &["extra/virtual-app".into()]).unwrap();
        assert_eq!(qualified.adds[0].name, "provider");
        assert!(prepare_native_preview(fixture.manager_mut(), &["absent/virtual-app".into()]).is_err());
    }

    #[test]
    fn preview_names_conflicting_packages_and_releases_failed_transaction() {
        let local = [pkg("installed", "1.0-1")];
        let mut conflicting = pkg("conflicting", "2.0-1");
        conflicting.conflicts = &["installed<2"];
        let sync = [conflicting, pkg("safe", "1.0-1")];
        let mut fixture = AlpmFixture::new(&local, &[("extra", &sync)]);
        let error = prepare_native_preview(fixture.manager_mut(), &["conflicting".into()]).unwrap_err();
        assert!(error.contains("conflicting dependencies"), "{error}");
        assert!(error.contains("conflicting conflicts with installed (installed<2)"), "{error}");
        assert!(prepare_native_preview(fixture.manager_mut(), &["safe".into()]).is_ok());
    }

    #[test]
    fn preview_names_unsatisfied_versioned_dependencies() {
        let sync = [FixturePackage::with_depends("app", "2.0-1", &["missing>=3"])];
        let mut fixture = AlpmFixture::new(&[], &[("extra", &sync)]);
        let error = prepare_native_preview(fixture.manager_mut(), &["app".into()]).unwrap_err();
        assert!(error.contains("app requires missing>=3"), "{error}");
    }

    #[test]
    fn forced_reinstallation_stays_in_preview_for_capacity_checks() {
        let local = [FixturePackage::with_size("app", "1-1", 100, 10)];
        let sync = [FixturePackage::with_size("app", "1-1", 100, 10)];
        let mut fixture = AlpmFixture::new(&local, &[("extra", &sync)]);
        assert!(prepare_native_preview(fixture.manager_mut(), &["app".into()]).unwrap().adds.is_empty());
        let reinstall = prepare_native_preview_with_needed(fixture.manager_mut(), &["app".into()], false).unwrap();
        assert_eq!(reinstall.adds.len(), 1);
        assert_eq!(reinstall.adds[0].name, "app");
        assert_eq!(reinstall.adds[0].new_isize, 100);
        assert_eq!(native_peak(&reinstall), NATIVE_RESERVE + 100);
    }

    #[test]
    fn already_current_targets_are_not_counted_as_changes() {
        let local = [pkg("same", "1.0-1"), pkg("app", "1.0-1")];
        let sync = [pkg("same", "1.0-1"), pkg("app", "2.0-1")];
        let mut fixture = AlpmFixture::new(&local, &[("extra", &sync)]);
        let preview = prepare_native_preview(fixture.manager_mut(), &["same".into(), "app".into()]).unwrap();
        assert_eq!(preview.adds.len(), 1);
        assert_eq!(preview.adds[0].name, "app");
        let mut moved = preview.clone();
        moved.adds[0].repo = "other".into();
        assert_ne!(preview.signature(), moved.signature());
    }

    #[test]
    fn preview_reports_replacements_as_removals() {
        let local = [pkg("old-pkg", "1.0-1")];
        let mut replacer = FixturePackage::with_replaces("new-pkg", "1.0-1", &["old-pkg"]);
        replacer.conflicts = &["old-pkg"];
        let sync = [replacer];
        let mut fixture = AlpmFixture::new(&local[..], &[("extra", &sync[..])]);
        let manager = fixture.manager_mut();
        let preview = prepare_native_preview(manager, &["new-pkg".to_string()])
            .expect("preview works");
        assert!(preview.adds.iter().any(|p| p.name == "new-pkg"));
        assert!(preview
            .removes
            .iter()
            .any(|(n, v)| n == "old-pkg" && v == "1.0-1"));
    }

    #[test]
    fn preview_does_not_remove_packages_outside_replacement_version_constraint() {
        let local = [pkg("old-pkg", "3.0-1")];
        let sync = [FixturePackage::with_replaces("new-pkg", "1.0-1", &["old-pkg<2"])];
        let mut fixture = AlpmFixture::new(&local, &[("extra", &sync)]);
        let preview = prepare_native_preview(fixture.manager_mut(), &["new-pkg".into()]).unwrap();
        assert!(preview.removes.is_empty());
    }

    #[test]
    fn preview_removes_packages_matching_replacement_version_constraint() {
        let local = [pkg("old-pkg", "1.0-1")];
        let sync = [FixturePackage::with_replaces("new-pkg", "1.0-1", &["old-pkg<2"])];
        let mut fixture = AlpmFixture::new(&local, &[("extra", &sync)]);
        let preview = prepare_native_preview(fixture.manager_mut(), &["new-pkg".into()]).unwrap();
        assert_eq!(preview.removes, [("old-pkg".into(), "1.0-1".into())]);
    }

    #[test]
    fn preview_fails_closed_on_unknown_targets() {
        let sync = [pkg("real", "1.0-1")];
        let mut fixture = AlpmFixture::new(&[], &[("extra", &sync[..])]);
        let manager = fixture.manager_mut();
        assert!(prepare_native_preview(manager, &["nope".to_string()]).is_err());
        assert!(
            prepare_native_preview(manager, &["bogus-repo/real".to_string()]).is_err()
        );
        // A failed preview must release the transaction: a second attempt works.
        assert!(prepare_native_preview(manager, &["real".to_string()]).is_ok());
    }

    #[test]
    fn strip_repo_handles_bare_and_qualified_names() {
        assert_eq!(strip_repo("foo"), "foo");
        assert_eq!(strip_repo("extra/foo"), "foo");
    }
}
