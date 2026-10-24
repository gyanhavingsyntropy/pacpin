//! Filesystem ownership and atomic publication for retained source trees.
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Component, Path, PathBuf},
};

pub(super) const RECEIPT: &str = ".pacpin-directory.json";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    pub schema: u32,
    pub source: String,
    pub package: String,
    pub directory: PathBuf,
    pub version: String,
    pub tag: String,
    pub sha256: Option<String>,
    pub adopted: bool,
    pub device: u64,
    pub inode: u64,
}

pub(super) fn validate_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path.components().count() < 3
        || path
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        || path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .any(|b| b.is_ascii_control())
    {
        return Err(
            "install_dir must be an absolute application directory, without '.' or '..'".into(),
        );
    }
    for root in [
        "/usr",
        "/etc",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/dev",
        "/proc",
        "/sys",
        "/var/lib/pacman",
    ] {
        if path.starts_with(root) {
            return Err("install_dir cannot replace a system directory".into());
        }
    }
    Ok(())
}

pub(super) fn parent(path: &Path) -> Result<PathBuf, String> {
    validate_path(path)?;
    let parent = path.parent().ok_or("Missing installation parent")?;
    let actual = parent
        .canonicalize()
        .map_err(|e| format!("Installation parent must exist: {e}"))?;
    if actual != parent {
        return Err(
            "install_dir parent must use its canonical path, without symlink components".into(),
        );
    }
    let metadata = fs::symlink_metadata(&actual).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err("Installation parent must be a directory owned by your regular user".into());
    }
    Ok(actual)
}

pub(super) fn owned_directory(path: &Path) -> Result<fs::Metadata, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err("Installation must be a real directory owned by your regular user".into());
    }
    Ok(metadata)
}

const OWNER: &str = ".pacpin-directory-owner.json";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    schema: u32,
    source: String,
    package: String,
    directory: PathBuf,
    device: u64,
    inode: u64,
    imported: Option<PathBuf>,
}

fn json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Directory record must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(16_385)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 16_384 {
        return Err("Directory record exceeds 16 KiB".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("Invalid directory record: {e}"))
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        serde_json::to_writer(&mut file, value).map_err(|e| e.to_string())?;
        file.write_all(b"\n").map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}
fn owner(path: &Path, alias: &str, name: &str) -> Result<Owner, String> {
    let metadata = owned_directory(path)?;
    let owner: Owner = json(&path.join(OWNER))?;
    if owner.schema != 1
        || owner.source != alias
        || owner.package != name
        || owner.directory != path
        || owner.device != metadata.dev()
        || owner.inode != metadata.ino()
    {
        return Err("Installation directory differs from its ownership record".into());
    }
    Ok(owner)
}
fn initialize(
    path: &Path,
    destination: &Path,
    alias: &str,
    name: &str,
    imported: Option<PathBuf>,
) -> Result<(), String> {
    fs::create_dir(path).map_err(|e| e.to_string())?;
    fs::create_dir(path.join("versions")).map_err(|e| e.to_string())?;
    let metadata = owned_directory(path)?;
    write_json(
        &path.join(OWNER),
        &Owner {
            schema: 1,
            source: alias.into(),
            package: name.into(),
            directory: destination.into(),
            device: metadata.dev(),
            inode: metadata.ino(),
            imported,
        },
    )
}
fn payload(path: &Path, owner: &Owner) -> Result<(PathBuf, bool), String> {
    let current = fs::read_link(path.join("current"))
        .map_err(|e| format!("Missing/invalid current version link: {e}"))?;
    if owner.imported.as_ref() == Some(&current) && current.is_absolute() {
        if current.canonicalize().map_err(|e| e.to_string())? != current {
            return Err("Imported application path changed".into());
        }
        return Ok((current, true));
    }
    if current.components().count() != 2
        || !current.starts_with("versions")
        || !current
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err("Current version escapes the managed versions directory".into());
    }
    owned_directory(&path.join("versions"))?;
    let root = path.join(current);
    owned_directory(&root)?;
    Ok((root, false))
}
fn verify_receipt(
    receipt: &Receipt,
    root: &Path,
    directory: &Path,
    alias: &str,
    name: &str,
) -> Result<(), String> {
    let metadata = owned_directory(root)?;
    if receipt.schema != 1
        || receipt.source != alias
        || receipt.package != name
        || receipt.directory != directory
        || receipt.device != metadata.dev()
        || receipt.inode != metadata.ino()
        || !valid_version(&receipt.version)
        || receipt.tag.chars().any(char::is_control)
        || receipt
            .sha256
            .as_ref()
            .is_some_and(|value| !super::super::valid_digest(value))
    {
        return Err(
            "Directory ownership differs from its receipt; refusing update or removal".into(),
        );
    }
    Ok(())
}
pub(super) fn read(path: &Path, alias: &str, name: &str) -> Result<Option<Receipt>, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        Ok(_) => {
            owned_directory(path)?;
        }
    }
    match fs::symlink_metadata(path.join(OWNER)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        Ok(_) => {}
    }
    let owner = owner(path, alias, name)?;
    let (root, imported) = payload(path, &owner)?;
    let receipt: Receipt = json(&if imported {
        path.join(".pacpin-adopted.json")
    } else {
        root.join(RECEIPT)
    })?;
    verify_receipt(&receipt, &root, path, alias, name)?;
    Ok(Some(receipt))
}

