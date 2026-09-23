use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use colored::Colorize;

/// Computes the SHA256 checksum of a file.
pub fn compute_sha256(path: &Path) -> Result<String, String> {
    let out = Command::new("sha256sum")
        .arg(path)
        .output()
        .map_err(|e| format!("Failed to run sha256sum: {}", e))?;

    if !out.status.success() {
        return Err(format!("sha256sum failed for '{}'", path.display()));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let hash = stdout.split_whitespace().next().unwrap_or("").to_lowercase();
    if hash.is_empty() {
        return Err(format!("Empty hash returned for '{}'", path.display()));
    }

    Ok(hash)
}

/// Creates a secure temporary file with 0600 permissions.
pub fn create_secure_temp_file(dir: &Path, prefix: &str, ext: &str) -> Result<PathBuf, String> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

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
        let target = dir.join(format!("{}-{}.{}", prefix, hex, ext));

        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600);

        if opts.open(&target).is_ok() {
            return Ok(target);
        }
    }
    Err("Failed to create secure temporary file".to_string())
}

/// Checks if an archive path entry is safe against directory traversal attacks.
pub fn is_safe_tar_entry(entry: &str) -> bool {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Absolute paths or drive letters
    if trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return false;
    }
    // Path traversal components
    for component in trimmed.split(['/', '\\']) {
        if component == ".." {
            return false;
        }
    }
    true
}

/// Validates whether a relative symlink destination remains strictly inside the target root.
pub fn is_safe_link_destination(src: &str, dst: &str) -> bool {
    let trimmed_dst = dst.trim();
    if trimmed_dst.is_empty() {
        return false;
    }
    // No absolute paths or drive letters
    if trimmed_dst.starts_with('/') || trimmed_dst.starts_with('\\') {
        return false;
    }
    if trimmed_dst.len() >= 2
        && trimmed_dst.chars().next().unwrap().is_ascii_alphabetic()
        && trimmed_dst.chars().nth(1) == Some(':')
    {
        return false;
    }

    // Calculate directory depth of the link's source relative to sandbox root
    let src_trimmed = src.trim().trim_start_matches(['/', '\\']);
    let parent_components: Vec<&str> = src_trimmed
        .split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    let mut depth: isize = if parent_components.is_empty() {
        0
    } else {
        (parent_components.len() - 1) as isize
    };

    for comp in trimmed_dst.split(['/', '\\']) {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." {
            depth -= 1;
            if depth < 0 {
                // Link traverses above the archive root!
                return false;
            }
        } else {
            depth += 1;
        }
    }

    true
}

/// Validates a single line of `tar -tvf` output.
pub fn is_safe_tar_listing_line(line: &str) -> Result<(String, u64), String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.is_empty() {
        return Ok((String::new(), 0));
    }
    if tokens.len() < 6 {
        // Shorter output: fallback to raw entry check
        let entry = tokens.last().unwrap_or(&"");
        if !is_safe_tar_entry(entry) {
            return Err(format!("Unsafe archive path: {}", entry));
        }
        return Ok((entry.to_string(), 0));
    }

    let size = tokens[2].parse::<u64>().unwrap_or(0);
    let full_path = tokens[5..].join(" ");

    if let Some((src, dst)) = full_path.split_once(" -> ") {
        if !is_safe_tar_entry(src) {
            return Err(format!("Unsafe symlink source entry: {}", src));
        }
        if !is_safe_link_destination(src, dst) {
            return Err(format!("Unsafe symlink destination '{}' points outside sandbox root", dst));
        }
        Ok((src.to_string(), size))
    } else if let Some((src, dst)) = full_path.split_once(" link to ") {
        if !is_safe_tar_entry(src) {
            return Err(format!("Unsafe hardlink source entry: {}", src));
        }
        if !is_safe_link_destination(src, dst) {
            return Err(format!("Unsafe hardlink destination '{}' points outside sandbox root", dst));
        }
        Ok((src.to_string(), size))
    } else {
        if !is_safe_tar_entry(&full_path) {
            return Err(format!("Unsafe path traversal entry: {}", full_path));
        }
        Ok((full_path, size))
    }
}

