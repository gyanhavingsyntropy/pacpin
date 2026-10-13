//! Shared quota and memory checks; managers own workspace lifetimes.
use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const OS_RESERVE: u64 = 5 * 1024 * 1024 * 1024 / 2;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Policy {
    #[default]
    Auto,
    Ram,
    Storage,
}
static POLICY: OnceLock<Policy> = OnceLock::new();
pub fn initialize(policy: Policy) {
    let _ = POLICY.set(policy);
}
pub fn policy() -> Policy {
    POLICY.get().copied().unwrap_or_default()
}

/// Private temporary workspace shared by user-configured manager adapters.
pub struct Workspace { pub path: PathBuf }
impl Workspace {
    pub fn create(policy: Policy, bytes: u64) -> Result<Self, String> {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let base = choose(policy, bytes, 128)?;
        let mut template = base.join("pacpin-manager-XXXXXX").as_os_str().as_bytes().to_vec();
        template.push(0);
        if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() { return Err(io::Error::last_os_error().to_string()); }
        template.pop();
        let path = PathBuf::from(std::ffi::OsString::from_vec(template));
        crate::sandbox::register_cleanup_target(path.clone());
        Ok(Self { path })
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
        crate::sandbox::unregister_cleanup_target(&self.path);
    }
}
pub fn describe_plan(has_sources: bool, has_aur: bool) -> Option<String> {
    describe_managers(has_sources, has_aur, &[])
}
pub fn describe_managers(has_sources: bool, has_aur: bool, external: &[&str]) -> Option<String> {
    if policy() == Policy::Auto { return None; }
    let mut managers = Vec::new();
    if has_sources { managers.push("Sources"); }
    if has_aur { managers.push("Paru"); }
    managers.extend_from_slice(external);
    if managers.is_empty() { return Some("Scratch preference unused: this plan has no temporary build workspace.".into()); }
    let managers = managers.join(" / ");
    Some(format!("{:?} temporary workspace for {managers}; root installation space remains required.", policy()))
}
#[derive(Clone, Copy, Debug)]
pub struct Capacity {
    pub bytes: u64,
    pub inodes: u64,
    pub ram: bool,
}

pub fn quota_remaining(hard: u64, soft: u64, used: u64, unit: u64) -> Option<u64> {
    // Respect soft limits too, rather than relying on a grace period during builds.
    [hard, soft]
        .into_iter()
        .filter(|limit| *limit != 0)
        .min()
        .map(|limit| limit.saturating_mul(unit).saturating_sub(used))
}

pub fn capacity(path: &Path) -> Result<Capacity, String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let mut fs_stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: paths are NUL terminated and output structures are correctly sized.
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0
        || unsafe { libc::statfs(c_path.as_ptr(), &mut fs_stat) } != 0
    {
        return Err(io::Error::last_os_error().to_string());
    }
    if unsafe { libc::access(c_path.as_ptr(), libc::W_OK) } != 0 {
        return Err(format!(
            "Scratch directory is not writable: {}",
            io::Error::last_os_error()
        ));
    }
    let mut result = Capacity {
        bytes: stat.f_bavail.saturating_mul(stat.f_frsize),
        // Filesystems such as Btrfs report zero when inodes are allocated
        // dynamically, rather than exposing a fixed inode pool.
        inodes: if stat.f_files == 0 {
            u64::MAX
        } else {
            stat.f_favail
        },
        ram: fs_stat.f_type == libc::TMPFS_MAGIC,
    };
    let dir = File::open(path).map_err(|e| e.to_string())?;
    let metadata = dir.metadata().map_err(|e| e.to_string())?;
    // A setgid parent assigns its group to newly created build directories.
    let gid = if metadata.mode() & libc::S_ISGID != 0 {
        metadata.gid()
    } else {
        unsafe { libc::getegid() }
    };
    for (kind, id) in [(0, unsafe { libc::geteuid() }), (1, gid)] {
        let mut quota: libc::dqblk = unsafe { std::mem::zeroed() };
        // SAFETY: fd remains open, Q_GETQUOTA only reads this identity's quota,
        // and the kernel writes into a live Linux dqblk with the matching ABI.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_quotactl_fd,
                dir.as_raw_fd(),
                libc::QCMD(libc::Q_GETQUOTA, kind),
                id,
                &mut quota as *mut libc::dqblk,
            )
        };
        if rc != 0 {
            let error = io::Error::last_os_error();
            // No active quota, unsupported filesystem, or an older kernel.
            if matches!(
                error.raw_os_error(),
                Some(libc::ESRCH | libc::ENOSYS | libc::EOPNOTSUPP | libc::EINVAL)
            ) {
                continue;
            }
            return Err(format!(
                "Cannot inspect quota for {}: {error}",
                path.display()
            ));
        }
        if let Some(bytes) = quota_remaining(
            quota.dqb_bhardlimit,
            quota.dqb_bsoftlimit,
            quota.dqb_curspace,
            1024,
        ) {
            result.bytes = result.bytes.min(bytes);
        }
        if let Some(inodes) = quota_remaining(
            quota.dqb_ihardlimit,
            quota.dqb_isoftlimit,
            quota.dqb_curinodes,
            1,
        ) {
            result.inodes = result.inodes.min(inodes);
        }
    }
    Ok(result)
}

