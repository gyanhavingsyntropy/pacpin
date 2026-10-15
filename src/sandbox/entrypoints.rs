//! Select only executables owned by the requested package.
#[cfg(test)]
use std::fs;
use std::path::PathBuf;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use crate::utils::is_executable_file;

#[cfg(test)]
pub(super) fn find_executable(sandbox_dir: &Path, pkg: &str, requested_bin: Option<&str>) -> Result<PathBuf, String> {
    let usr_bin = sandbox_dir.join("usr").join("bin");
    let usr_sbin = sandbox_dir.join("usr").join("sbin");

    let mut executables = Vec::new();
    for dir in [&usr_bin, &usr_sbin] {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if is_executable_file(&path) { executables.push(path); }
            }
        }
    }
    select_executable(executables, pkg, requested_bin)
}

pub(super) fn select_executable(mut executables: Vec<PathBuf>, pkg: &str, requested_bin: Option<&str>) -> Result<PathBuf, String> {
    executables.sort();
    executables.dedup();
    if let Some(bin_name) = requested_bin {
        if bin_name.is_empty()
            || bin_name == "."
            || bin_name == ".."
            || bin_name.starts_with('-')
            || bin_name.contains(['/', '\\']) || bin_name.chars().any(char::is_control)
        {
            return Err("--bin must name a single executable within the package".to_string());
        }
        let candidates: Vec<_> = executables.iter().filter(|p| p.file_name().is_some_and(|n| n == bin_name)).collect();
        if candidates.len() == 1 { return Ok(candidates[0].clone()); }
        if candidates.len() > 1 { return Err(format!("Requested binary '{bin_name}' is ambiguous within this package: {}", candidates.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", "))); }
        return Err(format!(
            "Requested binary '{}' not found in package executable inventory.",
            bin_name
        ));
    }

    // 1. Direct match: usr/bin/<pkg>
    if let Some(path) = executables.iter().find(|p| p.file_name().is_some_and(|n| n == pkg)) { return Ok(path.clone()); }

    if executables.is_empty() {
        return Err(format!(
            "No executable entry point found in requested package '{}'. Use --bin with a package-owned tool name when available.",
            pkg
        ));
    }

    if executables.len() == 1 {
        return Ok(executables.remove(0));
    }

    // If multiple executables, look for exact match stripping "lib"
    let pkg_cleaned = pkg.trim_start_matches("lib");
    if let Some(pos) = executables.iter().position(|p| {
        p.file_name().map(|n| n.to_string_lossy() == pkg_cleaned).unwrap_or(false)
    }) {
        return Ok(executables.remove(pos));
    }

    // Ambiguous: list candidates and fail closed
    let names: Vec<String> = executables
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect();

    Err(format!(
        "Package '{}' provides multiple executables: {}\n  \
        Specify which binary to run using: pacpin try --bin <binary> {}",
        pkg,
        names.join(", "),
        pkg
    ))
}
