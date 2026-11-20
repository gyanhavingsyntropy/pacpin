//! Private dependency roots. No ALPM transaction or system dependency install.
use super::{create_secure_temp_dir_in, Options, SandboxGuard};
use crate::{config::Config, db::AlpmManager, execution::BuildWorkspace};
use std::{
    collections::HashSet,
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};

pub(crate) struct PackageRoot<'a> {
    pub root: PathBuf,
    pub work: PathBuf,
    pub manager: AlpmManager,
    pub view: BuildWorkspace,
    config: &'a Config,
    options: &'a Options,
    guard: SandboxGuard,
    archives: Vec<(PathBuf, crate::db::ArchiveMetadata)>,
    names: HashSet<String>,
    building: HashSet<String>,
    resolving: HashSet<String>,
    pub build_failed: bool,
}

pub(crate) fn confirm_build(label: &str, noconfirm: bool) -> Result<(), String> {
    println!(":: {label}. Build code runs as your user in a private filesystem view; no system packages will be installed.");
    if noconfirm {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        return Err("Temporary builds require interactive approval or --noconfirm".into());
    }
    print!("Proceed with temporary preparation? [y/N] ");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    if matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err("Temporary preparation cancelled".into())
    }
}

impl<'a> PackageRoot<'a> {
    pub fn cleanup(&mut self, manager: crate::progress::Manager) -> Result<(), String> {
        crate::progress::operation(manager, "Removing temporary build dependencies", || {
            self.guard.cleanup()
        })?;
        println!(":: Removed private build workspace and its temporary dependencies.");
        Ok(())
    }
    pub fn new(
        config: &'a Config,
        pkg: &str,
        options: &'a Options,
        bytes: u64,
    ) -> Result<Self, String> {
        crate::execution::init_signal_handlers();
        if unsafe { libc::geteuid() } == 0 {
            return Err("Temporary package preparation must run as a regular user".into());
        }
        super::check_capability()?;
        let overlay = true;
        let base = crate::scratch::choose(crate::scratch::policy(), bytes, 128)?;
        let prefix = options.prefix;
        let work = create_secure_temp_dir_in(&format!("{prefix}-{pkg}"), &base)?;
        let guard = SandboxGuard::new(work.clone());
        let root = work.join("root");
        fs::create_dir(&root).map_err(|e| e.to_string())?;
        fs::create_dir(work.join("downloads")).map_err(|e| e.to_string())?;
        let manager =
            AlpmManager::with_repo_order(&config.repo_order).map_err(|e| e.to_string())?;
        println!(":: Temporary app/build workspace: {} ({:?}); package files, dependencies and build caches are removed on exit.", work.display(), crate::scratch::policy());
        Ok(Self {
            view: BuildWorkspace {
                root: root.clone(),
                overlay,
                readonly_paths: vec![PathBuf::from(&crate::pacman_config::snapshot()?.option("DBPath")[0])],
            },
            root,
            work,
            manager,
            config,
            options,
            guard,
            archives: Vec::new(),
            names: HashSet::new(),
            building: HashSet::new(),
            resolving: HashSet::new(),
            build_failed: false,
        })
    }

    pub fn capacity(&self, bytes: u64, inodes: u64) -> Result<(), String> {
        let capacity = crate::scratch::capacity(&self.work)?;
        let memory = crate::scratch::mem_available_bytes().unwrap_or(0);
        if crate::scratch::accepts(crate::scratch::policy(), capacity)
            && crate::scratch::fits(capacity, bytes, inodes, memory)
        {
            Ok(())
        } else {
            Err(format!(
                "Insufficient temporary workspace capacity: {}",
                crate::scratch::describe(capacity, bytes, inodes, memory)
            ))
        }
    }

    pub fn satisfied(&self, dependency: &str) -> Result<bool, String> {
        if dependency.contains('\0') {
            return Err("Invalid dependency".into());
        }
        if self
            .archives
            .iter()
            .any(|(_, meta)| crate::db::satisfies_spec(meta, dependency))
        {
            return Ok(true);
        }
        Ok(self
            .manager
            .handle()
            .localdb()
            .pkgs()
            .into_iter()
            .filter(|package| !self.names.contains(package.name()))
            .any(|package| crate::db::package_satisfies(package, dependency)))
    }

    pub fn ensure_dependencies(&mut self, dependencies: &[String]) -> Result<(), String> {
        for dependency in dependencies {
            self.ensure_dependency(dependency)?;
        }
        Ok(())
    }

