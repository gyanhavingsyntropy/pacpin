//! Owned private workspaces shared by builds and temporary applications.
use crate::execution::{register_cleanup_target, unregister_cleanup_target};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) struct SandboxGuard {
    pub(crate) sandbox_dir: PathBuf,
    pub(crate) downloaded_tarballs: Vec<PathBuf>,
    retained: bool,
}

impl SandboxGuard {
    pub fn new(sandbox_dir: PathBuf) -> Self {
        register_cleanup_target(sandbox_dir.clone());
        Self {
            sandbox_dir,
            downloaded_tarballs: Vec::new(),
            retained: false,
        }
    }

    pub fn register_tarball(&mut self, tarball: &Path) {
        let p = tarball.to_path_buf();
        if self.downloaded_tarballs.contains(&p) {
            register_cleanup_target(p);
        }
    }

    pub fn cleanup(&mut self) -> Result<(), String> {
        if self.sandbox_dir.exists() {
            crate::execution::remove_owned_tree(&self.sandbox_dir).map_err(|e| e.to_string())?;
        }
        unregister_cleanup_target(&self.sandbox_dir);
        for tarball in &self.downloaded_tarballs {
            if tarball.exists() {
                fs::remove_file(tarball).map_err(|e| e.to_string())?;
            }
            unregister_cleanup_target(tarball);
        }
        self.downloaded_tarballs.clear();
        Ok(())
    }

    /// Retain an owned tree only after a short atomic filesystem publication.
    pub fn publish(mut self, action: impl FnOnce() -> std::io::Result<()>) -> Result<(), String> {
        crate::execution::publish_cleanup_target(&self.sandbox_dir, action)
            .map_err(|e| e.to_string())?;
        self.retained = true;
        Ok(())
    }
}

impl Drop for SandboxGuard {
    fn drop(&mut self) {
        if self.retained {
            return;
        }
        if let Err(error) = self.cleanup() {
            eprintln!(
                ":: Warning: Cannot remove temporary workspace {}: {error}",
                self.sandbox_dir.display()
            );
        }
    }
}

pub(crate) fn create_secure_temp_dir_in(prefix: &str, base_dir: &Path) -> Result<PathBuf, String> {
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
        let hex = random_bytes
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>();
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
