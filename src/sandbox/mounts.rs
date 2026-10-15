//! Bubblewrap capability probes and permitted host mounts.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) fn safe_wayland_socket(runtime: &Path, display: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    use std::os::unix::fs::FileTypeExt;

    if display.is_empty() || display == "." || display == ".." || display.contains(['/', '\\', '\0']) {
        return None;
    }
    // Validate the runtime directory without creating anything in it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = fs::symlink_metadata(runtime).ok()?;
        if !meta.is_dir() || meta.file_type().is_symlink()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            return None;
        }
    }
    let path = runtime.join(display);
    let meta = fs::symlink_metadata(&path).ok()?;
    #[cfg(unix)]
    if !meta.file_type().is_socket() { return None; }
    #[cfg(not(unix))]
    if !meta.is_file() { return None; }
    Some(path)
}

pub(super) fn should_bind_cwd(cwd: &Path, home: &Path) -> bool {
    let (Ok(cwd), Ok(home)) = (cwd.canonicalize(), home.canonicalize()) else {
        return false;
    };
    if cwd == Path::new("/") || cwd.starts_with(&home) || home.starts_with(&cwd) {
        return false;
    }
    let protected = [
        "/bin", "/boot", "/dev", "/etc", "/lib", "/lib64", "/proc", "/run",
        "/sbin", "/sys", "/tmp", "/usr", "/var",
    ];
    !protected.iter().any(|path| cwd.starts_with(path))
}

#[allow(unused_imports)]
pub(super) fn add_host_runtime_mounts(command: &mut Command) {
    for dir in &["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc"] {
        let path = Path::new(dir);
        if path.exists() {
            command.arg("--ro-bind").arg(path).arg(path);
        }
    }
}

pub(super) fn supports_ro_overlay() -> bool {
    let mut probe = Command::new("bwrap");
    probe.args(["--unshare-all", "--die-with-parent", "--new-session"]);
    add_host_runtime_mounts(&mut probe);
    probe.args([
        "--overlay-src", "/usr/share", "--overlay-src", "/etc",
        "--ro-overlay", "/usr/share", "--proc", "/proc", "--dev", "/dev",
        "--tmpfs", "/tmp", "--", "/usr/bin/true",
    ]);
    crate::execution::capture_output(&mut probe).is_ok_and(|output| output.status.success())
}

pub(super) fn add_package_data_mounts(
    command: &mut Command,
    sandbox_dir: &Path,
    overlay_supported: bool,
) -> Result<(), String> {
    // Keep host /usr and /etc available for system libraries, but make data and
    // configuration shipped by the ephemeral package visible at their normal
    // absolute paths (many binaries have these paths compiled in).
    for relative in ["usr/share", "usr/libexec", "etc"] {
        let source = sandbox_dir.join(relative);
        let target = Path::new("/").join(relative);
        if source.is_dir() && target.is_dir() {
            if overlay_supported {
                command.arg("--overlay-src").arg(&target);
                command.arg("--overlay-src").arg(source);
                command.arg("--ro-overlay").arg(target);
            } else if relative != "etc" {
                // Bubblewrap cannot mkdir a new target beneath a read-only /usr bind.
                command.arg("--ro-bind").arg(source).arg(target);
            }
        }
    }
    if !overlay_supported {
        let source_dir = sandbox_dir.join("etc");
        if let Ok(entries) = fs::read_dir(&source_dir) {
            for entry in entries {
                let entry = entry.map_err(|e| format!("Cannot inspect package config: {e}"))?;
                let target = Path::new("/etc").join(entry.file_name());
                if target.exists() {
                    command.arg("--ro-bind").arg(entry.path()).arg(target);
                }
            }
        }
    }
    Ok(())
}

/// Checks if bubblewrap is installed and functional with unprivileged user namespaces.
pub fn check_bwrap_capability() -> Result<(), String> {
    if !crate::is_command_available("bwrap") {
        return Err("Bubblewrap ('bwrap') is not installed.".to_string());
    }

    let mut probe = Command::new("bwrap");
    probe.args(["--unshare-all", "--die-with-parent", "--new-session"]);
    add_host_runtime_mounts(&mut probe);
    let test_res = crate::execution::capture_output(probe.args(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--", "/usr/bin/true"]));

    match test_res {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let msg = if stderr.contains("Setting up uid map")
                || stderr.contains("No permissions")
                || stderr.contains("Permission denied")
                || stderr.contains("unprivileged_userns_clone")
            {
                "Bubblewrap is installed, but kernel unprivileged user namespaces appear restricted or disabled by your system.\n  \
                (e.g., sysctl kernel.unprivileged_userns_clone = 0 or AppArmor restrictions).\n  \
                To run without sandbox container isolation, pass --no-sandbox."
            } else {
                "Bubblewrap container execution test failed.\n  \
                To bypass sandbox container isolation, pass --no-sandbox."
            };
            Err(msg.to_string())
        }
        Err(e) => Err(format!("Failed to execute 'bwrap': {}", e)),
    }
}
