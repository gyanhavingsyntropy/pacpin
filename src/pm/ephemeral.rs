//! Download AUR recipes and build archives without any host installation.
use crate::{config::Config, execution::BuildWorkspace};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug)]
pub(crate) struct BuildError { pub build_failed: bool, message: String }
impl From<String> for BuildError { fn from(message: String) -> Self { Self { build_failed: false, message } } }
impl From<&str> for BuildError { fn from(message: &str) -> Self { message.to_owned().into() } }
impl std::fmt::Display for BuildError { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.message) } }

fn read_recipe_file(path: &Path) -> Result<String, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("AUR recipe input must be a regular file".into());
    }
    let mut text = String::new();
    file.take(131_073)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() > 131_072 {
        return Err("AUR recipe input exceeds 128 KiB".into());
    }
    Ok(text)
}

/// Read declarative metadata; never source PKGBUILD to discover dependencies.
pub(crate) fn aur_recipe_inputs(
    recipe: &Path,
    target: &str,
) -> Result<(Vec<String>, String), String> {
    let info = read_recipe_file(&recipe.join(".SRCINFO"))?;
    let dependencies = recipe_dependencies(&info, target)?;
    Ok((dependencies, read_recipe_file(&recipe.join("PKGBUILD"))?))
}

fn recipe_dependencies(info: &str, target: &str) -> Result<Vec<String>, String> {
    let mut section = None;
    let mut found = false;
    let mut dependencies = Vec::new();
    let configured = crate::pacman_config::architectures();
    for line in info.lines() {
        let Some((key, value)) = line.trim().split_once(" = ") else {
            continue;
        };
        if key == "pkgname" {
            section = Some(value);
            found |= value == target;
        }
        let applicable = section.is_none_or(|name| name == target);
        if applicable
            && ["depends", "makedepends"]
                .iter()
                .any(|base| {
                    key == *base
                        || key == format!("{base}_{}", std::env::consts::ARCH)
                        || configured.iter().any(|arch| key == format!("{base}_{arch}"))
                })
        {
            if !crate::utils::is_safe_pkg_name(crate::aur::clean_dep_name(value))
                || value.chars().any(char::is_control)
                || value.len() > 256
            {
                return Err("Invalid dependency in AUR .SRCINFO".into());
            }
            dependencies.push(value.into());
        }
    }
    if !found {
        return Err(format!(
            "Fetched AUR recipe does not declare requested package '{target}'"
        ));
    }
    dependencies.sort();
    dependencies.dedup();
    if dependencies.len() > 128 {
        return Err("Too many AUR recipe dependencies".into());
    }
    Ok(dependencies)
}

pub(crate) fn fetch_aur_recipe(
    config: &Config,
    base: &str,
    work: &Path,
    view: &BuildWorkspace,
) -> Result<PathBuf, String> {
    if !super::workspace::is_paru(&config.options.helper) {
        return Err("AUR try/run currently requires Paru's recipe-only -G operation".into());
    }
    if !crate::utils::is_safe_pkg_name(base) {
        return Err("Invalid AUR package base".into());
    }
    let mut command = Command::new(&config.options.helper);
    command.current_dir(work).args(["-G", "--aur", "--", base]);
    let mut command = view.command(command, work)?;
    let status = crate::progress::run(
        &mut command,
        crate::progress::Manager::Runtime,
        "Fetching AUR recipe",
        || {},
    )
    .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("AUR recipe download failed ({status})"));
    }
    let recipe = work.join(base);
    let actual = recipe.canonicalize().map_err(|e| e.to_string())?;
    if actual.parent() != Some(work) || !actual.is_dir() {
        return Err("AUR recipe escaped its workspace".into());
    }
    Ok(actual)
}

pub(crate) fn build_aur_archive(
    recipe: &Path,
    work: &Path,
    view: &BuildWorkspace,
) -> Result<Vec<PathBuf>, BuildError> {
    let output = work.join("packages");
    fs::create_dir(&output).map_err(|e| e.to_string())?;
    let mut command = Command::new("makepkg");
    command
        .current_dir(recipe)
        .args(["--nodeps", "--nocheck", "--noconfirm", "--nocolor"])
        .env("PKGDEST", &output)
        .env("BUILDDIR", work.join("build"))
        .env("SRCDEST", work.join("sources"))
        .env("LOGDEST", work.join("logs"));
    for dir in ["build", "sources", "logs"] {
        fs::create_dir(work.join(dir)).map_err(|e| e.to_string())?;
    }
    let mut command = view.command(command, work)?;
    let mut evidence = super::continuation::FailureEvidence::default();
    let status = crate::progress::run_observed(
        &mut command,
        crate::progress::Manager::Runtime,
        "Building temporary AUR package",
        || {},
        |line| evidence.observe(line),
    )
    .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(BuildError { build_failed: status.code().is_some_and(|code| code != 130 && code != 137) && evidence.verify().is_ok(), message: format!("Temporary AUR build failed ({status}); no package was installed") });
    }
    let mut packages = Vec::new();
    for entry in fs::read_dir(output).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.contains(".pkg.tar.") && !name.ends_with(".sig") {
            let metadata = entry.file_type().map_err(|e| e.to_string())?;
            if !metadata.is_file() {
                return Err("AUR output archive must be a regular file".into());
            }
            packages.push(entry.path());
        }
    }
    if packages.is_empty() || packages.len() > 64 {
        return Err("Invalid number of AUR output archives".into());
    }
    Ok(packages)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selects_requested_split_package_and_current_arch() {
        let info = format!("pkgbase = base\nmakedepends = make\ndepends_{} = common>=2\npkgname = first\ndepends = one\npkgname = second\ndepends = two\ndepends_aarch64 = arm-only\n", std::env::consts::ARCH);
        let deps = recipe_dependencies(&info, "first").unwrap();
        assert!(deps.contains(&"common>=2".into()));
        assert!(deps.contains(&"make".into()));
        assert!(deps.contains(&"one".into()));
        assert!(!deps.contains(&"two".into()));
        assert!(recipe_dependencies(&info, "missing").is_err());

        let info_arch = format!("pkgbase = base\nmakedepends_{} = tool\npkgname = first\n", std::env::consts::ARCH);
        let deps_arch = recipe_dependencies(&info_arch, "first").unwrap();
        assert!(deps_arch.contains(&"tool".into()));
    }
}
