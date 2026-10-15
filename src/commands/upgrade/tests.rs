//! Upgrade command and requested-plan contract checks.
use super::*;
use super::planning::requested_status;

#[test]
fn noconfirm_only_autoremoves_pure_orphans_when_requested() {
    let make_orphan = |name: &str, optional_for: Vec<String>| OrphanPackage {
        name: name.into(), version: "1".into(), isize: 1,
        desc: String::new(), is_projected: false, dropped_by: Vec::new(), optional_for,
    };
    let orphans = vec![
        make_orphan("pure", Vec::new()),
        make_orphan("plugin", vec!["installed-parent".into()]),
    ];
    assert!(noconfirm_orphan_selection(&orphans, false).is_empty());
    assert_eq!(noconfirm_orphan_selection(&orphans, true), vec!["pure"]);
    let selected = vec!["pure".into(), "plugin".into(), "no-longer-orphan".into()];
    assert_eq!(revalidate_orphan_selection(&selected, &orphans, false), vec!["pure", "plugin"]);
    assert_eq!(revalidate_orphan_selection(&selected, &orphans, true), vec!["pure"]);
}

#[test]
fn upgrade_targets_reject_option_injection() {
    assert!(validate_upgrade_targets(&["extra/git".into()], &["paru-bin".into()]).is_ok());
    assert!(validate_upgrade_targets(&["--config".into()], &[]).is_err());
    assert!(validate_upgrade_targets(&[], &["-evil".into()]).is_err());
}

#[test]
fn repo_prefetch_downloads_without_installing_or_refreshing() {
    let targets = vec!["core/linux".into(), "extra/git".into()];
    let command = repo_prefetch_command(&targets, &["--ignore", "linux-firmware"]);
    let args: Vec<_> = command.get_args().map(|arg| arg.to_str().unwrap()).collect();
    assert_eq!(args, [
        "-n", "pacman", "-Sw", "--needed", "--noconfirm",
        "--ignore", "linux-firmware", "--", "core/linux", "extra/git",
    ]);
}

#[test]
fn requested_status_compares_versions() {
    assert_eq!(requested_status(None, "1.0-1"), RequestedStatus::NewInstall);
    assert_eq!(
        requested_status(Some("1.0-1"), "2.0-1"),
        RequestedStatus::Upgrade
    );
    assert_eq!(
        requested_status(Some("2.0-1"), "1.0-1"),
        RequestedStatus::Downgrade
    );
    assert_eq!(
        requested_status(Some("1.0-1"), "1.0-1"),
        RequestedStatus::AlreadyCurrent
    );
}

#[test]
fn ignored_names_collects_comma_lists() {
    let flags = vec![
        "--ignore=a,b".to_string(),
        "--ignore".to_string(),
        "c".to_string(),
        "--noconfirm".to_string(),
    ];
    let got = ignored_names(&flags);
    assert!(got.contains("a") && got.contains("b") && got.contains("c"));
    assert_eq!(got.len(), 3);
}

#[test]
fn collect_ignored_names_expands_groups() {
    use crate::db::test_support::{AlpmFixture, FixturePackage};
    let mut member = FixturePackage::new("member", "1-1");
    member.groups = &["mygroup"];
    let sync = [member, FixturePackage::new("other", "1-1")];
    let fixture = AlpmFixture::new(&[], &[("extra", &sync)]);
    let flags = vec![
        "--ignore=solo".to_string(),
        "--ignoregroup".to_string(),
        "mygroup".to_string(),
        "--ignoregroup=bogusgroup".to_string(),
    ];
    let got = collect_ignored_names(&fixture.manager, &flags);
    assert!(got.contains("solo"));
    assert!(got.contains("member"));
    assert!(!got.contains("other"));
    assert!(!got.contains(""));
}

#[test]
fn ignored_names_matches_exact_entries() {
    let flags = vec![
        "--ignore=linux,linux-firmware".to_string(),
        "--ignore".to_string(),
        "foo".to_string(),
    ];
    let got = ignored_names(&flags);
    assert!(got.contains("linux"));
    assert!(got.contains("linux-firmware"));
    assert!(got.contains("foo"));
    assert!(!got.contains("bar"));
    assert!(!got.contains("linux-firm"));
}

