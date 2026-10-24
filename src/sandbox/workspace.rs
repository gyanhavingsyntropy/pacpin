//! App workspace lifetime and conservative stale-workspace cleanup.
use std::{env, fs};
use std::path::{Path, PathBuf};

pub fn clean_stale_sandboxes() {
    if let Ok(xdg_runtime) = env::var("XDG_RUNTIME_DIR") {
        for sub in &["run", "try"] {
            let dir = PathBuf::from(&xdg_runtime).join("pacpin").join(sub);
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() && should_clean_stale_sandbox(&p) {
                        let _ = fs::remove_dir_all(&p);
                    }
                }
            }
        }
    }

    // Forced policies can select a base different from the default TMPDIR.
    let mut bases = crate::scratch::bases(crate::scratch::Policy::Ram);
    bases.extend(crate::scratch::bases(crate::scratch::Policy::Storage));
    bases.sort();
    bases.dedup();
    for base in bases {
    if let Ok(entries) = fs::read_dir(base) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if (name.starts_with("pacpin-run-") || name.starts_with("pacpin-try-") || name.starts_with("pacpin-build-"))
                    && p.is_dir()
                    && should_clean_stale_sandbox(&p)
                {
                    let _ = fs::remove_dir_all(&p);
                }
            }
        }
    }
    }
}

pub(super) fn should_clean_stale_sandbox(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else { return false; };
    if !metadata.is_dir() { return false; }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return false;
        }
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !name.starts_with("pacpin-run-") && !name.starts_with("pacpin-try-") && !name.starts_with("pacpin-build-") {
        return false;
    }
    if let Some((_, suffix)) = name.rsplit_once("-pid") {
        if let Some(pid) = suffix.split('-').next().and_then(|s| s.parse::<i32>().ok()) {
            if pid > 0 {
                #[cfg(unix)]
                return unsafe { libc::kill(pid, 0) != 0 }
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            }
        }
    }
    // Older pacpin directories have no PID. Leave recent ones alone because they
    // may still belong to a live process.
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age >= std::time::Duration::from_secs(24 * 60 * 60))
}

pub(super) use crate::preparation::{SandboxGuard, create_secure_temp_dir_in};

#[cfg(test)]
pub(super) fn create_secure_temp_dir(prefix: &str) -> Result<PathBuf, String> {
    let sub = if prefix.starts_with("pacpin-run") { "run" } else { "try" };
    let base_dir = env::var_os("XDG_RUNTIME_DIR")
        .and_then(|runtime| private_runtime_subdir(Path::new(&runtime), sub).ok())
        .unwrap_or_else(env::temp_dir);
    create_secure_temp_dir_in(prefix, &base_dir)
}

#[cfg(test)]
pub(super) fn private_runtime_subdir(runtime: &Path, sub: &str) -> Result<PathBuf, String> {
    #[cfg(unix)]
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    #[cfg(unix)]
    fn private_owned_dir(path: &Path) -> bool {
        let Ok(meta) = fs::symlink_metadata(path) else { return false; };
        meta.is_dir()
            && !meta.file_type().is_symlink()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.permissions().mode() & 0o077 == 0
    }

    #[cfg(not(unix))]
    fn private_owned_dir(path: &Path) -> bool {
        path.is_dir()
    }

    if !runtime.is_absolute() || !private_owned_dir(runtime) {
        return Err("XDG_RUNTIME_DIR must be a private directory owned by the current user".into());
    }
    let parent = runtime.join("pacpin");
    let child = parent.join(sub);
    for dir in [&parent, &child] {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        match builder.create(dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(format!("Cannot create private runtime directory: {err}")),
        }
        if !private_owned_dir(dir) {
            return Err(format!("Unsafe runtime directory: {}", dir.display()));
        }
    }
    Ok(child)
}
