//! Package extraction through the system libarchive, not textual tar listings.
//!
//! Only tar and native gzip/xz/zstd/bzip2 filters are enabled. All warnings fail
//! closed. No ownership, ACLs, xattrs (including capabilities), or special mode
//! bits are restored. The caller owns a private extraction directory and must
//! discard it on error; partial extraction is never suitable for execution.

use libarchive3_sys::ffi as la;
use std::collections::HashSet;
use std::ffi::{CStr, CString};
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::ptr::{self, NonNull};

use crate::download::{is_safe_link_destination, reject_existing_symlink_components};

// Stable libarchive APIs/constants newer than the raw binding crate. Keep this
// small compatibility shim here rather than enabling every decompression filter.
const ARCHIVE_EXTRACT_SAFE_WRITES: i32 = 0x40000;
extern "C" {
    fn archive_read_support_filter_zstd(archive: *mut la::Struct_archive) -> libc::c_int;
}

struct Archive {
    ptr: NonNull<la::Struct_archive>,
    writer: bool,
}

impl Archive {
    fn new(writer: bool) -> Result<Self, String> {
        // SAFETY: constructors have no prerequisites and return an owned handle.
        let ptr = unsafe {
            if writer {
                la::archive_write_disk_new()
            } else {
                la::archive_read_new()
            }
        };
        Ok(Self {
            ptr: NonNull::new(ptr).ok_or("Cannot allocate libarchive handle")?,
            writer,
        })
    }

    fn raw(&self) -> *mut la::Struct_archive {
        self.ptr.as_ptr()
    }

    fn check(&self, status: i32, operation: &str) -> Result<(), String> {
        if status == la::ARCHIVE_OK {
            return Ok(());
        }
        // SAFETY: handle is live; libarchive owns this optional NUL-terminated
        // string. Copy it before any further call can invalidate it.
        let error = unsafe {
            let text = la::archive_error_string(self.raw());
            if text.is_null() {
                "unknown libarchive error".into()
            } else {
                CStr::from_ptr(text).to_string_lossy().into_owned()
            }
        };
        Err(format!(
            "{}: {}",
            operation,
            crate::utils::sanitize_display_text(&error)
        ))
    }
}

impl Drop for Archive {
    fn drop(&mut self) {
        // SAFETY: this wrapper exclusively owns the handle and frees it once.
        unsafe {
            if self.writer {
                la::archive_write_free(self.raw());
            } else {
                la::archive_read_free(self.raw());
            }
        }
    }
}

/// Copy a header string while its reader-owned entry is still valid.
unsafe fn header_text(text: *const libc::c_char) -> Result<Option<String>, String> {
    if text.is_null() {
        return Ok(None);
    }
    // SAFETY: caller supplies a live libarchive entry's NUL-terminated field.
    let text = unsafe { CStr::from_ptr(text) }
        .to_str()
        .map_err(|_| "Archive contains a non-UTF-8 path; refusing lossy conversion")?;
    Ok(Some(text.to_owned()))
}

fn relative_path(text: &str) -> Result<PathBuf, String> {
    let path = Path::new(text);
    if text.is_empty()
        || !crate::download::is_safe_tar_entry(text)
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        || path.components().count() > 128
    {
        return Err("Unsafe archive path".into());
    }
    // Normalize '.' and repeated separators without resolving symlinks.
    Ok(path
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect())
}

fn native_path(path: &Path) -> Result<CString, String> {
    // libarchive's very-long-path fallback can change cwd internally. Disallow
    // it in this multithreaded process, and bound filesystem traversal depth.
    if path.as_os_str().as_bytes().len() >= libc::PATH_MAX as usize {
        return Err("Archive path exceeds filesystem path limit".into());
    }
    CString::new(path.as_os_str().as_bytes()).map_err(|_| "NUL in archive path".into())
}