/// Downloads or retrieves a verified package archive from local pacman cache or mirrors.
pub fn fetch_package_archive(
    pkg: &str,
    candidate_urls: &[String],
    expected_sha: Option<&str>,
    allow_unverified: bool,
    downloaded_tarballs: &mut Vec<PathBuf>,
) -> Result<PathBuf, String> {
    // 1. Check local pacman cache first
    let cache_dirs = crate::journal::TransactionJournal::get_cache_dirs();
    for cache_dir in cache_dirs {
        if let Ok(entries) = fs::read_dir(&cache_dir) {
            let mut matching: Vec<PathBuf> = entries
                .flatten()
                .filter(|e| {
                    let file_name = e.file_name();
                    let name_str = file_name.to_string_lossy();
                    (name_str.ends_with(".pkg.tar.zst") || name_str.ends_with(".pkg.tar.xz"))
                        && (name_str.starts_with(&format!("{}-", pkg)))
                })
                .map(|e| e.path())
                .collect();

            if !matching.is_empty() {
                matching.sort_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok());
                let found = matching.last().unwrap().clone();

                let cache_valid = if let Some(expected) = expected_sha {
                    if let Ok(computed) = compute_sha256(&found) {
                        computed.eq_ignore_ascii_case(expected)
                    } else {
                        false
                    }
                } else {
                    allow_unverified
                };

                if cache_valid {
                    println!("{} Found verified '{}' in local pacman cache.", "::".cyan(), pkg);
                    return Ok(found);
                } else if expected_sha.is_some() {
                    println!(
                        "{} Cached package for '{}' failed checksum verification or was outdated; downloading fresh...",
                        "::".yellow(),
                        pkg
                    );
                }
            }
        }
    }

    // 2. Fail-closed: verify checksum availability unless user explicitly opted out
    let expected = match expected_sha {
        Some(s) => s,
        None if allow_unverified => {
            println!(
                "{} Warning: No SHA256 checksum in repository metadata for '{}'; proceeding (--allow-unverified).",
                "::".yellow(),
                pkg
            );
            ""
        }
        None => {
            return Err(format!(
                "Security violation: Repository metadata for '{}' does not provide a SHA256 checksum.\n  \
                Fail-closed policy rejects unverified package downloads.\n  \
                To bypass integrity verification at your own risk, re-run with --allow-unverified.",
                pkg
            ));
        }
    };

    if candidate_urls.is_empty() {
        return Err(format!("No download URLs configured for package '{}'", pkg));
    }

    // 3. Download with automatic mirror failover and SHA256 integrity verification
    let mut last_err = String::new();
    for (i, url) in candidate_urls.iter().enumerate() {
        if i > 0 {
            println!(
                "{} Retrying download from backup mirror {}...",
                "::".yellow(),
                url.dimmed()
            );
        } else {
            println!("{} Fetching '{}' from {}...", "::".cyan(), pkg, url.dimmed());
        }

        let ext = if url.ends_with(".pkg.tar.xz") {
            "pkg.tar.xz"
        } else {
            "pkg.tar.zst"
        };

        let temp_download = create_secure_temp_file(
            &env::temp_dir(),
            &format!("pacpin-download-{}", pkg),
            ext,
        )?;
        downloaded_tarballs.push(temp_download.clone());

        // -f (--fail) ensures curl fails on HTTP 4xx/5xx rather than writing HTML error pages
        let curl_status = Command::new("curl")
            .args(["-sSLf", "-o", temp_download.to_str().unwrap(), url])
            .status();

        match curl_status {
            Ok(s) if s.success() => {
                if !expected.is_empty() {
                    match compute_sha256(&temp_download) {
                        Ok(computed) => {
                            if computed.eq_ignore_ascii_case(expected) {
                                return Ok(temp_download);
                            } else {
                                last_err = format!(
                                    "SHA256 checksum mismatch (expected: {}, computed: {})",
                                    expected, computed
                                );
                                let _ = fs::remove_file(&temp_download);
                                println!(
                                    "{} Warning: Mirror '{}' returned invalid checksum. Trying next mirror...",
                                    "::".yellow(),
                                    url
                                );
                            }
                        }
                        Err(e) => {
                            last_err = format!("Failed to compute SHA256: {}", e);
                            let _ = fs::remove_file(&temp_download);
                        }
                    }
                } else {
                    return Ok(temp_download);
                }
            }
            Ok(s) => {
                last_err = format!("HTTP download failed with exit code {}", s.code().unwrap_or(1));
                let _ = fs::remove_file(&temp_download);
                println!(
                    "{} Warning: Mirror '{}' failed. Trying next mirror...",
                    "::".yellow(),
                    url
                );
            }
            Err(e) => {
                last_err = format!("Failed to spawn curl: {}", e);
                let _ = fs::remove_file(&temp_download);
                println!(
                    "{} Warning: Failed to connect to '{}'. Trying next mirror...",
                    "::".yellow(),
                    url
                );
            }
        }
    }

    Err(format!(
        "Failed to download verified package '{}' from all candidate mirrors: {}",
        pkg, last_err
    ))
}

