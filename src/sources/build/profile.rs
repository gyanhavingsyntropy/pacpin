//! Temporary makepkg profiles and tracked build invocations.
use super::*;
pub(super) fn expanded(args: &[String], plan: &Plan, root: &Path, output: &Path) -> Vec<String> {
    args.iter()
        .map(|arg| {
            arg.replace("{source}", &root.to_string_lossy())
                .replace("{output}", &output.to_string_lossy())
                .replace("{version}", &plan.version)
        })
        .collect()
}

pub(super) fn profile(build: &BuildRecipe, base: &Path) -> String {
    let mut text = format!("# Load makepkg's original config, conf.d and user overrides without editing them.\nsource_makepkg_config {}\n", crate::utils::shell_quote(&base.to_string_lossy()));
    if build.optimization.cpu == Cpu::Native {
        let flag = if matches!(std::env::consts::ARCH, "arm" | "aarch64") {
            "-mcpu=native"
        } else {
            "-march=native"
        };
        text.push_str(&format!(
            "CFLAGS+=\" {flag}\"\nCXXFLAGS+=\" {flag}\"\nRUSTFLAGS+=\" -C target-cpu=native\"\n"
        ));
    }
    if let Some(level) = build.optimization.level {
        text.push_str(&format!("CFLAGS+=\" -O{level}\"\nCXXFLAGS+=\" -O{level}\"\nRUSTFLAGS+=\" -C opt-level={level}\"\n"));
    }
    if let Some(jobs) = build.jobs {
        text.push_str(&format!("MAKEFLAGS=\"-j{jobs}\"\nNINJAFLAGS=\"-j{jobs}\"\nexport CMAKE_BUILD_PARALLEL_LEVEL={jobs}\nexport CARGO_BUILD_JOBS={jobs}\n"));
    }
    text
}

pub(super) fn run_makepkg(
    work: &Path,
    profile: &Path,
    flag: &str,
    label: &str,
) -> Result<(), String> {
    let mut command = Command::new("makepkg");
    command
        .current_dir(work)
        .args([
            "--nodeps",
            "--noconfirm",
            "--nocheck",
            "--nocolor",
            "--config",
        ])
        .arg(profile)
        .arg(flag);
    for (key, path) in [
        ("PKGDEST", work.to_path_buf()),
        ("BUILDDIR", work.to_path_buf()),
        ("SRCDEST", work.to_path_buf()),
        ("SRCPKGDEST", work.to_path_buf()),
        ("LOGDEST", work.to_path_buf()),
        ("TMPDIR", work.join("tmp")),
        ("CARGO_HOME", work.join("cache/cargo")),
        ("CARGO_TARGET_DIR", work.join("target")),
        ("XDG_CACHE_HOME", work.join("cache")),
        ("PIP_CACHE_DIR", work.join("cache/pip")),
        ("npm_config_cache", work.join("cache/npm")),
    ] {
        command.env(key, path);
    }
    let status = crate::progress::run(
        &mut command,
        crate::progress::Manager::Sources,
        label,
        || {},
    )
    .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Source compilation/packaging failed ({status}); no source package was installed"
        ))
    }
}
