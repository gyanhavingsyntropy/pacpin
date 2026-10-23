//! Private dependency roots. No ALPM transaction or system dependency install.
use super::{
    workspace::{create_secure_temp_dir_in, SandboxGuard},
    TryOptions,
};
use crate::{config::Config, db::AlpmManager, execution::BuildWorkspace};
use std::{
    collections::HashSet,
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};

pub(super) struct PackageRoot<'a> {
    pub root: PathBuf,
    pub work: PathBuf,
    pub manager: AlpmManager,
    pub view: BuildWorkspace,
    config: &'a Config,
    options: &'a TryOptions,
    guard: SandboxGuard,
    archives: Vec<PathBuf>,
    names: HashSet<String>,
    building: HashSet<String>,
    resolving: HashSet<String>,
}

pub(super) fn confirm_build(label: &str, noconfirm: bool) -> Result<(), String> {
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
    pub fn new(
        config: &'a Config,
        pkg: &str,
        options: &'a TryOptions,
        bytes: u64,
    ) -> Result<Self, String> {
        crate::execution::init_signal_handlers();
        if unsafe { libc::geteuid() } == 0 {
            return Err("Temporary package preparation must run as a regular user".into());
        }
        super::mounts::check_bwrap_capability()?;
        let overlay = crate::execution::supports_ro_overlay();
        if !overlay {
            return Err("AUR, Sources and export try/run require Bubblewrap read-only overlays; no system packages were changed".into());
        }
        let base = crate::scratch::choose(crate::scratch::policy(), bytes, 128)?;
        let prefix = if options.is_run_mode {
            "pacpin-run"
        } else {
            "pacpin-try"
        };
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
        let packages = self
            .archives
            .iter()
            .map(|path| {
                self.manager
                    .handle()
                    .pkg_load(path.to_string_lossy().as_ref(), true, alpm::SigLevel::NONE)
                    .map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        if packages
            .iter()
            .any(|package| crate::db::package_satisfies(package, dependency))
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
            let (bytes, inodes) = super::native::ephemeral_footprint(&targets);
            self.capacity(bytes, inodes)?;
            let archives = super::native::fetch_targets(
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
        let archives = crate::pm::build_aur_archive(&recipe, &work, &self.view)?;
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
        if package.name() != expected
            || package
                .arch()
                .is_none_or(|arch| arch != "any" && arch != std::env::consts::ARCH)
        {
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
        self.archives.push(archive.into());
        self.names.insert(expected.into());
        Ok(())
    }

    pub fn executable(&self, archive: &Path, pkg: &str) -> Result<PathBuf, String> {
        // Revalidate every runtime dependency against the final combined root.
        // This detects a dependency displaced by a newer prepared package.
        let packages = self
            .archives
            .iter()
            .map(|path| {
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
        let (_, _, owned) = crate::archive::executable_footprint(archive)?;
        let mut executables = Vec::new();
        for path in owned {
            let path = self.root.join(path);
            if !crate::utils::is_executable_file(&path) {
                continue;
            }
            let resolved = path.canonicalize().map_err(|e| e.to_string())?;
            if !resolved.starts_with(&self.root) {
                return Err("Temporary executable escapes its package root".into());
            }
            executables.push(path);
        }
        super::entrypoints::select_executable(executables, pkg, self.options.bin.as_deref())
    }
}