    fn ensure_dependency(&mut self, dependency: &str) -> Result<(), String> {
        if !crate::utils::is_safe_pkg_name(crate::aur::clean_dep_name(dependency))
            || dependency.chars().any(char::is_control)
        {
            return Err("Invalid temporary dependency identity".into());
        }
        if self.satisfied(dependency)? {
            return Ok(());
        }
        let name = crate::aur::clean_dep_name(dependency).to_owned();
        if self.resolving.len() >= 64 || !self.resolving.insert(name.clone()) {
            return Err(format!(
                "Cyclic or excessively deep temporary dependency: {dependency}"
            ));
        }
        let result = self.resolve_dependency(dependency);
        self.resolving.remove(&name);
        result
    }

    fn resolve_dependency(&mut self, dependency: &str) -> Result<(), String> {
        if crate::db::find_sync_package(self.manager.handle(), dependency).is_ok() {
            let targets = self.manager.resolve_download_urls(None, dependency)?;
            let (bytes, inodes) = super::downloads::ephemeral_footprint(&targets);
            self.capacity(bytes, inodes)?;
            let archives = super::downloads::fetch_targets(
                &targets,
                self.options,
                &self.work.join("downloads"),
                &mut self.guard,
            )?;
            for (target, archive) in targets.iter().zip(archives) {
                if !self.names.contains(&target.name) {
                    self.add_archive(&archive, &target.name)?;
                }
            }
        } else {
            self.build_aur(crate::aur::clean_dep_name(dependency))?;
        }
        if !self.satisfied(dependency)? {
            return Err(format!("Temporary packages do not satisfy '{dependency}'; virtual AUR providers must be installed or selected explicitly before this command"));
        }
        Ok(())
    }

    pub fn build_aur(&mut self, pkg: &str) -> Result<PathBuf, String> {
        for (archive, meta) in &self.archives {
            if meta.name == pkg {
                return Ok(archive.clone());
            }
        }
        if self.building.len() >= 32 || !self.building.insert(pkg.into()) {
            return Err(format!(
                "Cyclic or excessively deep temporary AUR dependency: {pkg}"
            ));
        }
        let result = self.build_aur_inner(pkg);
        self.building.remove(pkg);
        result
    }

    fn build_aur_inner(&mut self, pkg: &str) -> Result<PathBuf, String> {
        let item = crate::aur::query_aur_checked(&[pkg.into()])?
            .remove(pkg)
            .ok_or_else(|| {
                format!("No exact AUR package '{pkg}'; ambiguous virtual providers are not guessed")
            })?;
        let base = item.package_base.as_deref().unwrap_or(pkg);
        self.capacity(128 * 1024 * 1024, 128)?;
        let work = create_secure_temp_dir_in("aur-build", &self.work)?;
        let recipe = crate::pm::fetch_aur_recipe(self.config, base, &work, &self.view)?;
        let (mut dependencies, text) = crate::pm::aur_recipe_inputs(&recipe, pkg)?;
        println!(
            ":: AUR recipe for {pkg} ({base}/PKGBUILD):\n{}",
            crate::utils::sanitize_display_text(&text)
        );
        confirm_build(
            &format!("Build aur/{pkg}; dependencies: {}", dependencies.join(", ")),
            self.options.noconfirm,
        )?;
        dependencies.push("base-devel".into());
        self.ensure_dependencies(&dependencies)?;
        self.capacity(128 * 1024 * 1024, 128)?;
        let archives =
            crate::pm::build_aur_archive(&recipe, &work, &self.view).map_err(|error| {
                self.build_failed |= error.build_failed;
                error.to_string()
            })?;
        // Split outputs are inspected, but only the requested package is merged.
        let mut target = None;
        for archive in archives {
            let package = self
                .manager
                .handle()
                .pkg_load(
                    archive.to_string_lossy().as_ref(),
                    true,
                    alpm::SigLevel::NONE,
                )
                .map_err(|e| e.to_string())?;
            if package.name() == pkg {
                if target.is_some() {
                    return Err("Multiple AUR archives match requested package".into());
                }
                target = Some(archive);
            }
        }
        let target = target.ok_or("AUR build did not produce the requested package identity")?;
        self.add_archive(&target, pkg)?;
        Ok(target)
    }

