//! Paru owns the build; pacpin selects private temporary storage explicitly.
use crate::scratch::{self, Policy};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) struct AurWorkspace {
    path: PathBuf,
    preserve: bool,
}
pub(super) fn is_paru(helper: &str) -> bool {
    Path::new(helper)
        .file_name()
        .is_some_and(|name| name == "paru")
}
pub(super) fn validate(helper: &str, policy: Policy, forwarded: &[String]) -> Result<(), String> {
    check_options(helper, policy, forwarded).map_err(|error| error.to_string())
}
pub(super) fn check_options(helper: &str, policy: Policy, forwarded: &[String]) -> Result<(), crate::runtime::OptionError> {
    use crate::runtime::OptionError;
    if policy == Policy::Auto {
        return Ok(());
    }
    if forwarded.iter().any(|flag| {
        matches!(
            flag.split('=').next().unwrap_or(flag),
            "--clonedir" | "--makepkg" | "--mflags" | "--chroot" | "--localrepo"
        )
    }) {
        return Err(OptionError::Invalid(
            "Scratch policy conflicts with custom clone, makepkg, chroot or local-repository flags"
                .into(),
        ));
    }
    if !is_paru(helper) {
        return Err(OptionError::Unsupported("Explicit scratch policy currently supports Paru and Sources; this AUR helper is unsupported".into()));
    }
    Ok(())
}
impl AurWorkspace {
    pub fn prepare(helper: &str, policy: Policy, forwarded: &[String]) -> io::Result<Option<Self>> {
        if policy == Policy::Auto {
            return Ok(None);
        }
        validate(helper, policy, forwarded).map_err(io::Error::other)?;
        // AUR recipes are arbitrary: a starting floor is not a package footprint
        // estimate. Keep the uncertainty visible and never promise zero disk use.
        let base = scratch::choose(policy, 128 * 1024 * 1024, 128).map_err(io::Error::other)?;
        let mut template = base
            .join("pacpin-aur-XXXXXX")
            .as_os_str()
            .as_bytes()
            .to_vec();
        template.push(0);
        if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
            return Err(io::Error::last_os_error());
        }
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(&template[..template.len() - 1]));
        crate::execution::register_cleanup_target(path.clone());
        let work = Self {
            path,
            preserve: false,
        };
        for child in ["clones", "build", "packages", "sources", "logs"] {
            fs::create_dir(work.path.join(child))?;
        }
        println!(":: AUR temporary workspace: {} ({policy:?}). Build size is unknown; installed files and build dependencies still require root space.", work.path.display());
        Ok(Some(work))
    }
    pub fn configure(&self, command: &mut Command) {
        command.arg("--clonedir").arg(self.path.join("clones"));
        self.configure_environment(command);
        command.arg("--nochroot").arg("--nolocalrepo");
    }
    fn configure_environment(&self, command: &mut Command) {
        // makepkg's load_makepkg_config preserves these environment overrides.
        // Do not rewrite the user's configuration or run makepkg as root.
        for (key, dir) in [
            ("BUILDDIR", "build"),
            ("PKGDEST", "packages"),
            ("SRCDEST", "sources"),
            ("SRCPKGDEST", "sources"),
            ("LOGDEST", "logs"),
            ("TMPDIR", "build"),
        ] {
            command.env(key, self.path.join(dir));
        }
    }
    pub fn finish(&mut self, success: bool) {
        self.preserve = !success;
        if self.preserve {
            crate::execution::unregister_cleanup_target(&self.path);
            eprintln!(":: AUR build failed; workspace retained for inspection: {} (RAM workspaces do not survive reboot)", self.path.display());
        }
    }
}
impl Drop for AurWorkspace {
    fn drop(&mut self) {
        if !self.preserve {
            if let Err(error) = fs::remove_dir_all(&self.path) {
                if error.kind() != io::ErrorKind::NotFound {
                    eprintln!(
                        "Warning: Cannot remove AUR temporary workspace {}: {error}",
                        self.path.display()
                    );
                }
            }
            crate::execution::unregister_cleanup_target(&self.path);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn successful_workspace_is_cleaned_and_failed_workspace_is_retained() {
        let fixture = crate::db::test_support::AlpmFixture::new(&[], &[]);
        let root = PathBuf::from(fixture.manager.handle().dbpath());
        let owned = root.join("owned-aur-workspace");
        fs::create_dir(&owned).unwrap();
        let unrelated = root.join("unrelated");
        fs::write(&unrelated, "keep").unwrap();
        {
            let mut work = AurWorkspace {
                path: owned.clone(),
                preserve: false,
            };
            work.finish(true);
        }
        assert!(!owned.exists());
        assert!(unrelated.exists());
        fs::create_dir(&owned).unwrap();
        {
            let mut work = AurWorkspace {
                path: owned.clone(),
                preserve: false,
            };
            work.finish(false);
        }
        assert!(owned.exists());
        assert!(unrelated.exists());
    }
    #[test]
    fn unsupported_helpers_and_conflicting_flags_fail_before_allocation() {
        assert!(AurWorkspace::prepare("yay", Policy::Ram, &[]).is_err());
        assert!(
            AurWorkspace::prepare("paru", Policy::Ram, &["--mflags=--config=x".into()]).is_err()
        );
        assert!(AurWorkspace::prepare("yay", Policy::Auto, &[])
            .unwrap()
            .is_none());
    }
    #[test]
    fn build_paths_are_arguments_and_environment_not_shell_text() {
        let work = AurWorkspace {
            path: "/tmp/path with spaces".into(),
            preserve: true,
        };
        let mut command = Command::new("paru");
        work.configure(&mut command);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args[0], "--clonedir");
        assert_eq!(args[1], "/tmp/path with spaces/clones");
        assert!(command.get_envs().any(|(name, value)| name == "BUILDDIR"
            && value == Some(std::ffi::OsStr::new("/tmp/path with spaces/build"))));
    }
}
