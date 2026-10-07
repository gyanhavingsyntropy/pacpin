use std::collections::HashSet;
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


/// Parses the package name from an Arch Linux package archive filename.
/// Follows standard format: `<pkgname>-<pkgver>-<pkgrel>-<arch>.pkg.tar.<ext>`.
pub fn parse_pkgname_from_filename(filename: &str) -> Option<&str> {
    let stem = filename
        .strip_suffix(".pkg.tar.zst")
        .or_else(|| filename.strip_suffix(".pkg.tar.xz"))
        .or_else(|| filename.strip_suffix(".pkg.tar.gz"))
        .or_else(|| filename.strip_suffix(".pkg.tar.bz2"))
        .unwrap_or(filename);
    let parts: Vec<&str> = stem.rsplitn(4, '-').collect();
    if parts.len() == 4 {
        Some(parts[3])
    } else {
        None
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
                    (name_str.ends_with(".pkg.tar.zst")
                        || name_str.ends_with(".pkg.tar.xz")
                        || name_str.ends_with(".pkg.tar.gz"))
                        && parse_pkgname_from_filename(&name_str) == Some(pkg)
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
        if crate::sandbox::is_terminating() {
            return Err("Download interrupted during shutdown".into());
        }
        if i > 0 {
            println!(
                "{} Retrying download from backup mirror {}...",
                "::".yellow(),
                url.dimmed()
            );
        } else {
            println!("{} Fetching '{}' from {}...", "::".cyan(), pkg, url.dimmed());
        }

        let ext = archive_extension(url);

        let temp_download = create_secure_temp_file(
            &env::temp_dir(),
            &format!("pacpin-download-{}", pkg),
            ext,
        )?;
        crate::sandbox::register_cleanup_target(temp_download.clone());
        downloaded_tarballs.push(temp_download.clone());

        // -f (--fail) ensures curl fails on HTTP 4xx/5xx rather than writing HTML error pages
        let curl_status = crate::sandbox::run_tracked_command(
            &mut package_download_command(url, &temp_download)
        );
        if crate::sandbox::is_terminating() {
            return Err("Download interrupted during shutdown".into());
        }

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

/// An archive member must not be unpacked through a symlink left by an earlier member.
pub(crate) fn reject_existing_symlink_components(root: &Path, relative: &Path) -> Result<(), String> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => current.push(part),
            std::path::Component::CurDir => continue,
            _ => return Err(format!("Unsafe archive path: {}", relative.display())),
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!("Archive path traverses a symlink: {}", current.display()));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(format!("Cannot inspect archive path '{}': {}", current.display(), e)),
        }
    }
    Ok(())
}

fn archive_extension(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    if path.ends_with(".pkg.tar.xz") {
        "pkg.tar.xz"
    } else if path.ends_with(".pkg.tar.gz") {
        "pkg.tar.gz"
    } else {
        "pkg.tar.zst"
    }
}

fn package_download_command(url: &str, output: &Path) -> Command {
    let mut command = Command::new("curl");
    // Ignore curlrc and URL globbing: one requested mirror must write one owned
    // file. A mirror string must never be interpreted as a curl option.
    command.args(["-q", "-g", "-sSLf", "--connect-timeout", "15", "--speed-time", "60", "--speed-limit", "1", "-o"])
        .arg(output).arg("--").arg(url);
    command
}

/// Safely inspects and extracts a package tarball into a destination directory.
pub fn extract_archive(archive_path: &Path, target_dir: &Path) -> Result<(), String> {
    crate::archive::extract(archive_path, target_dir, MAX_ARCHIVE_ENTRIES, MAX_UNPACKED_BYTES)?;

    // Verify uncompressed size on disk does not exceed safety limit (decompression bomb protection)
    fn verify_disk_size(dir: &Path, current_total: &mut u64, limit: u64) -> Result<(), String> {
        let entries = fs::read_dir(dir).map_err(|e| format!("Cannot verify extracted directory: {e}"))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("Cannot verify extracted entry: {e}"))?;
            let ft = entry.file_type().map_err(|e| format!("Cannot verify extracted file type: {e}"))?;
            if ft.is_file() {
                let sz = entry.metadata().map_err(|e| format!("Cannot verify extracted file size: {e}"))?.len();
                *current_total = current_total.saturating_add(sz);
                if *current_total > limit {
                    return Err(format!(
                        "Security violation: extracted package size exceeds safety limit of 5 GB ({} bytes). Extraction aborted.",
                        limit
                    ));
                }
            } else if ft.is_dir() {
                verify_disk_size(&entry.path(), current_total, limit)?;
            }
        }
        Ok(())
    }

    let mut disk_size = 0u64;
    verify_disk_size(target_dir, &mut disk_size, MAX_UNPACKED_BYTES)?;

    validate_symlink_traversal(target_dir, 32)?;

    Ok(())
}

