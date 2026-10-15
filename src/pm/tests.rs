//! Contract checks through the stable pm API and native fixture databases.
use super::*;
use super::lock::get_lock_info_at;
use crate::resolver::ResolverEngine;

#[test]
fn download_progress_excludes_complete_cached_archives() {
    let cache = std::env::temp_dir().join(format!("pacpin-download-progress-test-{}", std::process::id()));
    std::fs::create_dir(&cache).unwrap();
    let package = |name: &str, size| crate::preview::PreviewPackage {
        name: name.into(), version: "2-1".into(), repo: "fixture".into(), old_version: Some("1-1".into()),
        new_isize: 0, old_isize: 0, download_size: size, filename: format!("{name}-2-1-x86_64.pkg.tar.zst"),
    };
    let preview = crate::preview::TransactionPreview { adds: vec![package("cached", 10), package("partial", 20), package("missing", 30)], ..Default::default() };
    std::fs::write(cache.join(&preview.adds[0].filename), [0; 10]).unwrap();
    std::fs::write(cache.join(&preview.adds[1].filename), [0; 5]).unwrap();
    assert_eq!(download_targets(&preview, std::slice::from_ref(&cache)), [("partial-2-1-x86_64".into(), 20), ("missing-2-1-x86_64".into(), 30)]);
    std::fs::remove_dir_all(cache).unwrap();
}

#[test]
fn test_install_options_default() {
    let opts = InstallOptions::default();
    assert!(opts.needed);
    assert!(!opts.noconfirm);
    assert!(!opts.refresh);
    assert!(!opts.sysupgrade);
    assert!(!opts.dry_run);
    assert!(opts.forwarded_flags.is_empty());
}

#[test]
fn test_empty_targets_rejected() {
    let mut config = Config::default();
    let opts = InstallOptions::default();
    let res = install(&mut config, &[], &opts);
    assert!(res.is_err());
    assert_eq!(res.unwrap_err(), "No package targets specified.");
}

#[test]
fn test_unknown_repo_rejected() {
    let lock_path = AlpmManager::get_dbpath().join("db.lck");
    let mut config = Config::default();
    let opts = InstallOptions::default();
    let res = install(&mut config, &["nonexistent_repo/foobar".to_string()], &opts);
    assert!(res.is_err());
    if !lock_path.exists() {
        assert!(res.unwrap_err().contains("Repository '[nonexistent_repo]' is not recognized"));
    }
}

#[test]
fn test_check_lock_when_no_lock() {
    let lock_path = AlpmManager::get_dbpath().join("db.lck");
    if !lock_path.exists() {
        assert!(check_lock().is_ok());
        assert!(get_lock_info().is_none());
    }
}

#[test]
fn test_lock_inspection_live_and_stale_pids() {
    let lock_path = std::env::temp_dir().join(format!("pacpin-lock-test-{}", std::process::id()));
    std::fs::write(&lock_path, format!("{}\n", std::process::id())).unwrap();
    let live = get_lock_info_at(lock_path.clone()).unwrap();
    assert_eq!(live.pid, Some(std::process::id() as i32));
    assert!(live.is_alive);

    std::fs::write(&lock_path, format!("{}\n", i32::MAX)).unwrap();
    let stale = get_lock_info_at(lock_path.clone()).unwrap();
    assert_eq!(stale.pid, Some(i32::MAX));
    assert!(!stale.is_alive);
    std::fs::remove_file(lock_path).unwrap();
}

#[test]
fn test_wait_for_lock_succeeds_when_unlocked() {
    let lock_path = AlpmManager::get_dbpath().join("db.lck");
    if !lock_path.exists() {
        assert!(wait_for_lock(1).is_ok());
    }
}

#[test]
fn test_wait_flag_parsing() {
    let flags1 = ["--wait".to_string(), "42".to_string()];
    let secs1 = flags1
        .iter()
        .enumerate()
        .find_map(|(idx, f)| {
            if f.starts_with("--wait=") {
                f.strip_prefix("--wait=").and_then(|s| s.parse::<u64>().ok())
            } else if f == "--wait" {
                if let Some(next) = flags1.get(idx + 1) {
                    if let Ok(secs) = next.parse::<u64>() {
                        return Some(secs);
                    }
                }
                Some(600)
            } else {
                None
            }
        });
    assert_eq!(secs1, Some(42));

    let flags2 = ["--wait=15".to_string()];
    let secs2 = flags2
        .iter()
        .enumerate()
        .find_map(|(idx, f)| {
            if f.starts_with("--wait=") {
                f.strip_prefix("--wait=").and_then(|s| s.parse::<u64>().ok())
            } else if f == "--wait" {
                if let Some(next) = flags2.get(idx + 1) {
                    if let Ok(secs) = next.parse::<u64>() {
                        return Some(secs);
                    }
                }
                Some(600)
            } else {
                None
            }
        });
    assert_eq!(secs2, Some(15));
}