pub const MAX_ARCHIVE_ENTRIES: usize = 50_000;
pub const MAX_UNPACKED_BYTES: u64 = 5 * 1024 * 1024 * 1024; // 5 GB limit

/// Safely inspects and extracts a package tarball into a destination directory.
pub fn extract_archive(archive_path: &Path, target_dir: &Path) -> Result<(), String> {
    let tar_tvf = Command::new("tar")
        .args(["-tvf", archive_path.to_str().unwrap()])
        .output()
        .map_err(|e| format!("Failed to inspect archive '{}': {}", archive_path.display(), e))?;

    if !tar_tvf.status.success() {
        return Err(format!("Failed to list archive contents for '{}'", archive_path.display()));
    }

    let stdout = String::from_utf8_lossy(&tar_tvf.stdout);
    let mut total_entries = 0usize;
    let mut total_size = 0u64;

    for line in stdout.lines() {
        let line_trimmed = line.trim();
        if line_trimmed.is_empty() {
            continue;
        }
        total_entries += 1;
        if total_entries > MAX_ARCHIVE_ENTRIES {
            return Err(format!(
                "Security violation: archive '{}' exceeds maximum allowed file count ({})",
                archive_path.display(),
                MAX_ARCHIVE_ENTRIES
            ));
        }

        let (_entry, size) = is_safe_tar_listing_line(line_trimmed).map_err(|e| {
            format!(
                "Security violation in archive '{}': {}. Extraction aborted.",
                archive_path.display(),
                e
            )
        })?;

        total_size = total_size.saturating_add(size);
        if total_size > MAX_UNPACKED_BYTES {
            return Err(format!(
                "Security violation: archive '{}' uncompressed size exceeds safety limit of 5 GB. Extraction aborted.",
                archive_path.display()
            ));
        }
    }

    let extract_status = Command::new("tar")
        .args([
            "-xf",
            archive_path.to_str().unwrap(),
            "-C",
            target_dir.to_str().unwrap(),
            "--no-same-owner",
            "--no-same-permissions",
            "--delay-directory-restore",
        ])
        .status()
        .map_err(|e| format!("Failed to execute tar extraction: {}", e))?;

    if !extract_status.success() {
        return Err(format!("Failed to extract package archive '{}'", archive_path.display()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safe_tar_entry() {
        assert!(is_safe_tar_entry("usr/bin/foo"));
        assert!(is_safe_tar_entry("etc/config.toml"));
        assert!(!is_safe_tar_entry("/etc/shadow"));
        assert!(!is_safe_tar_entry("../../../etc/shadow"));
        assert!(!is_safe_tar_entry("usr/../bin/foo"));
    }

    #[test]
    fn test_is_safe_link_destination() {
        assert!(is_safe_link_destination("usr/bin/foo", "bar"));
        assert!(is_safe_link_destination("usr/bin/foo", "../lib/libfoo.so"));
        assert!(!is_safe_link_destination("usr/bin/foo", "../../../etc/shadow"));
        assert!(!is_safe_link_destination("usr/bin/foo", "/etc/shadow"));
    }
}