/// Traverses all symlinks in the extracted root to verify they do not escape the sandbox
/// root and do not contain circular loops or exceed maximum link depth.
pub fn validate_symlink_traversal(root: &Path, max_depth: usize) -> Result<(), String> {
    fn walk_dir(current_dir: &Path, root: &Path, max_depth: usize) -> Result<(), String> {
        let entries = match fs::read_dir(current_dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };

        let root_components: Vec<_> = root
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(n) => Some(n),
                _ => None,
            })
            .collect();

        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };

            if file_type.is_symlink() {
                let mut current = path.clone();
                let mut visited = HashSet::new();
                let mut depth = 0;

                while let Ok(target) = fs::read_link(&current) {
                    depth += 1;
                    if depth > max_depth {
                        return Err(format!(
                            "Security violation: Symlink '{}' exceeds maximum link depth of {} (circular loop detected).",
                            path.display(),
                            max_depth
                        ));
                    }

                    let mut norm_components = if target.is_absolute() {
                        root_components.clone()
                    } else {
                        current
                            .parent()
                            .unwrap_or(root)
                            .components()
                            .filter_map(|c| match c {
                                std::path::Component::Normal(n) => Some(n),
                                _ => None,
                            })
                            .collect()
                    };

                    let mut escaped = false;
                    for comp in target.components() {
                        match comp {
                            std::path::Component::ParentDir => {
                                if norm_components.len() <= root_components.len() {
                                    escaped = true;
                                    break;
                                }
                                norm_components.pop();
                            }
                            std::path::Component::Normal(c) => {
                                norm_components.push(c);
                            }
                            _ => {}
                        }
                    }

                    if escaped || !norm_components.starts_with(&root_components) {
                        return Err(format!(
                            "Security violation: Symlink '{}' resolves to path outside sandbox root.",
                            path.display()
                        ));
                    }

                    let mut resolved = PathBuf::from("/");
                    for c in &norm_components {
                        resolved.push(c);
                    }

                    if !visited.insert(resolved.clone()) {
                        return Err(format!(
                            "Security violation: Circular symlink loop detected involving '{}'.",
                            path.display()
                        ));
                    }

                    if resolved.is_symlink() {
                        current = resolved;
                    } else {
                        break;
                    }
                }
            } else if file_type.is_dir() {
                walk_dir(&path, root, max_depth)?;
            }
        }
        Ok(())
    }

    walk_dir(root, root, max_depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = env::temp_dir().join(format!("pacpin-test-{}-{}-{}", label, std::process::id(), nonce));
        fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn test_rejects_existing_symlink_in_archive_path() {
        let root = test_dir("path-link");
        fs::create_dir(root.join("usr")).unwrap();
        #[cfg(unix)] {
            std::os::unix::fs::symlink("../other", root.join("usr/link")).unwrap();
            assert!(reject_existing_symlink_components(&root, Path::new("usr/link/payload")).is_err());
        }
        assert!(reject_existing_symlink_components(&root, Path::new("usr/new/payload")).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_extract_rejects_special_archive_entries() {
        let root = test_dir("special-entry");
        let archive_path = root.join("special.tar");
        let mut builder = tar::Builder::new(fs::File::create(&archive_path).unwrap());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Char);
        header.set_size(0);
        header.set_cksum();
        builder.append_data(&mut header, "dev/hostile", std::io::empty()).unwrap();
        builder.finish().unwrap();
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let result = extract_archive(&archive_path, &dest);
        assert!(result.unwrap_err().contains("unsupported archive entry type"));
        assert!(!dest.join("dev/hostile").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn download_urls_cannot_be_options_and_compression_suffix_is_preserved() {
        let command = package_download_command("--config=/tmp/hostile", Path::new("/tmp/archive"));
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args[0], "-q");
        assert_eq!(&args[args.len() - 2..], &["--", "--config=/tmp/hostile"]);
        assert!(args.windows(2).any(|w| w == ["--connect-timeout", "15"]));
        assert_eq!(archive_extension("https://mirror/pkg.pkg.tar.gz?download=1"), "pkg.tar.gz");
        assert_eq!(archive_extension("https://mirror/pkg.pkg.tar.xz"), "pkg.tar.xz");
        assert_eq!(archive_extension("https://mirror/pkg.pkg.tar.zst"), "pkg.tar.zst");
    }

    #[test]
    fn structured_extraction_rejects_disguised_hardlink_escape() {
        let root = test_dir("hardlink-header-escape");
        let archive_path = root.join("hostile.tar");
        fs::write(root.join("outside"), "unchanged").unwrap();
        let mut builder = tar::Builder::new(fs::File::create(&archive_path).unwrap());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Link);
        header.set_size(0);
        header.set_cksum();
        builder.append_link(&mut header, "a link to b", "../outside").unwrap();
        builder.finish().unwrap();
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        assert!(extract_archive(&archive_path, &dest).unwrap_err().contains("Unsafe link target"));
        assert_eq!(fs::read_to_string(root.join("outside")).unwrap(), "unchanged");
        assert!(!dest.join("a link to b").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn extraction_strips_setuid_and_setgid_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let root = test_dir("archive-privileged-mode");
        let archive_path = root.join("mode.tar");
        let mut builder = tar::Builder::new(fs::File::create(&archive_path).unwrap());
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o6755);
        header.set_size(1);
        header.set_cksum();
        builder.append_data(&mut header, "program", &b"x"[..]).unwrap();
        builder.finish().unwrap();
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        extract_archive(&archive_path, &dest).unwrap();
        assert_eq!(fs::metadata(dest.join("program")).unwrap().permissions().mode() & 0o6000, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_extract_rejects_malformed_archive() {
        let root = test_dir("bad-archive");
        let archive_path = root.join("bad.tar");
        fs::write(&archive_path, [0xff; 512]).unwrap();
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        assert!(extract_archive(&archive_path, &dest).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_extract_rejects_symlink_parent_from_earlier_entry() {
        let root = test_dir("archive-link-parent");
        let archive_path = root.join("linked.tar");
        let mut builder = tar::Builder::new(fs::File::create(&archive_path).unwrap());
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_cksum();
        builder.append_link(&mut link, "usr/link", "../other").unwrap();
        let mut file = tar::Header::new_gnu();
        file.set_size(7);
        file.set_cksum();
        builder.append_data(&mut file, "usr/link/payload", &b"payload"[..]).unwrap();
        builder.finish().unwrap();
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        assert!(extract_archive(&archive_path, &dest).unwrap_err().contains("traverses a symlink"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_extract_rejects_oversized_header_before_writing() {
        let root = test_dir("huge-header");
        let archive_path = root.join("huge.tar");
        let mut header = tar::Header::new_gnu();
        header.set_path("huge-file").unwrap();
        header.set_size(MAX_UNPACKED_BYTES + 1);
        header.set_cksum();
        fs::write(&archive_path, header.as_bytes()).unwrap();
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        assert!(extract_archive(&archive_path, &dest).unwrap_err().contains("uncompressed size exceeds"));
        assert!(!dest.join("huge-file").exists());
        fs::remove_dir_all(root).unwrap();
    }

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

    #[test]
    fn test_validate_symlink_traversal_safe() {
        let temp_dir = std::env::temp_dir().join(format!("pacpin-test-symlink-safe-{}", std::process::id()));
        let _ = fs::create_dir_all(temp_dir.join("usr/bin"));
        let _ = fs::create_dir_all(temp_dir.join("usr/lib"));

        let real_file = temp_dir.join("usr/bin/target_bin");
        let _ = fs::write(&real_file, b"content");

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let link = temp_dir.join("usr/bin/link_bin");
            let _ = symlink("target_bin", &link);
            assert!(validate_symlink_traversal(&temp_dir, 32).is_ok());
        }

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_validate_symlink_traversal_cycle_rejected() {
        let temp_dir = std::env::temp_dir().join(format!("pacpin-test-symlink-cycle-{}", std::process::id()));
        let _ = fs::create_dir_all(temp_dir.join("usr/bin"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let link_a = temp_dir.join("usr/bin/loop_a");
            let link_b = temp_dir.join("usr/bin/loop_b");
            let _ = symlink("loop_b", &link_a);
            let _ = symlink("loop_a", &link_b);

            let res = validate_symlink_traversal(&temp_dir, 32);
            assert!(res.is_err());
            assert!(res.unwrap_err().contains("Circular symlink loop"));
        }

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_validate_symlink_traversal_escaping_rejected() {
        let temp_dir = std::env::temp_dir().join(format!("pacpin-test-symlink-escape-{}", std::process::id()));
        let _ = fs::create_dir_all(temp_dir.join("usr/bin"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let link = temp_dir.join("usr/bin/bad_link");
            let _ = symlink("../../../../../../../../../etc/passwd", &link);

            let res = validate_symlink_traversal(&temp_dir, 32);
            assert!(res.is_err());
            assert!(res.unwrap_err().contains("outside sandbox root"));
        }

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_parse_pkgname_from_filename() {
        assert_eq!(
            parse_pkgname_from_filename("gcc-14.2.1-1-x86_64.pkg.tar.zst"),
            Some("gcc")
        );
        assert_eq!(
            parse_pkgname_from_filename("gcc-libs-14.2.1-1-x86_64.pkg.tar.zst"),
            Some("gcc-libs")
        );
        assert_eq!(
            parse_pkgname_from_filename("linux-headers-6.10.1.arch1-1-x86_64.pkg.tar.zst"),
            Some("linux-headers")
        );
        assert_eq!(
            parse_pkgname_from_filename("linux-6.10.1.arch1-1-x86_64.pkg.tar.xz"),
            Some("linux")
        );
        assert_eq!(
            parse_pkgname_from_filename("python-pip-24.0-1-any.pkg.tar.gz"),
            Some("python-pip")
        );
        assert_eq!(parse_pkgname_from_filename("malformed"), None);
    }

}