/// Inspect logical payload sizes before allocating extraction/build space.
/// This writes no files and uses the same native filters and bounds as extraction.
pub(crate) fn footprint(archive_path: &Path) -> Result<(u64, u64), String> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(archive_path)
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Package archive must be a regular file".into());
    }
    let reader = Archive::new(false)?;
    let mut bytes = 0u64;
    let mut logical_bytes = 0u64;
    let mut paths = HashSet::new();
    let mut entries = 0usize;
    // SAFETY: reader owns its handle, file stays open until reader is dropped,
    // and each reader-owned entry is consumed before advancing to the next.
    unsafe {
        reader.check(
            la::archive_read_support_format_tar(reader.raw()),
            "Enable tar reader",
        )?;
        for enable in [
            la::archive_read_support_filter_gzip,
            la::archive_read_support_filter_xz,
            la::archive_read_support_filter_bzip2,
            archive_read_support_filter_zstd,
        ] {
            reader.check(enable(reader.raw()), "Enable native archive decompression")?;
        }
        reader.check(
            la::archive_read_open_fd(reader.raw(), file.as_raw_fd(), 64 * 1024),
            "Open package archive",
        )?;
        loop {
            if crate::sandbox::is_terminating() {
                return Err("Archive inspection interrupted".into());
            }
            let mut entry = ptr::null_mut();
            let status = la::archive_read_next_header(reader.raw(), &mut entry);
            if status == la::ARCHIVE_EOF {
                break;
            }
            reader.check(status, "Read archive header")?;
            if entry.is_null() {
                return Err("Missing archive entry".into());
            }
            entries += 1;
            if entries > crate::download::MAX_ARCHIVE_ENTRIES {
                return Err("Archive exceeds maximum allowed file count".into());
            }
            let size = u64::try_from(la::archive_entry_size(entry))
                .map_err(|_| "Negative archive entry size")?;
            logical_bytes = logical_bytes
                .checked_add(size)
                .ok_or("Archive size overflow")?;
            if logical_bytes > crate::download::MAX_UNPACKED_BYTES {
                return Err("Archive uncompressed size exceeds safety limit".into());
            }
            // Charge sparse files by their logical size, and small files by at
            // least one page. Include implicit parent directories and link inodes.
            bytes = bytes.saturating_add(size.div_ceil(4096).saturating_mul(4096));
            let name =
                header_text(la::archive_entry_pathname(entry))?.ok_or("Missing archive path")?;
            let path = relative_path(&name)?;
            for parent in path.ancestors().filter(|p| !p.as_os_str().is_empty()) {
                paths.insert(parent.to_path_buf());
                if paths.len() > crate::download::MAX_ARCHIVE_ENTRIES {
                    return Err(
                        "Archive exceeds maximum allowed file count including parent directories"
                            .into(),
                    );
                }
            }
            reader.check(
                la::archive_read_data_skip(reader.raw()),
                "Skip archive payload",
            )?;
        }
        reader.check(
            la::archive_read_close(reader.raw()),
            "Close package archive",
        )?;
    }
    // Extra per-path space covers directories, symlinks and filesystem metadata.
    Ok((
        bytes.saturating_add((paths.len() as u64).saturating_mul(4096)),
        paths.len() as u64,
    ))
}

