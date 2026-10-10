//! Evidence required before continuing after an AUR build failure.
//! Unknown failures and any native activity remain a stop condition.
use super::{helper_privilege_args, AlpmManager, Config, InstallOptions};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy)]
pub enum AurOperation {
    Install,
    Upgrade,
    RefreshUpgrade,
    Rebuild,
}
impl AurOperation {
    fn flag(self) -> &'static str {
        match self {
            Self::Install => "-S",
            Self::Upgrade => "-Su",
            Self::RefreshUpgrade => "-Syu",
            Self::Rebuild => "-S",
        }
    }
}

pub struct AurExecution {
    pub status: ExitStatus,
    pub continuation: Result<AurBuildFailurePermit, String>,
    pub verification: Result<(), String>,
}

#[derive(Default)]
struct FailureEvidence {
    build_failed: bool,
    native_seen: bool,
}
impl FailureEvidence {
    fn observe(&mut self, line: &str) {
        let lower = line.to_lowercase();
        if line.starts_with("==> ERROR:")
            && [
                "a failure occurred in prepare()",
                "a failure occurred in build()",
                "a failure occurred in check()",
                "a failure occurred in package()",
                "failed to create package file",
            ]
            .iter()
            .any(|marker| lower.contains(marker))
        {
            self.build_failed = true;
        }
        if [
            "processing package changes",
            "checking for file conflicts",
            "loading packages",
            "checking available disk space",
            "failed to commit transaction",
            "could not commit transaction",
            "could not extract",
            "running post-transaction hooks",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            self.native_seen = true;
        }
    }
    fn verify(&self) -> Result<(), String> {
        if self.native_seen {
            return Err("The AUR stage invoked or failed a native installation; recovery must be assessed first".into());
        }
        if !self.build_failed {
            return Err("The helper did not report a recognized makepkg build failure; its outcome is uncertain".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativePaths {
    root: PathBuf,
    db: PathBuf,
    log: PathBuf,
}
impl NativePaths {
    fn configured() -> Result<Self, String> {
        let out = crate::progress::capture_output(
            Command::new("pacman-conf").args(["RootDir", "DBPath", "LogFile"]),
        )
        .map_err(|e| format!("Cannot inspect native configuration: {e}"))?;
        if !out.status.success() {
            return Err("Native configuration lookup failed".into());
        }
        let text =
            std::str::from_utf8(&out.stdout).map_err(|_| "Invalid native configuration output")?;
        Self::parse(text)
    }
    fn parse(text: &str) -> Result<Self, String> {
        let mut values = HashMap::new();
        for line in text.lines() {
            let (name, value) = line
                .split_once(" = ")
                .ok_or("Unrecognized native configuration output")?;
            let path = PathBuf::from(value);
            if !path.is_absolute() || values.insert(name, path).is_some() {
                return Err("Ambiguous native configuration".into());
            }
        }
        if values.len() != 3 {
            return Err("Incomplete native configuration".into());
        }
        Ok(Self {
            root: values.remove("RootDir").ok_or("Missing RootDir")?,
            db: values.remove("DBPath").ok_or("Missing DBPath")?,
            log: values.remove("LogFile").ok_or("Missing LogFile")?,
        })
    }
}

#[derive(Clone)]
struct NativeCheckpoint {
    paths: NativePaths,
    digest: [u8; 32],
}
impl NativeCheckpoint {
    fn capture() -> Result<Self, String> {
        Self::at(NativePaths::configured()?)
    }
    fn at(paths: NativePaths) -> Result<Self, String> {
        if paths.db.join("db.lck").exists() {
            return Err("The native database is locked".into());
        }
        let mut digest = Sha256::new();
        let mut remaining = 256 * 1024 * 1024u64;
        fingerprint(&paths.db, &mut digest, &mut remaining)?;
        fingerprint(&paths.log, &mut digest, &mut remaining)?;
        Ok(Self {
            paths,
            digest: digest.finalize().into(),
        })
    }
    fn verify(&self) -> Result<(), String> {
        let paths = NativePaths::configured()?;
        if paths != self.paths {
            return Err("Native configuration changed during the AUR stage".into());
        }
        let current = Self::at(paths)?;
        if current.digest != self.digest {
            return Err("Native database or pacman log changed; isolated build failure cannot be established".into());
        }
        Ok(())
    }
}

fn fingerprint(path: &Path, digest: &mut Sha256, remaining: &mut u64) -> Result<(), String> {
    let before = fs::symlink_metadata(path)
        .map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
    if before.file_type().is_symlink() || !(before.is_dir() || before.is_file()) {
        return Err(format!(
            "Cannot verify native state at {} (not a regular file or directory)",
            path.display()
        ));
    }
    digest.update(path.as_os_str().as_encoded_bytes());
    for field in [
        before.dev(),
        before.ino(),
        before.len(),
        before.mtime() as u64,
        before.mtime_nsec() as u64,
        before.mode() as u64,
    ] {
        digest.update(field.to_le_bytes());
    }
    if before.is_dir() {
        let mut entries = fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        entries.sort();
        for entry in entries {
            fingerprint(&entry, digest, remaining)?;
        }
    } else {
        if path.file_name().is_some_and(|name| name == "desc") && before.len() == 0 {
            return Err("An installed package has empty native database metadata".into());
        }
        *remaining = remaining
            .checked_sub(before.len())
            .ok_or("Native state exceeds the bounded safety inspection")?;
        let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
        let mut buffer = [0u8; 16_384];
        let mut read_bytes = 0u64;
        loop {
            let size = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if size == 0 {
                break;
            }
            read_bytes = read_bytes.saturating_add(size as u64);
            if read_bytes > before.len() {
                return Err("Native state grew during safety inspection".into());
            }
            digest.update(&buffer[..size]);
        }
    }
    let after = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if (
        before.ino(),
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
    ) != (after.ino(), after.len(), after.mtime(), after.mtime_nsec())
    {
        return Err("Native state changed during safety inspection".into());
    }
    Ok(())
}

pub struct AurBuildFailurePermit {
    checkpoint: NativeCheckpoint,
    failed_packages: Vec<String>,
    installation_attempted: AtomicBool,
}
impl AurBuildFailurePermit {
    pub fn validate(&self) -> Result<(), String> {
        if self.installation_attempted.load(Ordering::Relaxed) {
            return Err("An independent native installation was attempted without verified completion; later updates must stop".into());
        }
        self.checkpoint.verify()?;
        let manager = AlpmManager::new()
            .map_err(|e| format!("Cannot inspect the installed database: {e}"))?;
        validate_installed_dependencies(&manager)
    }
    pub fn failed_packages(&self) -> &[String] {
        &self.failed_packages
    }
    pub fn begin_source_install(&self) -> Result<(), String> {
        self.validate()?;
        self.installation_attempted.store(true, Ordering::Relaxed);
        Ok(())
    }
    pub fn source_completed(&mut self) -> Result<(), String> {
        let manager = AlpmManager::new().map_err(|e| e.to_string())?;
        validate_installed_dependencies(&manager)?;
        self.checkpoint = NativeCheckpoint::capture()?;
        self.installation_attempted.store(false, Ordering::Relaxed);
        Ok(())
    }
}

fn validate_installed_dependencies(manager: &AlpmManager) -> Result<(), String> {
    let local = manager.handle().localdb().pkgs();
    let mut checked = HashSet::new();
    for package in local {
        for dependency in package.depends() {
            let dependency = dependency.to_string();
            if checked.insert(dependency.clone())
                && manager
                    .handle()
                    .localdb()
                    .pkgs()
                    .find_satisfier(dependency.as_str())
                    .is_none()
            {
                return Err(format!(
                    "Installed dependency mismatch: {} requires {dependency}",
                    package.name()
                ));
            }
        }
    }
    Ok(())
}

pub fn continuation_archive_check(
    config: &Config,
    archive: &Path,
    expected: &str,
) -> Result<(), String> {
    let mut manager =
        AlpmManager::with_repo_order(&config.repo_order).map_err(|e| e.to_string())?;
    let handle = manager.handle_mut();
    handle
        .trans_init(alpm::TransFlag::NO_LOCK)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        let package = handle
            .pkg_load(
                archive.as_os_str().as_encoded_bytes().to_vec(),
                true,
                alpm::SigLevel::NONE,
            )
            .map_err(|e| e.to_string())?;
        handle.trans_add_pkg(package).map_err(|e| e.to_string())?;
        handle
            .trans_prepare()
            .map_err(|e| format!("Independent source transaction cannot be prepared: {e}"))?;
        if !handle.trans_remove().is_empty()
            || handle.trans_add().into_iter().any(|p| p.name() != expected)
        {
            return Err("Continuation would affect other packages; source update skipped".into());
        }
        Ok(())
    })();
    handle.trans_release().map_err(|e| e.to_string())?;
    result
}

pub fn ensure_independent(
    manager: &AlpmManager,
    name: &str,
    dependencies: &[String],
    failed: &[String],
) -> Result<(), String> {
    let blocked: HashSet<_> = failed.iter().map(String::as_str).collect();
    if blocked.contains(name) {
        return Err(format!("{name} is part of the failed AUR stage"));
    }
    let mut pending = dependencies.to_vec();
    let mut seen = HashSet::new();
    while let Some(spec) = pending.pop() {
        let package = manager.handle().localdb().pkgs().find_satisfier(spec.as_str())
            .ok_or_else(|| format!("{name} requires unavailable installed dependency {spec}; continuation cannot install extra dependencies"))?;
        if blocked.contains(package.name()) {
            return Err(format!(
                "{name} depends on failed AUR target {}",
                package.name()
            ));
        }
        if seen.insert(package.name().to_owned()) {
            pending.extend(
                package
                    .depends()
                    .into_iter()
                    .map(|dependency| dependency.to_string()),
            );
        }
    }
    Ok(())
}

pub fn execute_aur(
    config: &Config,
    targets: &[String],
    options: &InstallOptions,
    operation: AurOperation,
    keep_going: bool,
) -> io::Result<AurExecution> {
    if targets.is_empty()
        || targets
            .iter()
            .any(|target| !crate::utils::is_safe_install_target(target))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unsafe AUR target",
        ));
    }
    // AUR output sizes are unknown; retain the root reserve check at the shared
    // execution boundary as well as the native installer's own space checks.
    crate::preview::check_package_root_capacity(0, 0).map_err(io::Error::other)?;
    let checkpoint = if !keep_going {
        Err("--keep-going was not requested".into())
    } else if !super::workspace::is_paru(&config.options.helper)
        || !options.forwarded_flags.is_empty()
        || !matches!(operation, AurOperation::Install)
    {
        Err("Continuation requires the default native configuration and dependency policy; forwarded helper flags or refresh operations are not eligible".into())
    } else {
        NativeCheckpoint::capture()
    };
    let flags = helper_privilege_args(&config.options.helper, &config.options.privilege_tool)
        .map_err(io::Error::other)?;
    crate::privilege::prepare(&config.options.privilege_tool)?;
    let mut command = Command::new(&config.options.helper);
    if checkpoint.is_ok() && super::workspace::is_paru(&config.options.helper) {
        command.args([
            "--pacman",
            "/usr/bin/pacman",
            "--makepkg",
            "/usr/bin/makepkg",
            "--nochroot",
            "--nolocalrepo",
        ]);
    }
    let mut workspace = super::workspace::AurWorkspace::prepare(
        &config.options.helper,
        crate::scratch::policy(),
        &options.forwarded_flags,
    )?;
    if let Some(work) = workspace.as_ref() {
        work.configure(&mut command);
    }
    command.args([operation.flag(), "--aur"]).args(flags);
    if matches!(operation, AurOperation::Rebuild) { command.arg("--rebuild"); }
    if options.needed {
        command.arg("--needed");
    }
    if options.noconfirm {
        command.arg("--noconfirm");
    }
    if options.no_cache {
        command.arg("--cleanafter");
    }
    command
        .args(&options.forwarded_flags)
        .arg("--")
        .args(targets);
    let mut evidence = FailureEvidence::default();
    let status = crate::progress::run_observed(
        &mut command,
        crate::progress::Manager::Aur,
        "Building / installing AUR packages",
        || {},
        |line| evidence.observe(line),
    )?;
    let verification = if status.success() && matches!(operation, AurOperation::Rebuild) {
        super::rebuild::verify_installed(config, targets)
    } else { Ok(()) };
    if let Some(work) = workspace.as_mut() {
        work.finish(status.success() && verification.is_ok());
    }
    let continuation = if status.success() {
        Err("AUR stage completed successfully".into())
    } else if status.code().is_none() {
        Err("AUR stage was interrupted; continuation is disabled".into())
    } else {
        (|| {
            evidence.verify()?;
            let checkpoint = checkpoint?;
            let permit = AurBuildFailurePermit {
                checkpoint,
                installation_attempted: AtomicBool::new(false),
                failed_packages: targets
                    .iter()
                    .map(|target| target.rsplit('/').next().unwrap_or(target).to_owned())
                    .collect(),
            };
            permit.validate()?;
            Ok(permit)
        })()
    };
    Ok(AurExecution {
        verification,
        status,
        continuation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{AlpmFixture, FixturePackage};
    #[test]
    fn only_recognized_build_failures_allow_continuation() {
        for line in [
            "==> ERROR: Failed to create package file.",
            "==> ERROR: A failure occurred in build().",
            "==> ERROR: A failure occurred in package().",
        ] {
            let mut evidence = FailureEvidence::default();
            evidence.observe(line);
            assert!(evidence.verify().is_ok());
            evidence.observe("error: could not extract /usr/lib/libfixture.so (Write failed)");
            assert!(evidence.verify().is_err());
        }
        let mut evidence = FailureEvidence::default();
        evidence.observe("error: No space left on device");
        assert!(evidence.verify().is_err());
        evidence.observe(":: Processing package changes...");
        evidence.observe("==> ERROR: Failed to create package file.");
        assert!(evidence.verify().is_err());
    }
    #[test]
    fn native_configuration_must_be_complete_and_unambiguous() {
        assert!(NativePaths::parse(
            "RootDir = /\nDBPath = /var/lib/pacman\nLogFile = /var/log/pacman.log\n"
        )
        .is_ok());
        assert!(NativePaths::parse("RootDir = /\n").is_err());
        assert!(NativePaths::parse("RootDir = /\nDBPath = relative\nLogFile = /log\n").is_err());
    }
    #[test]
    fn dependency_closure_rejects_failed_targets_missing_versions_and_cycles_terminate() {
        let fixture = AlpmFixture::new(
            &[
                FixturePackage::with_depends("lib-a", "1-1", &["lib-b"]),
                FixturePackage::with_depends("lib-b", "1-1", &["lib-a"]),
                FixturePackage::new("independent", "1-1"),
            ],
            &[],
        );
        let failed = vec!["lib-b".into()];
        assert!(
            ensure_independent(&fixture.manager, "app", &["independent".into()], &failed).is_ok()
        );
        assert!(ensure_independent(&fixture.manager, "app", &["lib-a".into()], &failed).is_err());
        assert!(
            ensure_independent(&fixture.manager, "app", &["independent>=2".into()], &failed)
                .is_err()
        );
        assert!(ensure_independent(&fixture.manager, "app", &["lib-a".into()], &[]).is_ok());
        assert!(ensure_independent(&fixture.manager, "lib-b", &[], &failed).is_err());
    }
    #[test]
    fn checkpoints_detect_native_changes_and_empty_metadata() {
        let fixture = AlpmFixture::new(&[FixturePackage::new("fixture", "1-1")], &[]);
        let db = PathBuf::from(fixture.manager.handle().dbpath());
        let log = db.join("pacman.log");
        fs::write(&log, "original log\n").unwrap();
        let paths = NativePaths {
            root: "/".into(),
            db: db.clone(),
            log: log.clone(),
        };
        let original = NativeCheckpoint::at(paths.clone()).unwrap();
        assert_eq!(
            original.digest,
            NativeCheckpoint::at(paths.clone()).unwrap().digest
        );
        fs::write(&log, "native transaction started\n").unwrap();
        assert_ne!(
            original.digest,
            NativeCheckpoint::at(paths.clone()).unwrap().digest
        );
        fs::write(db.join("local/fixture-1-1/desc"), "").unwrap();
        assert!(NativeCheckpoint::at(paths).is_err());
    }
    #[test]
    fn uncertain_source_installation_poison_is_checked_before_external_probes() {
        let fixture = AlpmFixture::new(&[FixturePackage::new("fixture", "1-1")], &[]);
        let db = PathBuf::from(fixture.manager.handle().dbpath());
        let log = db.join("pacman.log");
        fs::write(&log, "original").unwrap();
        let checkpoint = NativeCheckpoint::at(NativePaths {
            root: "/".into(),
            db,
            log,
        })
        .unwrap();
        let permit = AurBuildFailurePermit {
            checkpoint,
            failed_packages: vec!["failed".into()],
            installation_attempted: AtomicBool::new(true),
        };
        assert!(permit
            .validate()
            .unwrap_err()
            .contains("without verified completion"));
    }
    #[test]
    fn installed_dependency_mismatches_block_continuation() {
        let fixture = AlpmFixture::new(
            &[FixturePackage::with_depends("app", "1-1", &["missing>=2"])],
            &[],
        );
        assert!(validate_installed_dependencies(&fixture.manager).is_err());
    }
}