#[test]
fn test_build_install_commands() {
    let targets = vec!["ollama".to_string()];
    let no_aur = Vec::new();

    // 1. Dry run with refresh only (-Sy): includes sudo pacman -Sy and sudo pacman -S
    let opts_dry_sy = InstallOptions {
        needed: true,
        noconfirm: false,
        refresh: true,
        sysupgrade: false,
        dry_run: true,
        forwarded_flags: Vec::new(),
        channel: None,
        no_cache: false,
    };
    let (cmds, op, _) = build_install_commands(&targets, &no_aur, "paru", "sudo", &opts_dry_sy, false);
    assert_eq!(op, "-S");
    assert_eq!(
        cmds,
        vec![
            "sudo pacman -Sy".to_string(),
            "sudo pacman -S --needed -- ollama".to_string()
        ]
    );

    // 2. Real execution with refresh only (sysupgrade false, dry_run false):
    // Does NOT duplicate "sudo pacman -Sy" in execution string!
    let opts_sy = InstallOptions {
        needed: true,
        noconfirm: false,
        refresh: true,
        sysupgrade: false,
        dry_run: false,
        forwarded_flags: Vec::new(),
        channel: None,
        no_cache: false,
    };
    let (cmds, op, _) = build_install_commands(&targets, &no_aur, "paru", "sudo", &opts_sy, false);
    assert_eq!(op, "-S");
    assert_eq!(cmds, vec!["sudo pacman -S --needed -- ollama".to_string()]);

    // 3. Sysupgrade (-Syu): atomic full upgrade, no duplicate -Sy
    let opts_syu = InstallOptions {
        needed: true,
        noconfirm: false,
        refresh: true,
        sysupgrade: true,
        dry_run: false,
        forwarded_flags: Vec::new(),
        channel: None,
        no_cache: false,
    };
    let (cmds, op, _) = build_install_commands(&targets, &no_aur, "paru", "sudo", &opts_syu, true);
    assert_eq!(op, "-Syu");
    assert_eq!(cmds, vec!["sudo pacman -Syu --needed -- ollama".to_string()]);

    // 4. Sysupgrade without refresh (-Su):
    let opts_su = InstallOptions {
        needed: true,
        noconfirm: false,
        refresh: false,
        sysupgrade: true,
        dry_run: false,
        forwarded_flags: Vec::new(),
        channel: None,
        no_cache: false,
    };
    let (cmds, op, _) = build_install_commands(&targets, &no_aur, "paru", "sudo", &opts_su, true);
    assert_eq!(op, "-Su");
    assert_eq!(cmds, vec!["sudo pacman -Su --needed -- ollama".to_string()]);

    // 5. AUR targets with sysupgrade when no repo targets:
    let aur_targets = vec!["google-chrome".to_string()];
    let (cmds, _op, aur_op) = build_install_commands(&[], &aur_targets, "paru", "sudo", &opts_syu, true);
    assert_eq!(aur_op, "-Syu");
    assert_eq!(cmds, vec!["paru -Syu --aur --needed -- google-chrome".to_string()]);
}

#[test]
fn test_install_unsafe_target_rejected() {
    let mut cfg = Config::default();
    let opts = InstallOptions::default();

    let res_flag = install(&mut cfg, &["--config=/tmp/bad".to_string()], &opts);
    assert!(res_flag.is_err());
    assert!(res_flag.unwrap_err().contains("Refusing unsafe package target"));

    let res_hyphen = install(&mut cfg, &["-badpkg".to_string()], &opts);
    assert!(res_hyphen.is_err());

    let res_remove = remove(&mut cfg, &["--dbpath=/tmp/bad".to_string()], &[]);
    assert!(res_remove.is_err());
    assert!(res_remove.unwrap_err().contains("Refusing unsafe package target for removal"));
}