pub fn fits(capacity: Capacity, bytes: u64, inodes: u64, available_memory: u64) -> bool {
    capacity.bytes >= bytes
        && capacity.inodes >= inodes
        && (!capacity.ram || available_memory >= bytes.saturating_add(OS_RESERVE))
}

pub fn mem_available_bytes() -> Option<u64> {
    let content = fs::read_to_string("/proc/meminfo").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let num_str = rest.split_whitespace().next()?;
            let kb: u64 = num_str.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

pub fn bases(policy: Policy) -> Vec<PathBuf> {
    let mut bases: Vec<PathBuf> = match policy {
        Policy::Auto => vec![std::env::temp_dir(), "/var/tmp".into()],
        Policy::Ram => vec![std::env::temp_dir(), "/tmp".into(), "/dev/shm".into()],
        Policy::Storage => vec![std::env::temp_dir(), "/var/tmp".into()],
    };
    let mut seen = std::collections::HashSet::new();
    bases.retain(|base| seen.insert(base.clone()));
    bases
}
pub fn accepts(policy: Policy, space: Capacity) -> bool {
    match policy {
        Policy::Auto => true,
        Policy::Ram => space.ram,
        Policy::Storage => !space.ram,
    }
}
pub fn choose(policy: Policy, bytes: u64, inodes: u64) -> Result<PathBuf, String> {
    let available = mem_available_bytes().unwrap_or(0);
    let mut reasons = Vec::new();
    for base in bases(policy) {
        match capacity(&base) {
            Ok(space) if accepts(policy, space) && fits(space, bytes, inodes, available) => {
                return Ok(base)
            }
            Ok(space) => reasons.push(format!(
                "{}: {:?} policy; {}",
                base.display(),
                policy,
                describe(space, bytes, inodes, available)
            )),
            Err(error) => reasons.push(format!("{}: {error}", base.display())),
        }
    }
    Err(format!("Insufficient scratch capacity for {policy:?} policy: need {:.1} MiB and {inodes} inodes.\n  {}\nSet TMPDIR to a writable filesystem matching the selected policy with enough capacity.", bytes as f64 / 1_048_576.0, reasons.join("\n  ")))
}
pub fn describe(space: Capacity, bytes: u64, inodes: u64, available: u64) -> String {
    format!(
        "{:.1} MiB available after quotas (need {:.1} MiB), {} free inodes (need {inodes}){}",
        space.bytes as f64 / 1_048_576.0,
        bytes as f64 / 1_048_576.0,
        space.inodes,
        if space.ram {
            format!(
                "; {:.1} MiB RAM available, {:.1} MiB required including the 2.5 GiB OS reserve",
                available as f64 / 1_048_576.0,
                bytes.saturating_add(OS_RESERVE) as f64 / 1_048_576.0
            )
        } else {
            String::new()
        }
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forced_policies_never_change_medium() {
        let ram = Capacity {
            bytes: 100,
            inodes: 100,
            ram: true,
        };
        let disk = Capacity { ram: false, ..ram };
        assert!(accepts(Policy::Ram, ram));
        assert!(!accepts(Policy::Ram, disk));
        assert!(accepts(Policy::Storage, disk));
        assert!(!accepts(Policy::Storage, ram));
        assert!(fits(ram, 100, 100, OS_RESERVE + 100));
        assert!(!fits(ram, 100, 100, OS_RESERVE + 99));
        assert!(!fits(disk, 101, 1, 0));
    }
}
