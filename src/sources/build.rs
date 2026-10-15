//! Explicit source compilation recipes. makepkg owns build execution; pacman
//! receives only the locally generated, inspected archive after a successful build.
use super::*;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

mod profile;
mod recipe;
use profile::{expanded, profile, run_makepkg};
pub use recipe::BuildRecipe;
use recipe::Cpu;
pub(super) use recipe::{preview, recipe_digest, validate};

pub(super) fn check_prerequisites(
    config: &Config,
    plan: &Plan,
    permit: Option<&crate::pm::AurBuildFailurePermit>,
) -> Result<(), String> {
    let Some(build) = &plan.recipe.build else {
        return Ok(());
    };
    validate(&plan.recipe, &plan.source)?;
    if permit.is_some() {
        return Err("Independent continuation after AUR failure is unavailable for source-code builds; arbitrary build commands have unverified dependency effects".into());
    }
    if plan.git.as_ref().map(|git| &git.repository) != build.git.as_ref() {
        return Err("Source build input differs from the approved recipe".into());
    }
    if unsafe { libc::geteuid() } == 0 {
        return Err("Source compilation must start as a regular user".into());
    }
    if !crate::is_command_available("makepkg") {
        return Err("Source builds require makepkg".into());
    }
    let manager =
        crate::db::AlpmManager::with_repo_order(&config.repo_order).map_err(|e| e.to_string())?;
    let missing: Vec<_> = build
        .makedepends
        .iter()
        .filter(|dep| {
            manager
                .handle()
                .localdb()
                .pkgs()
                .find_satisfier(dep.as_str())
                .is_none()
        })
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(format!("Missing source build dependencies: {}. Install them explicitly through pacpin before building; no dependencies were installed automatically.", missing.join(" ")));
    }
    for args in &build.commands {
        if args[0].contains('{') || args[0].starts_with("./") {
            continue;
        }
        if !crate::is_command_available(&args[0]) {
            return Err(format!("Source build tool '{}' is unavailable", args[0]));
        }
    }
    Ok(())
}

pub(super) fn check_capacity(plan: &Plan, work: &Path) -> Result<(), String> {
    let Some(build) = &plan.recipe.build else {
        return Ok(());
    };
    let space = crate::scratch::capacity(work)?;
    if crate::scratch::accepts(crate::scratch::policy(), space)
        && crate::scratch::fits(
            space,
            build.headroom_bytes(),
            build.scratch_inodes,
            crate::scratch::mem_available_bytes().unwrap_or(0),
        )
    {
        Ok(())
    } else {
        Err(format!("Insufficient source compilation headroom: need {} MiB and {} inodes, plus the OS RAM reserve for tmpfs; declared estimates are not reservations", build.scratch_mib, build.scratch_inodes))
    }
}

