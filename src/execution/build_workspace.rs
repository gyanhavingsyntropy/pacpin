//! Commands in an unprivileged build view: host files read-only, owned work writable.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn readonly_runtime(command: &mut Command) {
    for path in ["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc"] {
        if Path::new(path).exists() {
            command.args(["--ro-bind", path, path]);
        }
    }
}

pub(crate) fn supports_ro_overlay() -> bool {
    let mut probe = Command::new("bwrap");
    probe.args(["--unshare-all", "--die-with-parent", "--new-session"]);
    readonly_runtime(&mut probe);
    probe.args([
        "--overlay-src",
        "/usr/share",
        "--overlay-src",
        "/etc",
        "--ro-overlay",
        "/usr/share",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--",
        "/usr/bin/true",
    ]);
    super::capture_output(&mut probe).is_ok_and(|output| output.status.success())
}

#[derive(Clone)]
pub(crate) struct BuildWorkspace {
    pub root: PathBuf,
    pub overlay: bool,
}

/// Merge prepared package trees into their absolute paths without installing them.
pub(crate) fn package_mounts(
    command: &mut Command,
    root: &Path,
    overlay: bool,
) -> Result<(), String> {
    for relative in ["usr", "etc", "opt"] {
        let source = root.join(relative);
        if fs::symlink_metadata(&source).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err("Package mount roots must be real directories".into());
        }
        if !source.is_dir() {
            continue;
        }
        let target = Path::new("/").join(relative);
        if target.exists() {
            if !overlay {
                return Err("Temporary dependencies require Bubblewrap read-only overlay support; no system dependencies were installed".into());
            }
            command
                .arg("--overlay-src")
                .arg(&target)
                .arg("--overlay-src")
                .arg(&source)
                .arg("--ro-overlay")
                .arg(target);
        } else {
            command.arg("--ro-bind").arg(source).arg(target);
        }
    }
    Ok(())
}

impl BuildWorkspace {
    /// Preserve literal arguments and explicit environment; never run a shell.
    pub fn command(&self, original: Command, work: &Path) -> Result<Command, String> {
        if unsafe { libc::geteuid() } == 0 {
            return Err("Temporary builds must run as a regular user".into());
        }
        let work = work.canonicalize().map_err(|e| e.to_string())?;
        let cwd = original
            .get_current_dir()
            .unwrap_or(&work)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !cwd.starts_with(&work) {
            return Err("Build working directory escapes its owned workspace".into());
        }
        for name in ["home", "tmp", "cache"] {
            fs::create_dir_all(work.join(name)).map_err(|e| e.to_string())?;
        }
        let mut command = Command::new("bwrap");
        command.args([
            "--unshare-all",
            "--share-net",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
        ]);
        readonly_runtime(&mut command);
        for path in ["/opt", "/var/lib/pacman"] {
            if Path::new(path).exists() {
                command.args(["--ro-bind", path, path]);
            }
        }
        package_mounts(&mut command, &self.root, self.overlay)?;
        // Only the resolver file is exposed from /run, never service sockets.
        // /etc/resolv.conf is often a symlink to NetworkManager/systemd state.
        if let Ok(resolver) = Path::new("/etc/resolv.conf").canonicalize() {
            if resolver.starts_with("/run") {
                command.arg("--ro-bind").arg(&resolver).arg(&resolver);
            }
        }
        command.args(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"]);
        command.arg("--bind").arg(&work).arg(&work);
        command.arg("--chdir").arg(&cwd);
        for (name, value) in [
            ("PATH", "/usr/local/bin:/usr/bin:/bin"),
            ("LANG", "C.UTF-8"),
            ("USER", "builder"),
            ("LOGNAME", "builder"),
        ] {
            command.args(["--setenv", name, value]);
        }
        for (name, value) in original.get_envs() {
            if let Some(value) = value {
                command.arg("--setenv").arg(name).arg(value);
            }
        }
        // A caller's environment cannot redirect owned caches into the host home.
        for (name, path) in [
            ("HOME", work.join("home")),
            ("TMPDIR", work.join("tmp")),
            ("XDG_CACHE_HOME", work.join("cache")),
            ("CARGO_HOME", work.join("cache/cargo")),
            ("PIP_CACHE_DIR", work.join("cache/pip")),
            ("npm_config_cache", work.join("cache/npm")),
        ] {
            command.arg("--setenv").arg(name).arg(path);
        }
        command
            .arg("--")
            .arg(original.get_program())
            .args(original.get_args());
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Work(PathBuf);
    impl Work {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "pacpin-build-view-test-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Work {
        fn drop(&mut self) {
            let _ = super::super::remove_owned_tree(&self.0);
        }
    }

    #[test]
    fn command_retains_literal_arguments_and_owns_home_and_caches() {
        let work = Work::new();
        let view = BuildWorkspace {
            root: work.0.join("missing"),
            overlay: false,
        };
        let mut original = Command::new("makepkg");
        original
            .current_dir(&work.0)
            .args(["--nodeps", "; touch /outside"])
            .env("HOME", "/host-home")
            .env("TMPDIR", "/host-tmp");
        let result = view.command(original, &work.0);
        if unsafe { libc::geteuid() } == 0 {
            assert!(result.is_err());
            return;
        }
        let command = result.unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            &args[args.len() - 3..],
            ["makepkg", "--nodeps", "; touch /outside"]
        );
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--bind", work.0.to_str().unwrap()]));
        assert!(args.iter().any(|arg| arg == "--clearenv"));
        for key in [
            "HOME",
            "TMPDIR",
            "XDG_CACHE_HOME",
            "CARGO_HOME",
            "PIP_CACHE_DIR",
        ] {
            let last = args.iter().rposition(|arg| arg == key).unwrap();
            assert!(Path::new(&args[last + 1]).starts_with(&work.0));
        }
    }

    #[test]
    fn build_working_directory_must_be_owned() {
        let work = Work::new();
        let view = BuildWorkspace {
            root: work.0.join("missing"),
            overlay: false,
        };
        let mut command = Command::new("true");
        command.current_dir("/");
        assert!(view.command(command, &work.0).is_err());
    }

    #[test]
    fn package_mounts_refuse_missing_overlay_support_and_symlink_roots() {
        let work = Work::new();
        fs::create_dir(work.0.join("usr")).unwrap();
        assert!(package_mounts(&mut Command::new("bwrap"), &work.0, false).is_err());
        fs::remove_dir(work.0.join("usr")).unwrap();
        std::os::unix::fs::symlink("/usr", work.0.join("usr")).unwrap();
        assert!(package_mounts(&mut Command::new("bwrap"), &work.0, true).is_err());
    }
}
