use super::*;
use std::os::unix::fs::symlink;
use storage::{Lock, RECEIPT};

fn recipe(path: &Path) -> Recipe {
    let mut recipe: Recipe =
        toml::from_str("asset='app.tar'\nformat='archive'\n[binaries]\napp='bin/app'\n").unwrap();
    recipe.install_dir = Some(path.into());
    recipe
}
fn plan(path: &Path) -> Plan {
    let recipe = recipe(path);
    Plan {
        alias: "local".into(),
        name: "app".into(),
        source: super::super::Source {
            kind: super::super::Kind::Local,
            url: "/tmp/catalog.json".into(),
            token_env: None,
            packages: std::collections::BTreeMap::from([("app".into(), recipe.clone())]),
        },
        recipe,
        tag: "v1".into(),
        version: "1.0.0-1".into(),
        asset: super::super::Asset {
            name: "app.tar".into(),
            url: "app.tar".into(),
            sha256: Some("a".repeat(64)),
        },
        git: None,
        persist_source: false,
        no_cache: false,
    }
}

#[test]
fn directory_configuration_preserves_default_and_rejects_system_or_overlapping_paths() {
    let normal: Recipe =
        toml::from_str("asset='app.tar'\nformat='archive'\n[binaries]\napp='bin/app'\n").unwrap();
    assert!(normal.install_dir.is_none());
    for path in [
        "/",
        "/tmp",
        "/usr/local/app",
        "/proc/self",
        "relative/app",
        "/tmp/../app",
    ] {
        assert!(validate(&recipe(Path::new(path))).is_err(), "{path}");
    }
    let mut native = recipe(Path::new("/tmp/programs/app"));
    native.format = Format::Native;
    assert!(validate(&native).is_err());
    let mut config = Config::default();
    let p = plan(Path::new("/tmp/programs/app"));
    let mut source = p.source;
    source.packages.insert(
        "other".into(),
        recipe(Path::new("/tmp/programs/app/nested")),
    );
    config.sources.insert("local".into(), source);
    assert!(super::super::validate_config(&config)
        .unwrap_err()
        .contains("overlap"));
}

fn publish_fixture(
    work: &Path,
    destination: &Path,
    version: &str,
    content: &str,
    previous: Option<&Receipt>,
) -> Receipt {
    let stage = super::super::install::Workdir::new_with_base(work).unwrap();
    let payload = stage.0.join("payload");
    fs::create_dir(&payload).unwrap();
    fs::write(payload.join("content"), content).unwrap();
    let mut p = plan(destination);
    p.version = version.into();
    let receipt = receipt(&p, &payload, false).unwrap();
    storage::write(&payload, &receipt).unwrap();
    storage::publish(&payload, destination, previous).unwrap();
    receipt
}

#[test]
fn ownership_receipts_bind_identity_path_and_inode() {
    let work = super::super::install::Workdir::new().unwrap();
    let destination = work.0.join("app");
    let r = publish_fixture(&work.0, &destination, "1-1", "original", None);
    assert_eq!(
        storage::read(&destination, "local", "app").unwrap(),
        Some(r)
    );
    assert!(storage::read(&destination, "wrong", "app").is_err());
    assert!(storage::read(&destination, "local", "wrong").is_err());
    let outside = work.0.join("keep");
    fs::write(&outside, "preserve").unwrap();
    fs::remove_file(destination.join("current").join(RECEIPT)).unwrap();
    symlink(&outside, destination.join("current").join(RECEIPT)).unwrap();
    assert!(storage::read(&destination, "local", "app").is_err());
    assert_eq!(fs::read_to_string(&outside).unwrap(), "preserve");
}