pub(super) fn execute(plan: &Plan, work: &Path, input: &Path) -> Result<PathBuf, String> {
    if unsafe { libc::geteuid() } == 0 {
        return Err("Source compilation must run unprivileged".into());
    }
    let build = plan
        .recipe
        .build
        .as_ref()
        .ok_or("Missing source build recipe")?;
    validate(&plan.recipe, &plan.source)?;
    check_capacity(plan, work)?;
    let payload = if plan.git.is_some() {
        input.to_path_buf()
    } else {
        let payload = work.join("payload");
        fs::create_dir(&payload).map_err(|e| e.to_string())?;
        crate::progress::operation(
            crate::progress::Manager::Sources,
            "Extracting source code",
            || crate::download::extract_archive(input, &payload),
        )?;
        fs::remove_file(input).map_err(|e| e.to_string())?;
        payload
    };
    let root = if plan.recipe.root.is_empty() {
        payload.clone()
    } else {
        payload.join(&plan.recipe.root)
    };
    let root = fs::canonicalize(root).map_err(|_| "Source-code root is missing")?;
    if !root.starts_with(&payload) || !root.is_dir() {
        return Err("Source-code root escapes its workspace".into());
    }
    let output = root.join(&build.output);
    let mut pkgbuild = super::install::pkgbuild(plan, &output)
        .replace("options=('!strip' '!debug')", "options=('!debug')");
    if let Some(lto) = build.optimization.lto {
        pkgbuild = pkgbuild.replace(
            "options=('!debug')",
            &format!("options=('!debug' '{}lto')", if lto { "" } else { "!" }),
        );
    }
    pkgbuild.push_str(&format!(
        "\nbuild() {{\n  cd -- {}\n",
        crate::utils::shell_quote(&root.to_string_lossy())
    ));
    pkgbuild.push_str("  printf ':: Building source with CFLAGS=%s CXXFLAGS=%s RUSTFLAGS=%s\\n' \"$CFLAGS\" \"$CXXFLAGS\" \"$RUSTFLAGS\"\n");
    for args in &build.commands {
        pkgbuild.push_str(&format!(
            "  {} || return $?\n",
            expanded(args, plan, &root, &output)
                .iter()
                .map(|arg| crate::utils::shell_quote(arg))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    pkgbuild.push_str("}\n");
    // Validate staging confinement before makepkg moves it into pkgdir.
    let mut validation = format!("package() {{\n  local staged binary\n  staged=$(realpath -- {}) || return 1\n  [[ -d $staged && $staged == {}/* ]] || return 1\n", crate::utils::shell_quote(&output.to_string_lossy()), crate::utils::shell_quote(&root.to_string_lossy()));
    for relative in plan.recipe.binaries.values() {
        validation.push_str(&format!("  binary=$(realpath -- \"$staged\"/{}) || return 1\n  [[ -f $binary && $binary == \"$staged/\"* ]] || return 1\n", crate::utils::shell_quote(relative)));
    }
    pkgbuild = pkgbuild.replace("package() {\n", &validation);
    fs::write(work.join("PKGBUILD"), pkgbuild).map_err(|e| e.to_string())?;
    let original = std::env::var_os("MAKEPKG_CONF")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/makepkg.conf"));
    let original = if original.is_absolute() {
        original
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(original)
    };
    if !fs::metadata(&original).is_ok_and(|m| m.is_file()) {
        return Err("makepkg configuration must be a readable regular file".into());
    }
    let config_path = work.join("makepkg-source.conf");
    fs::write(&config_path, profile(build, &original)).map_err(|e| e.to_string())?;
    for path in [work.join("tmp"), work.join("cache/cargo")] {
        fs::create_dir_all(path).map_err(|e| e.to_string())?;
    }
    run_makepkg(work, &config_path, "--noarchive", "Compiling source code")?;
    let installed_payload = work
        .join("pkg")
        .join(&plan.name)
        .join("opt")
        .join(&plan.name);
    let canonical =
        fs::canonicalize(&installed_payload).map_err(|_| "Build produced no staged payload")?;
    if canonical != installed_payload {
        return Err("Staged build payload is a symlink".into());
    }
    for relative in plan.recipe.binaries.values() {
        let binary = fs::canonicalize(installed_payload.join(relative))
            .map_err(|_| "Build did not produce a declared executable")?;
        if !binary.starts_with(&canonical) || !binary.is_file() {
            return Err("Build executable escapes the staged payload".into());
        }
        super::install::verify_elf(&binary, false)?;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    let receipt = serde_json::json!({"schema":2,"source":plan.alias,"package":plan.name,"tag":plan.tag,"version":plan.version,
        "sha256":plan.asset.sha256,"git":plan.git,"recipe_sha256":recipe_digest(plan),"build":{"optimization":build.optimization,"jobs":build.jobs}});
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(installed_payload.join(".pacpin-source.json"))
        .map_err(|_| "Build supplied a reserved provenance path")?;
    file.write_all(receipt.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    drop(file);
    // --repackage reruns package(), even though it skips build(). Stage the
    // inspected payload separately and use the shared packaging-only recipe.
    let validated = work.join("compiled-payload");
    fs::rename(&installed_payload, &validated).map_err(|e| e.to_string())?;
    fs::write(
        work.join("PKGBUILD"),
        super::install::pkgbuild(plan, &validated),
    )
    .map_err(|e| e.to_string())?;
    for directory in ["payload", "git-source", "src", "target", "cache"] {
        let path = work.join(directory);
        if path.exists() {
            crate::execution::remove_owned_tree(&path)
                .map_err(|e| format!("Cannot release compilation scratch: {e}"))?;
        }
    }
    check_capacity(plan, work)?;
    check_archive_capacity(&validated, work)?;
    run_makepkg(
        work,
        &config_path,
        "--repackage",
        "Packaging compiled source",
    )?;
    let packages: Vec<_> = fs::read_dir(work)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                name.to_string_lossy().contains(".pkg.tar.")
                    && !name.to_string_lossy().ends_with(".sig")
            })
        })
        .collect();
    if packages.len() != 1 {
        return Err("Source build did not produce exactly one package".into());
    }
    for directory in [
        "payload",
        "git-source",
        "pkg",
        "src",
        "target",
        "cache",
        "tmp",
    ] {
        let path = work.join(directory);
        if path.exists() {
            crate::execution::remove_owned_tree(&path)
                .map_err(|e| format!("Cannot release compilation scratch: {e}"))?;
        }
    }
    Ok(packages[0].clone())
}

fn check_archive_capacity(payload: &Path, work: &Path) -> Result<(), String> {
    let mut pending = vec![(payload.to_path_buf(), 0)];
    let (mut bytes, mut entries) = (0u64, 0u64);
    while let Some((path, depth)) = pending.pop() {
        if depth > 256 || entries > 1_000_000 {
            return Err("Compiled output exceeds the inspection limit".into());
        }
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        entries += 1;
        if metadata.is_dir() {
            for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
                if pending.len() as u64 + entries >= 1_000_000 {
                    return Err("Compiled output exceeds the inspection limit".into());
                }
                pending.push((entry.map_err(|e| e.to_string())?.path(), depth + 1));
            }
        } else if metadata.is_file() || metadata.file_type().is_symlink() {
            bytes = bytes.saturating_add(metadata.len());
        } else {
            return Err("Compiled output contains a special file".into());
        }
    }
    let space = crate::scratch::capacity(work)?;
    if crate::scratch::fits(
        space,
        bytes
            .saturating_add(entries.saturating_mul(16_384))
            .saturating_add(64 * 1_048_576),
        entries.saturating_add(32),
        crate::scratch::mem_available_bytes().unwrap_or(0),
    ) {
        Ok(())
    } else {
        Err(
            "Insufficient scratch capacity for the actual compiled output and package archive"
                .into(),
        )
    }
}

#[cfg(test)]
mod tests;