#[test]
fn test_check_package_conflicts_runs_safely() {
    if let Ok(manager) = AlpmManager::new() {
        // Verify empty targets returns empty conflicts without error
        let empty = check_package_conflicts(&manager, &[]);
        assert!(empty.is_empty());

        // Non-existent target returns no conflicts
        let non_existent = check_package_conflicts(&manager, &["nonexistent_pkg_xyz_12345".to_string()]);
        assert!(non_existent.is_empty());
    }
}

#[test]
fn test_versioned_reverse_conflicts() {
    use crate::db::test_support::{AlpmFixture, FixturePackage};
    for (version, count) in [("37-1", 2), ("38-1", 0), ("42.1-1", 0)] {
        let mut cryptsetup = FixturePackage::new("cryptsetup", "1-1");
        cryptsetup.conflicts = &["mkinitcpio<38-1"];
        let mut mdadm = FixturePackage::new("mdadm", "1-1");
        mdadm.conflicts = &["mkinitcpio<38"];
        let fixture = AlpmFixture::new(
            &[cryptsetup, mdadm],
            &[("core", &[FixturePackage::new("mkinitcpio", version)])],
        );
        assert_eq!(check_package_conflicts(&fixture.manager, &["mkinitcpio".into()]).len(), count);
    }
}

fn firefox_provider_fixture() -> crate::db::test_support::AlpmFixture {
    use crate::db::test_support::{AlpmFixture, FixturePackage};
    let mut provider = FixturePackage::new("firefox-pure", "2.0-1");
    provider.provides = &["firefox", "firefox=2.0"];
    provider.conflicts = &["firefox"];
    AlpmFixture::new(
        &[FixturePackage::new("firefox", "1.0-1")],
        &[
            ("first", &[provider]),
            ("second", &[FixturePackage::new("firefox", "2.0-1")]),
        ],
    )
}

#[test]
fn upgrade_conflicts_inspect_exact_package_before_earlier_repo_provider() {
    let fixture = firefox_provider_fixture();
    let result = ResolverEngine::new(&fixture.manager).resolve_with_options(&Config::default(), true);
    assert_eq!(result.updates.len(), 1);
    let candidate = result.updates[0].candidate.as_ref().unwrap();
    assert_eq!(candidate.name, "firefox");
    assert_eq!(candidate.repo, "second");
    assert!(check_package_conflicts(&fixture.manager, &["firefox".into()]).is_empty());
    assert!(check_resolved_package_conflicts(&fixture.manager, &result.updates).is_empty());
}

#[test]
fn explicit_provider_conflicts_and_repository_scope_are_preserved() {
    let fixture = firefox_provider_fixture();
    let expected = vec![("firefox-pure".to_string(), "firefox".to_string())];
    assert_eq!(check_package_conflicts(&fixture.manager, &["firefox-pure".into()]), expected);
    assert_eq!(check_package_conflicts(&fixture.manager, &["first/firefox".into()]), expected);
    assert!(check_package_conflicts(&fixture.manager, &["second/firefox".into()]).is_empty());
    assert!(check_package_conflicts(&fixture.manager, &["missing/firefox".into()]).is_empty());
}

#[test]
fn virtual_target_still_uses_provider_when_no_exact_sync_package_exists() {
    use crate::db::test_support::{AlpmFixture, FixturePackage};
    let mut provider = FixturePackage::new("firefox-pure", "2.0-1");
    provider.provides = &["firefox"];
    provider.conflicts = &["firefox"];
    let fixture = AlpmFixture::new(
        &[FixturePackage::new("firefox", "1.0-1")],
        &[("first", &[provider])],
    );
    assert_eq!(
        check_package_conflicts(&fixture.manager, &["firefox".into()]),
        vec![("firefox-pure".to_string(), "firefox".to_string())],
    );
}

