//! App workspace lifetime and conservative stale-workspace cleanup.
use std::{env, fs};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use crate::execution::{register_cleanup_target, unregister_cleanup_target};

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
                if (name.starts_with("pacpin-run-") || name.starts_with("pacpin-try-"))
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
    if !name.starts_with("pacpin-run-") && !name.starts_with("pacpin-try-") {
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

pub(super) struct SandboxGuard {
    pub(super) sandbox_dir: PathBuf,
    pub(super) downloaded_tarballs: Vec<PathBuf>,
}

impl SandboxGuard {
    pub fn new(sandbox_dir: PathBuf) -> Self {
        register_cleanup_target(sandbox_dir.clone());
        Self {
            sandbox_dir,
            downloaded_tarballs: Vec::new(),
        }
    }

    pub fn register_tarball(&mut self, tarball: &Path) {
        let p = tarball.to_path_buf();
        if self.downloaded_tarballs.contains(&p) {
            register_cleanup_target(p);
        }
    }
}

impl Drop for SandboxGuard {
    fn drop(&mut self) {
        unregister_cleanup_target(&self.sandbox_dir);
        if self.sandbox_dir.exists() {
            let _ = fs::remove_dir_all(&self.sandbox_dir);
        }
        for tarball in &self.downloaded_tarballs {
            unregister_cleanup_target(tarball);
            if tarball.exists() {
                let _ = fs::remove_file(tarball);
            }
        }
    }
}

#[cfg(test)]
pub(super) fn create_secure_temp_dir(prefix: &str) -> Result<PathBuf, String> {
    let sub = if prefix.starts_with("pacpin-run") { "run" } else { "try" };
    let base_dir = env::var_os("XDG_RUNTIME_DIR")
        .and_then(|runtime| private_runtime_subdir(Path::new(&runtime), sub).ok())
        .unwrap_or_else(env::temp_dir);
    create_secure_temp_dir_in(prefix, &base_dir)
}

pub(super) fn create_secure_temp_dir_in(prefix: &str, base_dir: &Path) -> Result<PathBuf, String> {

    #[cfg(unix)]
    use std::os::unix::fs::DirBuilderExt;

    for _ in 0..100 {
        let mut random_bytes = [0u8; 16];
        if let Ok(mut f) = fs::File::open("/dev/urandom") {
            use std::io::Read;
            let _ = f.read_exact(&mut random_bytes);
        } else {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let pid = std::process::id() as u128;
            random_bytes = (nanos ^ (pid << 64)).to_le_bytes();
        }
        let hex = random_bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>();
        let target = base_dir.join(format!("{}-pid{}-{}", prefix, std::process::id(), hex));

        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);

        if builder.create(&target).is_ok() {
            return Ok(target);
        }
    }
    Err("Failed to create secure sandbox directory with exclusive permissions".to_string())
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
