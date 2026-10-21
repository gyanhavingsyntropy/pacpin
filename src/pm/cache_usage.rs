//! Bounded, read-only cache inspection. Never traverse build/download trees.
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default)]
pub struct CacheUsage {
    pub bytes: u64,
    pub complete: bool,
}

/// Logical file bytes, not guaranteed reclaimable filesystem space. Unknown
/// on unreadable/nested entries or an exhausted inspection budget.
pub fn native_cache_usage() -> CacheUsage {
    match crate::pacman_config::snapshot() {
        Ok(config) => inspect(&config.option("CacheDir").iter().map(PathBuf::from).collect::<Vec<_>>()),
        Err(_) => CacheUsage::default(),
    }
}

fn inspect(roots: &[PathBuf]) -> CacheUsage {
    let mut usage = CacheUsage { bytes: 0, complete: true };
    let mut seen = HashSet::new();
    let mut entries = 0;
    let deadline = Instant::now() + Duration::from_millis(100);
    for root in roots {
        let root = match root.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => { usage.complete = false; continue; }
        };
        if !seen.insert(root.clone()) { continue; }
        let directory = match std::fs::read_dir(root) {
            Ok(directory) => directory,
            Err(_) => { usage.complete = false; continue; }
        };
        for entry in directory {
            entries += 1;
            if entries > 20_000 || Instant::now() > deadline {
                usage.complete = false;
                return usage;
            }
            let metadata = entry.and_then(|entry| entry.metadata());
            match metadata {
                Ok(metadata) if metadata.is_file() => {
                    match usage.bytes.checked_add(metadata.len()) {
                        Some(bytes) => usage.bytes = bytes,
                        None => usage.complete = false,
                    }
                }
                Ok(metadata) if metadata.file_type().is_symlink() => {},
                _ => usage.complete = false,
            }
        }
    }
    usage
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usage_deduplicates_roots_and_does_not_follow_links_or_nested_trees() {
        let root = std::env::temp_dir().join(format!("pacpin-cache-usage-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("package.pkg.tar.zst"), b"12345").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("link")).unwrap();
        let usage = inspect(&[root.clone(), root.clone()]);
        assert_eq!(usage.bytes, 5);
        assert!(usage.complete);
        std::fs::create_dir(root.join("download-active")).unwrap();
        std::fs::write(root.join("download-active/partial"), b"123456789").unwrap();
        let usage = inspect(std::slice::from_ref(&root));
        assert_eq!(usage.bytes, 5);
        assert!(!usage.complete);
        assert!(root.join("download-active/partial").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
