//! Bounded verified native-archive downloads shared by app and build preparation.
use super::{Options, SandboxGuard};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

pub(crate) fn fetch_targets(
    resolved: &[crate::db::DownloadTarget],
    options: &Options,
    sandbox_dir: &Path,
    guard: &mut SandboxGuard,
) -> Result<Vec<PathBuf>, String> {
    let total = resolved.iter().fold(0u64, |sum, target| {
        sum.saturating_add(target.download_bytes)
    });
    let cache_bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let measured_cache = cache_bytes.clone();
    let measured_dir = sandbox_dir.to_path_buf();
    crate::progress::operation_dynamic(
        crate::progress::Manager::Runtime,
        move || {
            let downloaded = fs::read_dir(&measured_dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("pacpin-download-")
                })
                .filter_map(|entry| entry.metadata().ok().map(|meta| meta.len()))
                .fold(measured_cache.load(Ordering::Relaxed), u64::saturating_add);
            ephemeral_download_label(downloaded, total)
        },
        || {
            download_targets_parallel(
                resolved,
                options.allow_unverified,
                guard,
                |target, allow_unverified, downloaded| {
                    let urls = if target.candidate_urls.is_empty() {
                        vec![target.url.clone()]
                    } else {
                        target.candidate_urls.clone()
                    };
                    let archive = crate::download::fetch_package_archive(
                        &target.name,
                        &urls,
                        target.sha256.as_deref(),
                        allow_unverified,
                        downloaded,
                        sandbox_dir,
                        !options.no_cache,
                    )?;
                    if archive.parent() != Some(sandbox_dir) {
                        cache_bytes.fetch_add(target.download_bytes, Ordering::Relaxed);
                    }
                    Ok(archive)
                },
            )
        },
    )
}

pub(crate) fn ephemeral_download_label(downloaded: u64, total: u64) -> String {
    let (scale, unit) = if total.max(downloaded) < 1_048_576 {
        (1024.0, "KiB")
    } else {
        (1_048_576.0, "MiB")
    };
    if total == 0 {
        return format!("Download/verify {:.1} {unit}", downloaded as f64 / scale);
    }
    format!(
        "Download/verify {:.1}/{:.1} {unit} ({}%)",
        downloaded as f64 / scale,
        total as f64 / scale,
        downloaded
            .saturating_mul(100)
            .checked_div(total)
            .unwrap_or(0)
            .min(100)
    )
}

pub(crate) fn ephemeral_footprint(targets: &[crate::db::DownloadTarget]) -> (u64, u64) {
    // Archives and extraction coexist. Metadata sizes provide an estimate,
    // not a reservation or a limit on the running app's memory.
    let bytes = targets.iter().fold(32 * 1024 * 1024u64, |sum, target| {
        sum.saturating_add(target.download_bytes)
            .saturating_add(target.installed_bytes.saturating_mul(2))
    });
    // Before downloading we know the number of archive files, not the number
    // of extracted paths. Count those through archive::footprint afterwards.
    (
        bytes,
        (targets.len() as u64).saturating_mul(2).saturating_add(128),
    )
}

pub(crate) fn download_targets_parallel<F>(
    targets: &[crate::db::DownloadTarget],
    allow_unverified: bool,
    guard: &mut SandboxGuard,
    fetch: F,
) -> Result<Vec<PathBuf>, String>
where
    F: Fn(&crate::db::DownloadTarget, bool, &mut Vec<PathBuf>) -> Result<PathBuf, String> + Sync,
{
    const MAX_PARALLEL_DOWNLOADS: usize = 2;
    let mut archives = Vec::with_capacity(targets.len());
    for batch in targets.chunks(MAX_PARALLEL_DOWNLOADS) {
        let mut first_error = None;
        std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|target| {
                    let fetch = &fetch;
                    scope.spawn(move || {
                        let mut downloaded = Vec::new();
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            fetch(target, allow_unverified, &mut downloaded)
                        }))
                        .unwrap_or_else(|_| Err("Package download worker panicked".to_string()));
                        (result, downloaded)
                    })
                })
                .collect();
            for handle in handles {
                match handle.join() {
                    Ok((result, downloaded)) => {
                        for path in downloaded {
                            guard.downloaded_tarballs.push(path.clone());
                            guard.register_tarball(&path);
                        }
                        match result {
                            Ok(path) => archives.push(path),
                            Err(error) if first_error.is_none() => first_error = Some(error),
                            Err(_) => {}
                        }
                    }
                    Err(_) if first_error.is_none() => {
                        first_error = Some("Package download worker panicked".to_string());
                    }
                    Err(_) => {}
                }
            }
        });
        if let Some(error) = first_error {
            return Err(error);
        }
    }
    Ok(archives)
}