    pub fn add_archive(&mut self, archive: &Path, expected: &str) -> Result<(), String> {
        if self.names.len() >= 128 {
            return Err("Temporary package graph exceeds 128 packages".into());
        }
        let package = self
            .manager
            .handle()
            .pkg_load(
                archive.to_string_lossy().as_ref(),
                true,
                alpm::SigLevel::NONE,
            )
            .map_err(|e| format!("Cannot inspect temporary package: {e}"))?;
        if package.name() != expected || !self.manager.accepts_arch(package.arch()) {
            return Err(
                "Temporary archive identity or architecture does not match requested package"
                    .into(),
            );
        }
        if self.names.contains(expected) {
            return Err(format!(
                "Conflicting duplicate temporary package '{expected}'"
            ));
        }
        for file in package.files().files() {
            let name = std::str::from_utf8(file.name()).map_err(|_| "Non-UTF-8 package path")?;
            if !crate::download::is_safe_tar_entry(name) {
                return Err("Unsafe path in temporary package inventory".into());
            }
            match fs::symlink_metadata(self.root.join(name)) {
                Ok(meta) if !meta.is_dir() || !name.ends_with('/') => {
                    return Err(format!("Temporary dependency file conflict: {name}"))
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        let dependencies: Vec<String> = package
            .depends()
            .into_iter()
            .map(|dep| dep.to_string())
            .collect();
        let meta = crate::db::ArchiveMetadata {
            name: package.name().to_string(),
            version: package.version().as_str().to_string(),
            provides: package
                .provides()
                .into_iter()
                .map(|p| (p.name().to_string(), p.version().map(|v| v.as_str().to_string())))
                .collect(),
        };
        if package.has_scriptlet() {
            println!(":: {expected}: installation scripts/hooks are not executed by try/run; apps needing system integration may not work temporarily.");
        }
        drop(package);
        self.ensure_dependencies(&dependencies)?;
        let (bytes, inodes) = crate::archive::footprint(archive)?;
        self.capacity(
            bytes.saturating_add(32 * 1024 * 1024),
            inodes.saturating_add(32),
        )?;
        crate::progress::operation(
            crate::progress::Manager::Runtime,
            "Extracting temporary package",
            || crate::archive::extract_package(archive, &self.root),
        )?;
        self.archives.push((archive.into(), meta));
        self.names.insert(expected.into());
        Ok(())
    }

    pub fn validate_runtime(&self) -> Result<(), String> {
        // Revalidate every runtime dependency against the final combined root.
        // This detects a dependency displaced by a newer prepared package.
        let packages = self
            .archives
            .iter()
            .map(|(path, _)| {
                self.manager
                    .handle()
                    .pkg_load(path.to_string_lossy().as_ref(), true, alpm::SigLevel::NONE)
                    .map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let installed: Vec<_> = self
            .manager
            .handle()
            .localdb()
            .pkgs()
            .into_iter()
            .filter(|package| !self.names.contains(package.name()))
            .collect();
        let missing = self.manager.handle().check_deps(
            installed.iter(),
            std::iter::empty::<&alpm::Pkg>(),
            packages.iter(),
            false,
        );
        if let Some(missing) = missing.first() {
            return Err(format!(
                "Unsatisfied temporary runtime dependency: {} (required by {})",
                missing.depend(),
                missing.target()
            ));
        }
        Ok(())
    }

    /// Select only the requested packages and their actual runtime closure.
    /// Build-only archives never cross the installation boundary.
    pub fn installation_archives(&self, targets: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
        self.validate_runtime()?;
        let paths: Vec<PathBuf> = self.archives.iter().map(|(p, _)| p.clone()).collect();
        select_installation_archives(&self.manager, &paths, targets)
    }
}

fn select_installation_archives(
    manager: &AlpmManager,
    archives: &[PathBuf],
    targets: &[PathBuf],
) -> Result<Vec<PathBuf>, String> {
    let packages = archives
        .iter()
        .map(|path| {
            manager
                .handle()
                .pkg_load(
                    path.as_os_str().as_encoded_bytes().to_vec(),
                    true,
                    alpm::SigLevel::NONE,
                )
                .map(|package| (path, package))
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut pending = targets.to_vec();
    let mut selected = Vec::new();
    while let Some(path) = pending.pop() {
        if selected.contains(&path) {
            continue;
        }
        let (_, package) = packages
            .iter()
            .find(|(candidate, _)| **candidate == path)
            .ok_or("Requested archive is not owned by private preparation")?;
        selected.push(path);
        for dependency in package.depends() {
            let dependency = dependency.to_string();
            if manager
                .handle()
                .localdb()
                .pkgs()
                .find_satisfier(dependency.as_str())
                .is_some()
            {
                continue;
            }
            let (path, _) = packages
                .iter()
                .find(|(_, p)| crate::db::package_satisfies(p, &dependency))
                .ok_or_else(|| format!("Missing prepared runtime dependency: {dependency}"))?;
            pending.push((*path).clone());
        }
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{AlpmFixture, FixturePackage};
    fn package(directory: &Path, name: &str, depends: &[&str], provides: &[&str]) -> PathBuf {
        package_with_arch(directory, name, "any", depends, provides)
    }
    fn package_with_arch(directory: &Path, name: &str, arch: &str, depends: &[&str], provides: &[&str]) -> PathBuf {
        let path = directory.join(format!("{name}-{arch}.pkg.tar"));
        let mut archive = tar::Builder::new(fs::File::create(&path).unwrap());
        let mut info = format!("pkgname = {name}\npkgbase = {name}\npkgver = 2-1\npkgdesc = fixture\nsize = 1\narch = {arch}\nlicense = custom\n");
        for value in depends {
            info += &format!("depend = {value}\n");
        }
        for value in provides {
            info += &format!("provides = {value}\n");
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(info.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, ".PKGINFO", info.as_bytes())
            .unwrap();
        archive.finish().unwrap();
        path
    }
    #[test]
    fn installation_selects_runtime_closure_without_build_only_packages() {
        let fixture = AlpmFixture::new(&[], &[]);
        let directory = PathBuf::from(fixture.manager.handle().dbpath());
        let app = package(&directory, "app", &["runtime>=2"], &[]);
        let runtime = package(&directory, "runtime", &[], &[]);
        let compiler = package(&directory, "compiler", &[], &[]);
        let archives = vec![compiler.clone(), app.clone(), runtime.clone()];
        let selected =
            select_installation_archives(&fixture.manager, &archives, std::slice::from_ref(&app))
                .unwrap();
        assert_eq!(selected, [app, runtime]);
        assert!(!selected.contains(&compiler));
    }
    #[test]
    fn installed_dependencies_are_reused_and_missing_or_unowned_archives_fail() {
        let fixture = AlpmFixture::new(&[FixturePackage::new("runtime", "2-1")], &[]);
        let directory = PathBuf::from(fixture.manager.handle().dbpath());
        let app = package(&directory, "app", &["runtime>=2"], &[]);
        let archives = vec![app.clone()];
        assert_eq!(
            select_installation_archives(&fixture.manager, &archives, &archives).unwrap(),
            archives
        );
        let missing = package(&directory, "missing-app", &["absent"], &[]);
        assert!(select_installation_archives(
            &fixture.manager,
            std::slice::from_ref(&missing),
            std::slice::from_ref(&missing)
        )
        .is_err());
        assert!(select_installation_archives(
            &fixture.manager,
            &archives,
            &[directory.join("foreign.pkg.tar")]
        )
        .is_err());
    }
    #[test]
    fn runtime_virtual_provisions_keep_constraints() {
        let fixture = AlpmFixture::new(&[], &[]);
        let directory = PathBuf::from(fixture.manager.handle().dbpath());
        let app = package(&directory, "app", &["virtual>=2"], &[]);
        let provider = package(&directory, "provider", &[], &["virtual=2"]);
        let archives = vec![app.clone(), provider.clone()];
        assert_eq!(
            select_installation_archives(&fixture.manager, &archives, std::slice::from_ref(&app))
                .unwrap(),
            [app.clone(), provider]
        );
        let too_old = package(&directory, "old-provider", &[], &["virtual=1"]);
        assert!(
            select_installation_archives(&fixture.manager, &[app.clone(), too_old], &[app])
                .is_err()
        );
    }
    #[test]
    fn add_archive_respects_configured_architectures() {
        let fixture = AlpmFixture::new(&[], &[]);
        let directory = PathBuf::from(fixture.manager.handle().dbpath());
        let pkg_v3 = package_with_arch(&directory, "v3pkg", "x86_64_v3", &[], &[]);

        let work = directory.join("test-work");
        let root_dir = work.join("root");
        fs::create_dir_all(&root_dir).unwrap();

        let handle = alpm::Alpm::new(directory.to_str().unwrap(), directory.to_str().unwrap()).unwrap();
        let manager = AlpmManager::for_test(handle, vec!["x86_64".into()]);

        let config = Config::default();
        let options = Options {
            no_cache: false,
            allow_unverified: false,
            noconfirm: true,
            prefix: "test",
        };

        let mut root = PackageRoot {
            view: BuildWorkspace {
                root: root_dir.clone(),
                overlay: false,
                readonly_paths: Vec::new(),
            },
            root: root_dir,
            work: work.clone(),
            manager,
            config: &config,
            options: &options,
            guard: SandboxGuard::new(work),
            archives: Vec::new(),
            names: HashSet::new(),
            building: HashSet::new(),
            resolving: HashSet::new(),
            build_failed: false,
        };

        // When manager does not have x86_64_v3 configured, add_archive fails:
        assert!(root.add_archive(&pkg_v3, "v3pkg").is_err());

        // When manager has x86_64_v3 configured, add_archive succeeds:
        root.manager.set_architectures_for_test(vec!["x86_64".into(), "x86_64_v3".into()]);
        assert!(root.add_archive(&pkg_v3, "v3pkg").is_ok());

        assert_eq!(root.archives.len(), 1);
        assert_eq!(root.archives[0].0, pkg_v3);
        assert_eq!(root.archives[0].1.name, "v3pkg");
        assert!(root.satisfied("v3pkg").unwrap());
    }
}