pub(crate) fn extract(
    archive_path: &Path,
    target_dir: &Path,
    max_entries: usize,
    max_bytes: u64,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(target_dir).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Archive extraction root must be a real directory".into());
    }
    let root = target_dir.canonicalize().map_err(|e| e.to_string())?;
    // Open once; reader borrows the fd until it is closed/dropped. The file is
    // declared first so every error path drops the reader before closing its fd.
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(archive_path)
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Package archive must be a regular file".into());
    }
    let reader = Archive::new(false)?;
    let writer = Archive::new(true)?;

    // SAFETY: all handles below are non-null, exclusively owned, and used on
    // this thread only. Entries and data blocks belong to reader and are used
    // before its next read. Setters copy their strings. No Rust references to C
    // buffers escape this block, and file remains open throughout.
    unsafe {
        reader.check(
            la::archive_read_support_format_tar(reader.raw()),
            "Enable tar reader",
        )?;
        for enable in [
            la::archive_read_support_filter_gzip,
            la::archive_read_support_filter_xz,
            la::archive_read_support_filter_bzip2,
            archive_read_support_filter_zstd,
        ] {
            // WARN means an external program would be used; refuse that fallback.
            reader.check(enable(reader.raw()), "Enable native archive decompression")?;
        }
        reader.check(
            la::archive_read_open_fd(reader.raw(), file.as_raw_fd(), 64 * 1024),
            "Open package archive",
        )?;
        // NOABSOLUTEPATHS cannot be combined with our absolute destination paths.
        // Validate relative header paths BEFORE rebasing beneath canonical root.
        // This avoids process-wide chdir (pacpin has other active threads).
        // SAFE_WRITES replaces files instead of truncating pre-existing hardlinks.
        writer.check(
            la::archive_write_disk_set_options(
                writer.raw(),
                la::ARCHIVE_EXTRACT_SECURE_SYMLINKS
                    | la::ARCHIVE_EXTRACT_SECURE_NODOTDOT
                    | ARCHIVE_EXTRACT_SAFE_WRITES,
            ),
            "Set secure extraction options",
        )?;

        let mut entries = 0usize;
        let mut declared_total = 0u64;
        let mut actual_total = 0u64;
        let mut extracted_files = HashSet::new();
        loop {
            if crate::sandbox::is_terminating() {
                return Err("Extraction interrupted".into());
            }
            let mut entry = ptr::null_mut();
            let status = la::archive_read_next_header(reader.raw(), &mut entry);
            if status == la::ARCHIVE_EOF {
                break;
            }
            reader.check(status, "Read archive header")?;
            if entry.is_null() {
                return Err("Missing archive entry".into());
            }
            entries += 1;
            if entries > max_entries {
                return Err("Archive exceeds maximum allowed file count".into());
            }

            let name =
                header_text(la::archive_entry_pathname(entry))?.ok_or("Missing archive path")?;
            let path = relative_path(&name)?;
            let hardlink = header_text(la::archive_entry_hardlink(entry))?;
            let symlink = header_text(la::archive_entry_symlink(entry))?;
            let kind = la::archive_entry_filetype(entry);
            // Tar hardlink headers may have filetype 0 in libarchive.
            if !(matches!(kind, la::AE_IFREG | la::AE_IFDIR | la::AE_IFLNK)
                || kind == 0 && hardlink.is_some())
            {
                return Err("Security violation: unsupported archive entry type".into());
            }
            let size = u64::try_from(la::archive_entry_size(entry))
                .map_err(|_| "Negative archive entry size")?;
            declared_total = declared_total
                .checked_add(size)
                .ok_or("Archive size overflow")?;
            if declared_total > max_bytes {
                return Err("Archive uncompressed size exceeds safety limit".into());
            }
            if (hardlink.is_some() || kind != la::AE_IFREG) && size != 0 {
                return Err("Unexpected data on archive link or directory".into());
            }
            // A '.' directory header must not change the private root's mode.
            if path.as_os_str().is_empty() {
                if kind != la::AE_IFDIR || hardlink.is_some() || symlink.is_some() {
                    return Err("Unsafe archive root entry".into());
                }
                continue;
            }
            reject_existing_symlink_components(&root, &path)?;
            if let Some(link) = &symlink {
                if kind != la::AE_IFLNK
                    || hardlink.is_some()
                    || !is_safe_link_destination(&name, link)
                {
                    return Err("Unsafe link target in archive".into());
                }
            } else if kind == la::AE_IFLNK {
                return Err("Missing symlink target".into());
            }
            if let Some(link) = &hardlink {
                if !matches!(kind, 0 | la::AE_IFREG) {
                    return Err("Invalid hardlink entry type".into());
                }
                let link = relative_path(link).map_err(|_| "Unsafe link target in archive")?;
                reject_existing_symlink_components(&root, &link)?;
                // Do not chmod/link arbitrary pre-existing inodes: one could be
                // hardlinked outside the root. Only link files created by this
                // archive (including earlier hardlinks to those files).
                if !extracted_files.contains(&link)
                    || !fs::symlink_metadata(root.join(&link)).is_ok_and(|m| m.is_file())
                {
                    return Err(
                        "Unsafe link target: hardlink must refer to an extracted regular file"
                            .into(),
                    );
                }
                let destination = native_path(&root.join(link))?;
                la::archive_entry_set_hardlink(entry, destination.as_ptr());
            }
            let destination = native_path(&root.join(&path))?;
            la::archive_entry_set_pathname(entry, destination.as_ptr());
            let mut permissions = la::archive_entry_perm(entry) & 0o777;
            // Keep private extraction trees traversable and removable by their
            // owner, even when an adversarial directory header specifies 0000.
            if kind == la::AE_IFDIR {
                permissions |= 0o700;
            }
            la::archive_entry_set_perm(entry, permissions);
            writer.check(
                la::archive_write_header(writer.raw(), entry),
                "Create archive entry",
            )?;
            loop {
                if crate::sandbox::is_terminating() {
                    return Err("Extraction interrupted".into());
                }
                let mut block = ptr::null();
                let mut length = 0usize;
                let mut offset = 0i64;
                let status =
                    la::archive_read_data_block(reader.raw(), &mut block, &mut length, &mut offset);
                if status == la::ARCHIVE_EOF {
                    break;
                }
                reader.check(status, "Read archive data")?;
                let end = u64::try_from(offset)
                    .ok()
                    .and_then(|o| o.checked_add(length as u64))
                    .ok_or("Invalid archive data offset")?;
                actual_total = actual_total
                    .checked_add(length as u64)
                    .ok_or("Archive size overflow")?;
                if end > size || actual_total > max_bytes {
                    return Err("Archive uncompressed size exceeds safety limit".into());
                }
                if length > 0 {
                    if block.is_null() {
                        return Err("Missing archive data block".into());
                    }
                    let status = la::archive_write_data_block(writer.raw(), block, length, offset);
                    writer.check(status as i32, "Write archive data")?;
                }
            }
            writer.check(
                la::archive_write_finish_entry(writer.raw()),
                "Finish archive entry",
            )?;
            if kind == la::AE_IFREG || hardlink.is_some() {
                extracted_files.insert(path);
            }
        }
        writer.check(
            la::archive_write_close(writer.raw()),
            "Finish archive extraction",
        )?;
        reader.check(
            la::archive_read_close(reader.raw()),
            "Close package archive",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "pacpin-libarchive-{}-{nonce}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
        fn destination(&self, name: &str) -> PathBuf {
            let dest = self.0.join(name);
            fs::create_dir(&dest).unwrap();
            dest
        }
        fn save(&self, bytes: &[u8]) -> PathBuf {
            let archive = self.0.join("package.tar");
            fs::write(&archive, bytes).unwrap();
            archive
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn add_file(builder: &mut tar::Builder<Vec<u8>>, name: &str, data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, name, data).unwrap();
    }

    fn add_link(builder: &mut tar::Builder<Vec<u8>>, name: &str, target: &str, hard: bool) {
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o777);
        header.set_entry_type(if hard {
            tar::EntryType::Link
        } else {
            tar::EntryType::Symlink
        });
        header.set_cksum();
        builder.append_link(&mut header, name, target).unwrap();
    }

    // Rust tar generates the fixtures independently of libarchive's tar reader.
    // Libarchive's raw writer supplies native compression without test-only CLI
    // dependencies or altering PATH/environment shared with parallel tests.
    fn compress(bytes: &[u8], filter: &str, path: &Path) {
        if filter == "none" {
            fs::write(path, bytes).unwrap();
            return;
        }
        unsafe {
            let writer = Archive {
                ptr: NonNull::new(la::archive_write_new()).unwrap(),
                writer: true,
            };
            let filter = CString::new(filter).unwrap();
            writer
                .check(
                    la::archive_write_add_filter_by_name(writer.raw(), filter.as_ptr()),
                    "fixture filter",
                )
                .unwrap();
            writer
                .check(
                    la::archive_write_set_format_by_name(writer.raw(), c"raw".as_ptr()),
                    "fixture format",
                )
                .unwrap();
            let path = native_path(path).unwrap();
            writer
                .check(
                    la::archive_write_open_filename(writer.raw(), path.as_ptr()),
                    "fixture open",
                )
                .unwrap();
            let entry = la::archive_entry_new();
            assert!(!entry.is_null());
            la::archive_entry_set_pathname(entry, c"data".as_ptr());
            la::archive_entry_set_filetype(entry, la::AE_IFREG);
            la::archive_entry_set_size(entry, bytes.len() as i64);
            writer
                .check(
                    la::archive_write_header(writer.raw(), entry),
                    "fixture header",
                )
                .unwrap();
            assert_eq!(
                la::archive_write_data(writer.raw(), bytes.as_ptr().cast(), bytes.len()),
                bytes.len() as isize
            );
            la::archive_entry_free(entry);
            writer
                .check(la::archive_write_close(writer.raw()), "fixture close")
                .unwrap();
        }
    }

    #[test]
    fn native_compression_is_detected_from_content_not_filename() {
        let fixture = Fixture::new();
        let mut builder = tar::Builder::new(Vec::new());
        add_file(&mut builder, "usr/bin/tool", b"payload");
        let bytes = builder.into_inner().unwrap();
        for filter in ["none", "gzip", "xz", "zstd", "bzip2"] {
            let archive = fixture.0.join(format!("{filter}.wrong-extension"));
            compress(&bytes, filter, &archive);
            let dest = fixture.destination(filter);
            crate::download::extract_archive(&archive, &dest).unwrap();
            assert_eq!(fs::read(dest.join("usr/bin/tool")).unwrap(), b"payload");
            assert_ne!(
                fs::metadata(dest.join("usr/bin/tool")).unwrap().mode() & 0o111,
                0
            );
        }
    }

    #[test]
    fn valid_links_and_ambiguous_member_names_are_preserved() {
        let fixture = Fixture::new();
        let mut builder = tar::Builder::new(Vec::new());
        add_file(&mut builder, "usr/bin/a -> b link to c", b"payload");
        add_link(&mut builder, "usr/bin/sym", "a -> b link to c", false);
        add_link(
            &mut builder,
            "usr/bin/hard",
            "usr/bin/a -> b link to c",
            true,
        );
        let archive = fixture.save(&builder.into_inner().unwrap());
        let dest = fixture.destination("out");
        crate::download::extract_archive(&archive, &dest).unwrap();
        assert_eq!(fs::read(dest.join("usr/bin/sym")).unwrap(), b"payload");
        assert_eq!(
            fs::metadata(dest.join("usr/bin/hard")).unwrap().ino(),
            fs::metadata(dest.join("usr/bin/a -> b link to c"))
                .unwrap()
                .ino()
        );
    }

    #[test]
    fn disguised_escaping_links_are_rejected_from_real_headers() {
        let fixture = Fixture::new();
        let outside = fixture.0.join("outside");
        fs::write(&outside, b"unchanged").unwrap();
        for (i, (name, target, hard)) in [
            ("a -> b", "../outside", false),
            ("usr/share/x -> y", "../../../outside", false),
            ("a link to b", outside.to_str().unwrap(), true),
            ("usr/bin/bad", "../outside", true),
            ("symlink", outside.to_str().unwrap(), false),
        ]
        .into_iter()
        .enumerate()
        {
            let mut builder = tar::Builder::new(Vec::new());
            add_link(&mut builder, name, target, hard);
            let archive = fixture.save(&builder.into_inner().unwrap());
            let dest = fixture.destination(&format!("out-{i}"));
            assert!(extract(&archive, &dest, 10, 100).is_err());
            assert!(!dest.join(name).exists());
        }
        assert_eq!(fs::read(outside).unwrap(), b"unchanged");
    }

    #[test]
    fn zero_permission_archive_directories_remain_cleanable() {
        let fixture = Fixture::new();
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0);
        header.set_cksum();
        builder
            .append_data(&mut header, "locked", std::io::empty())
            .unwrap();
        add_file(&mut builder, "locked/app", b"payload");
        let archive = fixture.save(&builder.into_inner().unwrap());
        let dest = fixture.destination("out");
        extract(&archive, &dest, 10, 100).unwrap();
        assert_eq!(fs::read(dest.join("locked/app")).unwrap(), b"payload");
        assert_eq!(
            fs::metadata(dest.join("locked"))
                .unwrap()
                .permissions()
                .mode()
                & 0o700,
            0o700
        );
        fs::remove_dir_all(&dest).unwrap();
        assert!(!dest.exists());
    }

    #[test]
    fn absolute_parent_and_invalid_utf8_member_names_fail_closed() {
        let fixture = Fixture::new();
        let outside = fixture.0.join("outside");
        fs::write(&outside, b"unchanged").unwrap();
        for (i, name) in [
            outside.as_os_str().as_bytes(),
            b"../outside",
            b"usr/../../outside",
            b"invalid\xff",
        ]
        .into_iter()
        .enumerate()
        {
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o644);
            header.as_mut_bytes()[..name.len()].copy_from_slice(name);
            header.set_cksum();
            let mut bytes = header.as_bytes().to_vec();
            bytes.extend([0; 1024]);
            let archive = fixture.save(&bytes);
            assert!(extract(&archive, &fixture.destination(&format!("out-{i}")), 10, 100).is_err());
        }
        assert_eq!(fs::read(outside).unwrap(), b"unchanged");
    }

    #[test]
    fn limits_reject_before_the_next_file_is_created() {
        let fixture = Fixture::new();
        let mut builder = tar::Builder::new(Vec::new());
        add_file(&mut builder, "first", b"1234");
        add_file(&mut builder, "second", b"5678");
        let archive = fixture.save(&builder.into_inner().unwrap());
        let count_dest = fixture.destination("count");
        assert!(extract(&archive, &count_dest, 1, 100)
            .unwrap_err()
            .contains("file count"));
        assert!(!count_dest.join("second").exists());
        let size_dest = fixture.destination("size");
        assert!(extract(&archive, &size_dest, 10, 7)
            .unwrap_err()
            .contains("size exceeds"));
        assert!(!size_dest.join("second").exists());
        extract(&archive, &fixture.destination("exact"), 2, 8).unwrap();
    }

    #[test]
    fn truncated_payload_is_an_error() {
        let fixture = Fixture::new();
        let mut header = tar::Header::new_gnu();
        header.set_path("file").unwrap();
        header.set_mode(0o644);
        header.set_size(1024);
        header.set_cksum();
        let archive = fixture.save(header.as_bytes());
        assert!(extract(&archive, &fixture.destination("out"), 10, 2048).is_err());
    }

    #[test]
    fn sparse_logical_size_is_charged_before_writing() {
        let fixture = Fixture::new();
        let mut header = tar::Header::new_gnu();
        header.set_path("sparse").unwrap();
        header.set_entry_type(tar::EntryType::GNUSparse);
        header.set_size(1);
        header.set_mode(0o644);
        let gnu = header.as_gnu_mut().unwrap();
        gnu.set_real_size(1024 * 1024);
        gnu.sparse[0].set_offset(1024 * 1024 - 1);
        gnu.sparse[0].set_length(1);
        header.set_cksum();
        let mut bytes = header.as_bytes().to_vec();
        bytes.push(b'x');
        bytes.extend([0; 1535]);
        let archive = fixture.save(&bytes);
        let dest = fixture.destination("out");
        assert!(extract(&archive, &dest, 10, 100)
            .unwrap_err()
            .contains("size exceeds"));
        assert!(!dest.join("sparse").exists());
        let dest = fixture.destination("allowed");
        extract(&archive, &dest, 10, 1024 * 1024).unwrap();
        assert_eq!(
            fs::metadata(dest.join("sparse")).unwrap().len(),
            1024 * 1024
        );
    }

    #[test]
    fn footprint_charges_implicit_directories_and_small_file_pages() {
        let fixture = Fixture::new();
        let mut builder = tar::Builder::new(Vec::new());
        add_file(&mut builder, "usr/bin/app", b"tiny payload");
        let archive = fixture.save(&builder.into_inner().unwrap());
        let (bytes, inodes) = footprint(&archive).unwrap();
        assert_eq!(inodes, 3);
        assert_eq!(bytes, 4 * 4096);
        assert!(!fixture.0.join("usr").exists());
    }

    #[test]
    fn footprint_charges_sparse_logical_size_without_extracting() {
        let fixture = Fixture::new();
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::GNUSparse);
        header.set_path("sparse").unwrap();
        header.set_mode(0o644);
        header.set_size(1);
        let logical_size = 1024 * 1024;
        let gnu = header.as_gnu_mut().unwrap();
        gnu.set_real_size(logical_size);
        gnu.sparse[0].set_offset(logical_size - 1);
        gnu.sparse[0].set_length(1);
        header.set_cksum();
        let mut data = header.as_bytes().to_vec();
        data.push(b'x');
        data.extend([0; 1535]);
        let archive = fixture.save(&data);
        let (bytes, inodes) = footprint(&archive).unwrap();
        assert_eq!(bytes, logical_size + 4096);
        assert_eq!(inodes, 1);
        assert!(fs::metadata(&archive).unwrap().len() < logical_size);
        assert!(!fixture.0.join("sparse").exists());
    }

    #[test]
    fn footprint_rejects_oversized_headers_before_reading_payload() {
        let fixture = Fixture::new();
        let mut header = tar::Header::new_gnu();
        header.set_path("huge").unwrap();
        header.set_size(crate::download::MAX_UNPACKED_BYTES + 1);
        header.set_mode(0o644);
        header.set_cksum();
        let archive = fixture.save(header.as_bytes());
        assert!(footprint(&archive).unwrap_err().contains("size exceeds"));
    }

    #[test]
    fn multiple_packages_can_share_directories_and_replace_metadata() {
        let fixture = Fixture::new();
        let dest = fixture.destination("out");
        for name in ["dependency", "target"] {
            let mut builder = tar::Builder::new(Vec::new());
            let mut directory = tar::Header::new_gnu();
            directory.set_entry_type(tar::EntryType::Directory);
            directory.set_mode(0o755);
            directory.set_size(0);
            directory.set_cksum();
            builder
                .append_data(&mut directory, "usr/bin", std::io::empty())
                .unwrap();
            add_file(&mut builder, ".PKGINFO", name.as_bytes());
            add_file(&mut builder, &format!("usr/bin/{name}"), name.as_bytes());
            let archive = fixture.save(&builder.into_inner().unwrap());
            crate::download::extract_archive(&archive, &dest).unwrap();
        }
        assert_eq!(fs::read(dest.join(".PKGINFO")).unwrap(), b"target");
        assert!(dest.join("usr/bin/dependency").is_file());
        assert!(dest.join("usr/bin/target").is_file());
    }

    #[test]
    fn non_tar_formats_and_excessive_path_depth_are_rejected() {
        let fixture = Fixture::new();
        let zip = fixture.save(b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0");
        assert!(extract(&zip, &fixture.destination("zip"), 10, 100).is_err());
        assert!(relative_path(&"a/".repeat(129)).is_err());
        assert!(native_path(Path::new(&"a".repeat(libc::PATH_MAX as usize))).is_err());
    }

    #[test]
    fn existing_external_hardlink_is_not_truncated() {
        let fixture = Fixture::new();
        let outside = fixture.0.join("outside");
        fs::write(&outside, b"unchanged").unwrap();
        let dest = fixture.destination("out");
        fs::hard_link(&outside, dest.join("file")).unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        add_file(&mut builder, "file", b"new");
        let archive = fixture.save(&builder.into_inner().unwrap());
        extract(&archive, &dest, 10, 100).unwrap();
        assert_eq!(fs::read(&outside).unwrap(), b"unchanged");
        assert_eq!(fs::read(dest.join("file")).unwrap(), b"new");
    }

    #[test]
    fn hardlink_to_preexisting_inode_cannot_change_host_permissions() {
        let fixture = Fixture::new();
        let outside = fixture.0.join("outside");
        fs::write(&outside, b"unchanged").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
        let dest = fixture.destination("out");
        fs::hard_link(&outside, dest.join("existing")).unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        add_link(&mut builder, "new-link", "existing", true);
        let archive = fixture.save(&builder.into_inner().unwrap());
        assert!(extract(&archive, &dest, 10, 100).is_err());
        assert_eq!(fs::metadata(outside).unwrap().mode() & 0o7777, 0o600);
        assert!(!dest.join("new-link").exists());
    }

    #[test]
    fn fifo_and_symlink_archive_sources_fail_without_blocking() {
        let fixture = Fixture::new();
        let fifo = fixture.0.join("fifo");
        let name = native_path(&fifo).unwrap();
        // SAFETY: name is a live C string; create only inside our private fixture.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let dest = fixture.destination("out");
        assert!(extract(&fifo, &dest, 10, 100)
            .unwrap_err()
            .contains("regular file"));
        let link = fixture.0.join("link");
        std::os::unix::fs::symlink(&fifo, &link).unwrap();
        assert!(extract(&link, &dest, 10, 100).is_err());
    }

    #[test]
    fn root_directory_header_does_not_change_private_root_permissions() {
        let fixture = Fixture::new();
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o7777);
        header.set_size(0);
        header.set_cksum();
        builder
            .append_data(&mut header, ".", std::io::empty())
            .unwrap();
        let archive = fixture.save(&builder.into_inner().unwrap());
        let dest = fixture.destination("out");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o700)).unwrap();
        extract(&archive, &dest, 10, 100).unwrap();
        assert_eq!(fs::metadata(dest).unwrap().mode() & 0o7777, 0o700);
    }

    #[test]
    fn symlink_root_and_symlink_hardlink_targets_are_rejected() {
        let fixture = Fixture::new();
        let dest = fixture.destination("out");
        fs::write(dest.join("real"), b"keep").unwrap();
        std::os::unix::fs::symlink("real", dest.join("sym")).unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        add_link(&mut builder, "hard", "sym", true);
        let archive = fixture.save(&builder.into_inner().unwrap());
        assert!(extract(&archive, &dest, 10, 100).is_err());
        let root_link = fixture.0.join("root-link");
        std::os::unix::fs::symlink(&dest, &root_link).unwrap();
        assert!(extract(&archive, &root_link, 10, 100)
            .unwrap_err()
            .contains("real directory"));
    }
}