#[test]
fn planned_repository_is_used_instead_of_resolving_the_name_again() {
    use crate::db::test_support::{AlpmFixture, FixturePackage};
    let mut first_firefox = FixturePackage::new("firefox", "2-1");
    first_firefox.conflicts = &["plugin"];
    let fixture = AlpmFixture::new(
        &[FixturePackage::new("firefox", "1-1"), FixturePackage::new("plugin", "1-1")],
        &[
            ("first", &[first_firefox]),
            ("second", &[FixturePackage::new("firefox", "3-1")]),
        ],
    );
    let mut config = Config::default();
    config.pins.insert("firefox".into(), "second".into());
    let result = ResolverEngine::new(&fixture.manager).resolve_with_options(&config, true);
    assert_eq!(result.updates.len(), 1);
    assert_eq!(result.updates[0].candidate.as_ref().unwrap().repo, "second");
    assert_eq!(check_package_conflicts(&fixture.manager, &["firefox".into()]).len(), 1);
    assert!(check_resolved_package_conflicts(&fixture.manager, &result.updates).is_empty());
}

#[test]
fn native_conflict_rules_handle_versioned_and_unversioned_provides() {
    use crate::db::test_support::{AlpmFixture, FixturePackage};
    for (conflict, provides, expected) in [
        (&["api>=1:2.0"][..], &["api=1:2.0"][..], 1),
        (&["api>=1:2.0"][..], &["api=2.1"][..], 0),
        (&["api>=1:2.0"][..], &["api"][..], 0),
        (&["api"][..], &["api"][..], 1),
    ] {
        let mut consumer = FixturePackage::new("consumer", "1-1");
        consumer.conflicts = conflict;
        let mut provider = FixturePackage::new("provider", "99-1");
        provider.provides = provides;
        let fixture = AlpmFixture::new(&[consumer], &[("repo", &[provider])]);
        assert_eq!(check_package_conflicts(&fixture.manager, &["provider".into()]).len(), expected);
    }
}

#[test]
fn outdated_installed_conflict_is_removed_when_both_packages_are_upgraded() {
    use crate::db::test_support::{AlpmFixture, FixturePackage};
    let mut old_consumer = FixturePackage::new("consumer", "1-1");
    old_consumer.conflicts = &["engine>=2"];
    let fixture = AlpmFixture::new(
        &[old_consumer, FixturePackage::new("engine", "1-1")],
        &[("repo", &[FixturePackage::new("consumer", "2-1"), FixturePackage::new("engine", "2-1")])],
    );
    assert_eq!(check_package_conflicts(&fixture.manager, &["engine".into()]).len(), 1);
    assert!(check_package_conflicts(&fixture.manager, &["engine".into(), "consumer".into()]).is_empty());
}

#[test]
fn all_installed_providers_are_checked_and_self_upgrades_are_excluded() {
    use crate::db::test_support::{AlpmFixture, FixturePackage};
    let mut first = FixturePackage::new("plugin-a", "1-1");
    first.provides = &["api"];
    let mut second = FixturePackage::new("plugin-b", "1-1");
    second.provides = &["api"];
    let mut candidate = FixturePackage::new("plugin-a", "2-1");
    candidate.provides = &["api"];
    candidate.conflicts = &["api"];
    let fixture = AlpmFixture::new(&[first, second], &[("repo", &[candidate])]);
    assert_eq!(
        check_package_conflicts(&fixture.manager, &["plugin-a".into()]),
        vec![("plugin-a".to_string(), "plugin-b".to_string())],
    );
}

#[test]
fn conflicts_between_selected_candidates_are_reported_once() {
    use crate::db::test_support::{AlpmFixture, FixturePackage};
    let mut first = FixturePackage::new("first", "1-1");
    first.conflicts = &["second"];
    let mut second = FixturePackage::new("second", "1-1");
    second.conflicts = &["first"];
    let fixture = AlpmFixture::new(&[], &[("repo", &[first, second])]);
    assert!(check_package_conflicts(&fixture.manager, &["first".into()]).is_empty());
    assert_eq!(
        check_package_conflicts(&fixture.manager, &["second".into(), "first".into()]),
        vec![("first".to_string(), "second".to_string())],
    );
}

#[test]
fn remove_dry_run_never_executes() {
    // `-n` / `--dry-run` only preview; forwarded to `pacman -R` they
    // would mean `--nosave`, so they must be consumed, never forwarded.
    let mut config = crate::config::Config::default();
    let missing = ["definitely-not-a-real-package-xyz".to_string()];
    assert!(remove(&mut config, &missing, &["-n".to_string()]).is_ok());
    assert!(remove(&mut config, &missing, &["--dry-run".to_string()]).is_ok());
    assert!(remove(&mut config, &["--config".to_string()], &["-n".to_string()]).is_err());
}
