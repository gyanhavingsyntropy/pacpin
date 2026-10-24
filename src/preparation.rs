//! Private package preparation; callers own installation or application execution.
mod downloads;
mod resources;
mod root;
#[cfg(test)]
pub(crate) use downloads::{download_targets_parallel, ephemeral_download_label};
pub(crate) use downloads::{ephemeral_footprint, fetch_targets};
pub(crate) use resources::{create_secure_temp_dir_in, SandboxGuard};
pub(crate) use root::{confirm_build, PackageRoot};
#[derive(Clone, Debug)]
pub(crate) struct Options {
    pub no_cache: bool,
    pub allow_unverified: bool,
    pub noconfirm: bool,
    pub prefix: &'static str,
}

pub(crate) fn check_capability() -> Result<(), String> {
    if unsafe { libc::geteuid() } == 0 {
        return Err("Private package preparation must start as a regular user".into());
    }
    if !crate::is_command_available("bwrap") || !crate::execution::supports_ro_overlay() {
        return Err(
            "Private dependencies require Bubblewrap with read-only overlay support".into(),
        );
    }
    Ok(())
}
