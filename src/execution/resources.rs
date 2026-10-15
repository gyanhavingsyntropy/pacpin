//! Instance-owned cleanup registries. Tests do not drain the invocation registry.
use super::is_terminating;
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub(super) struct Registry(Mutex<Vec<PathBuf>>);
impl Registry {
    pub(super) const fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }
    pub(super) fn register(&self, path: PathBuf) {
        if is_terminating() {
            remove_cleanup_path(&path);
            return;
        }
        if let Ok(mut lock) = self.0.lock() {
            if is_terminating() {
                drop(lock);
                remove_cleanup_path(&path);
            } else if !lock.contains(&path) {
                lock.push(path);
            }
        }
    }

    pub(super) fn unregister(&self, path: &Path) {
        if let Ok(mut lock) = self.0.lock() {
            lock.retain(|p| p != path);
        }
    }

    pub(super) fn cleanup(&self) {
        if let Ok(mut lock) = self.0.lock() {
            for path in lock.drain(..) {
                remove_cleanup_path(&path);
            }
        }
    }
}

fn remove_cleanup_path(path: &Path) {
    if let Err(error) = remove_owned_tree(path) {
        eprintln!(
            "Warning: Cannot remove owned workspace {}: {error}",
            path.display()
        );
    }
}

/// Remove an owned private resource tree, including makepkg's mode-111 pkgdir
/// after build failure. Pin every directory before changing permissions; never
/// chmod symlink targets or descend into another filesystem.
pub fn remove_owned_tree(path: &Path) -> io::Result<()> {
    enum Task {
        Enter(PathBuf, Option<u64>),
        Exit(PathBuf, fs::File),
    }
    let mut pending = vec![Task::Enter(path.to_path_buf(), None)];
    while let Some(task) = pending.pop() {
        match task {
            Task::Exit(path, _directory) => fs::remove_dir(path)?,
            Task::Enter(path, device) => {
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                if !metadata.is_dir() {
                    fs::remove_file(path)?;
                    continue;
                }
                let directory = fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
                    .open(&path)?;
                let metadata = directory.metadata()?;
                if metadata.uid() != unsafe { libc::geteuid() }
                    || device.is_some_and(|device| device != metadata.dev())
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "refusing unowned directory or filesystem boundary",
                    ));
                }
                let pinned = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
                if metadata.permissions().mode() & 0o700 != 0o700 {
                    fs::set_permissions(
                        &pinned,
                        fs::Permissions::from_mode(metadata.permissions().mode() | 0o700),
                    )?;
                }
                // Keep the pinned parent alive until all child tasks finish.
                pending.push(Task::Exit(path, directory));
                for entry in fs::read_dir(&pinned)? {
                    pending.push(Task::Enter(entry?.path(), Some(metadata.dev())));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    #[test]
    fn restricted_build_directories_and_links_cleanup_without_touching_targets() {
        let root =
            std::env::temp_dir().join(format!("pacpin-cleanup-permissions-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let sentinel = root.join("sentinel");
        fs::write(&sentinel, "keep").unwrap();
        fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o600)).unwrap();
        let work = root.join("owned");
        fs::create_dir_all(work.join("pkg/nested")).unwrap();
        fs::write(work.join("pkg/nested/output"), "build").unwrap();
        std::os::unix::fs::symlink(&sentinel, work.join("outside")).unwrap();
        fs::set_permissions(work.join("pkg/nested"), fs::Permissions::from_mode(0o000)).unwrap();
        fs::set_permissions(work.join("pkg"), fs::Permissions::from_mode(0o111)).unwrap();
        remove_owned_tree(&work).unwrap();
        assert!(!work.exists());
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep");
        assert_eq!(
            fs::metadata(&sentinel).unwrap().permissions().mode() & 0o777,
            0o600
        );
        remove_owned_tree(&root).unwrap();
    }
}