pub(super) fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+:-".contains(&b))
}

pub(super) fn write(path: &Path, receipt: &Receipt) -> Result<(), String> {
    write_json(&path.join(RECEIPT), receipt)
}

pub(super) struct Lock(File);
impl Lock {
    pub fn acquire(path: &Path) -> Result<Self, String> {
        let directory = parent(path)?;
        let name = path
            .file_name()
            .ok_or("Missing application directory name")?;
        let lock = directory.join(format!(".{}.pacpin.lock", name.to_string_lossy()));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(lock)
            .map_err(|e| e.to_string())?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err("Unsafe directory installation lock".into());
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("Another operation owns this installation directory".into());
        }
        Ok(Self(file))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn rename_new(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn publish(
    staged: &Path,
    destination: &Path,
    previous: Option<&Receipt>,
) -> Result<(), String> {
    let new: Receipt = json(&staged.join(RECEIPT))?;
    verify_receipt(&new, staged, destination, &new.source, &new.package)?;
    let work = staged
        .parent()
        .ok_or("Missing publication staging parent")?;
    if let Some(previous) = previous {
        if read(destination, &previous.source, &previous.package)?.as_ref() != Some(previous) {
            return Err("Directory changed after planning; previous installation preserved".into());
        }
        let versions = destination.join("versions");
        owned_directory(&versions)?;
        let slot = crate::preparation::create_secure_temp_dir_in("generation", &versions)?;
        let guard = crate::preparation::SandboxGuard::new(slot.clone());
        fs::rename(staged, &slot).map_err(|e| e.to_string())?;
        let relative =
            Path::new("versions").join(slot.file_name().ok_or("Missing generation name")?);
        let link = work.join(".current-next");
        std::os::unix::fs::symlink(&relative, &link).map_err(|e| e.to_string())?;
        // The registry lock makes publication and relinquishing cleanup
        // ownership indivisible with respect to handled cancellation.
        guard.publish(|| fs::rename(&link, destination.join("current")))?;
    } else {
        let container = work.join(".installation");
        initialize(&container, destination, &new.source, &new.package, None)?;
        let generation = container.join("versions/initial");
        fs::rename(staged, &generation).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink("versions/initial", container.join("current"))
            .map_err(|e| e.to_string())?;
        rename_new(&container, destination).map_err(|e| {
            format!("Cannot publish installation; existing destination preserved: {e}")
        })?;
    }
    File::open(destination)
        .and_then(|file| file.sync_all())
        .map_err(|e| format!("Version published, but syncing directory failed: {e}"))?;
    Ok(())
}

pub(super) fn adopt(
    work: &Path,
    destination: &Path,
    imported: &Path,
    receipt: &Receipt,
) -> Result<(), String> {
    let container = work.join("installation");
    initialize(
        &container,
        destination,
        &receipt.source,
        &receipt.package,
        Some(imported.into()),
    )?;
    write_json(&container.join(".pacpin-adopted.json"), receipt)?;
    std::os::unix::fs::symlink(imported, container.join("current")).map_err(|e| e.to_string())?;
    rename_new(&container, destination).map_err(|e| {
        format!("Cannot adopt into installation directory; existing files preserved: {e}")
    })
}

pub(super) fn clean_previous(directory: &Path, alias: &str, name: &str) -> Result<(), String> {
    let owner = owner(directory, alias, name)?;
    let (active, _) = payload(directory, &owner)?;
    let versions = directory.join("versions");
    owned_directory(&versions)?;
    for entry in fs::read_dir(&versions).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path == active {
            continue;
        }
        let receipt: Receipt = json(&path.join(RECEIPT))?;
        verify_receipt(&receipt, &path, directory, alias, name)?;
        crate::execution::remove_owned_tree(&path).map_err(|e| {
            format!("New version is active, but previous-version cleanup failed: {e}")
        })?;
    }
    Ok(())
}