#[test]
fn atomic_publication_preserves_unowned_or_changed_destinations_and_selects_versions() {
    let work = super::super::install::Workdir::new().unwrap();
    let destination = work.0.join("app");
    let stage = super::super::install::Workdir::new_with_base(&work.0).unwrap();
    let payload = stage.0.join("payload");
    fs::create_dir(&payload).unwrap();
    let p = plan(&destination);
    storage::write(&payload, &receipt(&p, &payload, false).unwrap()).unwrap();
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("sentinel"), "old").unwrap();
    assert!(storage::publish(&payload, &destination, None).is_err());
    assert_eq!(
        fs::read_to_string(destination.join("sentinel")).unwrap(),
        "old"
    );
    fs::remove_dir_all(&destination).unwrap();
    let original = publish_fixture(&work.0, &destination, "1-1", "original", None);
    let new_stage = super::super::install::Workdir::new_with_base(&work.0).unwrap();
    let new_payload = new_stage.0.join("payload");
    fs::create_dir(&new_payload).unwrap();
    storage::write(&new_payload, &receipt(&p, &new_payload, false).unwrap()).unwrap();
    let mut changed = original.clone();
    changed.version = "unapproved".into();
    assert!(storage::publish(&new_payload, &destination, Some(&changed)).is_err());
    assert_eq!(
        fs::read_to_string(destination.join("current/content")).unwrap(),
        "original"
    );
    let next = publish_fixture(&work.0, &destination, "2-1", "new", Some(&original));
    assert_eq!(
        storage::read(&destination, "local", "app").unwrap(),
        Some(next)
    );
    assert_eq!(
        fs::read_to_string(destination.join("current/content")).unwrap(),
        "new"
    );
    storage::clean_previous(&destination, "local", "app").unwrap();
    assert_eq!(
        fs::read_dir(destination.join("versions")).unwrap().count(),
        1
    );
}

#[test]
fn imported_applications_are_referenced_and_never_deleted_with_the_container() {
    let work = super::super::install::Workdir::new().unwrap();
    let original = work.0.join("original");
    fs::create_dir(&original).unwrap();
    fs::write(original.join("keep"), "original bytes").unwrap();
    let destination = work.0.join("managed");
    let stage = super::super::install::Workdir::new_with_base(&work.0).unwrap();
    let receipt = receipt(&plan(&destination), &original, true).unwrap();
    storage::adopt(&stage.0, &destination, &original, &receipt).unwrap();
    assert_eq!(
        storage::read(&destination, "local", "app").unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(
        fs::read_to_string(destination.join("current/keep")).unwrap(),
        "original bytes"
    );
    assert!(!original.join(RECEIPT).exists());
    publish_fixture(&work.0, &destination, "2-1", "updated", Some(&receipt));
    storage::clean_previous(&destination, "local", "app").unwrap();
    crate::execution::remove_owned_tree(&destination).unwrap();
    assert_eq!(
        fs::read_to_string(original.join("keep")).unwrap(),
        "original bytes"
    );
}

#[test]
fn locks_are_exclusive_and_never_follow_links() {
    let work = super::super::install::Workdir::new().unwrap();
    let destination = work.0.join("app");
    let first = Lock::acquire(&destination).unwrap();
    assert!(Lock::acquire(&destination).is_err());
    drop(first);
    drop(Lock::acquire(&destination).unwrap());
    fs::remove_file(work.0.join(".app.pacpin.lock")).unwrap();
    let keep = work.0.join("keep");
    fs::write(&keep, "preserve").unwrap();
    symlink(&keep, work.0.join(".app.pacpin.lock")).unwrap();
    assert!(Lock::acquire(&destination).is_err());
    assert_eq!(fs::read_to_string(keep).unwrap(), "preserve");
}

#[test]
fn directory_roots_and_declared_executables_cannot_escape() {
    let work = super::super::install::Workdir::new().unwrap();
    let root = work.0.join("app");
    fs::create_dir_all(root.join("bin")).unwrap();
    let p = plan(&root);
    symlink("/bin/sh", root.join("bin/app")).unwrap();
    assert!(verify_payload(&p, &root).is_err());
    fs::remove_file(root.join("bin/app")).unwrap();
    fs::write(root.join("bin/app"), "#!/bin/sh\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(root.join("bin/app"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(verify_payload(&p, &root).is_ok());
    let link = work.0.join("link");
    symlink(&root, &link).unwrap();
    assert!(storage::owned_directory(&link).is_err());
    assert!(storage::parent(&link.join("nested")).is_err());
}

#[test]
fn pinned_missing_directory_is_an_incomplete_check_instead_of_no_updates() {
    let work = super::super::install::Workdir::new().unwrap();
    let p = plan(&work.0.join("missing"));
    let mut config = Config::default();
    config.sources.insert(p.alias.clone(), p.source.clone());
    assert!(installed(&config, "local", "app").unwrap().is_none());
    config.pins.insert("app".into(), "local".into());
    assert!(installed(&config, "local", "app")
        .unwrap_err()
        .contains("unavailable"));
}

#[test]
fn adoption_does_not_claim_upstream_checksum_verification() {
    let work = super::super::install::Workdir::new().unwrap();
    let p = plan(&work.0);
    let r = receipt(&p, &work.0, true).unwrap();
    assert!(r.adopted && r.sha256.is_none());
    for value in ["", "1\nspoof", "1/escape", "1 2"] {
        assert!(!storage::valid_version(value));
    }
}