#[test]
fn requested_targets_classify_native_and_fail_closed() {
    use crate::db::test_support::{AlpmFixture, FixturePackage};
    use std::collections::HashMap;

    let local = [FixturePackage::new("old", "1.0-1")];
    let sync = [
        FixturePackage::new("newpkg", "1.0-1"),
        FixturePackage::new("old", "2.0-1"),
    ];
    let fixture = AlpmFixture::new(&local[..], &[("extra", &sync[..])]);
    let config = Config::default();
    let res = ResolveResult {
        check_errors: Vec::new(),
        installed_count: 0,
        repos: Vec::new(),
        packages: HashMap::new(),
        updates: Vec::new(),
        held_packages: Vec::new(),
        unresolved_pins: Vec::new(),
    };

    let out = resolve_requested_targets(
        &config,
        &fixture.manager,
        &res,
        &["newpkg".to_string(), "old".to_string()],
        &HashSet::new(),
    )
    .expect("native requested targets resolve");
    assert_eq!(
        out.native,
        vec!["extra/newpkg".to_string(), "extra/old".to_string()]
    );
    assert!(out.aur.is_empty());
    let statuses: Vec<(String, RequestedStatus)> = out
        .items
        .iter()
        .map(|item| (item.name.clone(), item.status.clone()))
        .collect();
    assert!(statuses.contains(&("newpkg".to_string(), RequestedStatus::NewInstall)));
    assert!(statuses.contains(&("old".to_string(), RequestedStatus::Upgrade)));

    // Repeated targets are shown and executed once.
    let out = resolve_requested_targets(
        &config,
        &fixture.manager,
        &res,
        &["newpkg".to_string(), "extra/newpkg".to_string()],
        &HashSet::new(),
    )
    .expect("repeated targets resolve");
    assert_eq!(out.items.len(), 1);
    assert_eq!(out.native.len(), 1);

    // Unknown repos, unsafe targets, ignored packages, and upstream
    // source targets fail closed without network access.
    assert!(resolve_requested_targets(
        &config,
        &fixture.manager,
        &res,
        &["extra/nope".to_string()],
        &HashSet::new(),
    )
    .is_err());
    assert!(resolve_requested_targets(
        &config,
        &fixture.manager,
        &res,
        &["bogus/pkg".to_string()],
        &HashSet::new(),
    )
    .is_err());
    assert!(resolve_requested_targets(
        &config,
        &fixture.manager,
        &res,
        &["--config".to_string()],
        &HashSet::new(),
    )
    .is_err());
    let mut ignore_old = HashSet::new();
    ignore_old.insert("old".to_string());
    assert!(resolve_requested_targets(
        &config,
        &fixture.manager,
        &res,
        &["old".to_string()],
        &ignore_old,
    )
    .is_err());
    assert!(resolve_requested_targets(
        &config,
        &fixture.manager,
        &res,
        &["github:owner/repo".to_string()],
        &HashSet::new(),
    )
    .is_err());

    let mut alias_config = config.clone();
    alias_config.sources.insert(
        "myalias".to_string(),
        crate::sources::Source {
            kind: crate::sources::Kind::Github,
            url: String::new(),
            token_env: None,
            packages: Default::default(),
        },
    );
    assert!(resolve_requested_targets(
        &alias_config,
        &fixture.manager,
        &res,
        &["myalias/pkg".to_string()],
        &HashSet::new(),
    )
    .is_err());
}

fn empty_resolve_result() -> ResolveResult {
    ResolveResult {
        check_errors: Vec::new(),
        installed_count: 0,
        repos: Vec::new(),
        packages: std::collections::HashMap::new(),
        updates: Vec::new(),
        held_packages: Vec::new(),
        unresolved_pins: Vec::new(),
    }
}

#[test]
fn requested_targets_honor_repo_pins() {
    use crate::db::test_support::{AlpmFixture, FixturePackage};

    let local = [FixturePackage::new("dup", "1.0-1")];
    let repo_a = [FixturePackage::new("dup", "2.0-1")];
    let repo_b = [FixturePackage::new("dup", "1.0-1")];
    let fixture =
        AlpmFixture::new(&local[..], &[("repo-a", &repo_a[..]), ("repo-b", &repo_b[..])]);
    let res = empty_resolve_result();

    // Without a pin, plain repository priority (first sync db) wins.
    let out = resolve_requested_targets(
        &Config::default(),
        &fixture.manager,
        &res,
        &["dup".to_string()],
        &HashSet::new(),
    )
    .expect("unpinned request resolves");
    assert_eq!(out.native, vec!["repo-a/dup".to_string()]);
    assert_eq!(out.items[0].status, RequestedStatus::Upgrade);

    // With a pin, the pinned repository wins even when it is not first.
    let mut pinned_config = Config::default();
    pinned_config.pins.insert("dup".to_string(), "repo-b".to_string());
    let out = resolve_requested_targets(
        &pinned_config,
        &fixture.manager,
        &res,
        &["dup".to_string()],
        &HashSet::new(),
    )
    .expect("pinned request resolves");
    assert_eq!(out.native, vec!["repo-b/dup".to_string()]);
    assert_eq!(out.items[0].status, RequestedStatus::AlreadyCurrent);

    // Glob pins apply too.
    pinned_config.pins.clear();
    pinned_config
        .pins
        .insert("du*".to_string(), "repo-b".to_string());
    let out = resolve_requested_targets(
        &pinned_config,
        &fixture.manager,
        &res,
        &["dup".to_string()],
        &HashSet::new(),
    )
    .expect("glob-pinned request resolves");
    assert_eq!(out.native, vec!["repo-b/dup".to_string()]);

    // A pin to a repository that does not carry the package fails
    // closed instead of silently installing from elsewhere.
    pinned_config.pins.clear();
    pinned_config
        .pins
        .insert("dup".to_string(), "repo-b".to_string());
    let mut missing_config = Config::default();
    missing_config
        .pins
        .insert("dup".to_string(), "repo-missing".to_string());
    assert!(resolve_requested_targets(
        &missing_config,
        &fixture.manager,
        &res,
        &["dup".to_string()],
        &HashSet::new(),
    )
    .is_err());

    // Disabled pinning restores plain priority.
    pinned_config.features.pinning = false;
    let out = resolve_requested_targets(
        &pinned_config,
        &fixture.manager,
        &res,
        &["dup".to_string()],
        &HashSet::new(),
    )
    .expect("unpinned request resolves");
    assert_eq!(out.native, vec!["repo-a/dup".to_string()]);
}
