//! Native app preparation and launch; scratch placement does not relocate app output.
use colored::Colorize;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use crate::utils::is_executable_file;
use super::TryOptions;
use super::workspace::{SandboxGuard, create_secure_temp_dir_in};
use super::entrypoints::select_executable;
use super::mounts::check_bwrap_capability;

pub(super) fn try_pacman(cfg: &crate::config::Config, repo: Option<&str>, pkg: &str, args: &[String], options: &TryOptions) -> Result<i32, String> {
    if pkg.is_empty() || !crate::journal::TransactionJournal::is_safe_pkg_name(pkg) {
        return Err(format!("Invalid package name '{}'.", pkg));
    }

    crate::execution::init_signal_handlers();

    if !options.no_sandbox && !options.dry_run {
        if let Err(err_reason) = check_bwrap_capability() {
            if !crate::is_command_available("bwrap") {
                let is_interactive = io::stdin().is_terminal() && !options.noconfirm;
                if is_interactive {
                    print!(
                        "\n{} Bubblewrap ('bwrap') is required for sandbox container isolation but is not installed.\n   Would you like pacpin to install 'bubblewrap' now? [Y/n] ",
                        "::".cyan().bold()
                    );
                    let _ = io::stdout().flush();
                    let mut answer = String::new();
                    if io::stdin().read_line(&mut answer).is_ok() {
                        let trimmed = answer.trim().to_lowercase();
                        if trimmed.is_empty() || trimmed == "y" || trimmed == "yes" {
                            println!("{}", ":: Installing bubblewrap via pacpin...".cyan());
                            let mut config = cfg.clone();
                            let install_opts = crate::pm::InstallOptions {
                                needed: true,
                                noconfirm: false,
                                refresh: false,
                                sysupgrade: false,
                                dry_run: false,
                                forwarded_flags: Vec::new(),
                                channel: None,
                                no_cache: false,
                                wait: None,
                            };
                            match crate::pm::install(
                                &mut config,
                                &["bubblewrap".to_string()],
                                &install_opts,
                            ) {
                                Ok(_) => {
                                    if let Err(e) = check_bwrap_capability() {
                                        return Err(format!(
                                            "Bubblewrap installed but container isolation capability check failed: {}",
                                            e
                                        ));
                                    }
                                    println!(
                                        "{}",
                                        "✔ 'bubblewrap' installed and container isolation verified. Resuming sandbox..."
                                            .green()
                                            .bold()
                                    );
                                }
                                Err(e) => {
                                    return Err(format!("Failed to install 'bubblewrap': {}", e));
                                }
                            }
                        } else {
                            return Err(
                                "Bubblewrap ('bwrap') is not installed.\n  \
                                'pacpin try' defaults to fail-closed container isolation for security.\n  \
                                To bypass sandboxing and run directly on your host (UNSAFE), pass --no-sandbox."
                                    .to_string(),
                            );
                        }
                    } else {
                        return Err("Failed to read user input. Aborting.".to_string());
                    }
                } else {
                    return Err(
                        "Bubblewrap ('bwrap') is not installed on this system.\n  \
                        'pacpin try' defaults to fail-closed container isolation for security.\n  \
                        To run securely in a sandbox container, install bubblewrap:\n    \
                        pin install bubblewrap\n  \
                        To bypass sandboxing and run directly on your host (UNSAFE), pass --no-sandbox."
                            .to_string(),
                    );
                }
            } else {
                return Err(err_reason);
            }
        }
    }

    let manager = crate::db::AlpmManager::with_repo_order(&cfg.repo_order)
        .map_err(|e| format!("Error initializing ALPM: {}", e))?;

    if let Some(version) = manager.installed_version(pkg) {
        println!("{} '{}' is already installed ({version}).", "::".cyan(), pkg);
    }
    let installed = if options.installed || repo.is_none() && !manager.has_repository_target(pkg) {
        let installed = manager.installed_executables(pkg)?;
        if options.installed && installed.is_none() { return Err(format!("Package '{pkg}' is not installed; omit --installed to test a repository download")); }
        installed
    } else { None };
    let installed_execution = installed.is_some();
    if installed_execution && crate::scratch::policy() != crate::scratch::Policy::Auto {
        return Err("--use-ram/--use-storage requires a repository archive to relocate. Installed execution uses the existing files; omit --installed and select a repository target, or omit the scratch flag for this local package.".into());
    }
    let resolved = if installed_execution { Vec::new() } else {
        manager.resolve_download_urls(repo, pkg)
            .map_err(|err| format!("Error resolving download URLs: {err}"))?
    };
    let (bytes, inodes) = ephemeral_footprint(&resolved);
    let base = crate::scratch::choose(crate::scratch::policy(), bytes, inodes)?;
    if options.dry_run {
        if let Some((version, executables)) = &installed {
            let binary = select_executable(executables.clone(), pkg, options.bin.as_deref())?;
            println!(":: Dry-run: installed {pkg} ({version}), executable {}, arguments {args:?}.", binary.display());
        } else {
            println!(":: Dry-run: {:?} workspace in {}; estimated peak {:.1} MiB. Archive paths checked after download.{}", crate::scratch::policy(), base.display(), bytes as f64 / 1_048_576.0, if options.no_cache { " Existing archive cache bypassed." } else { "" });
            for target in &resolved { println!("  {}: {}", target.name, target.url); }
            println!(":: {} execution with arguments {args:?}; no download, extraction or application execution.", if options.no_sandbox { "Host" } else { "Bubblewrap" });
        }
        return Ok(0);
    }
    if options.no_cache {
        println!(":: --no-cache: {}; existing cache files are preserved.", if installed_execution { "installed execution needs no downloads" } else { "temporary downloads bypass the native archive cache" });
    }
    let prefix = if options.is_run_mode { "pacpin-run" } else { "pacpin-try" };
    let sandbox_dir = create_secure_temp_dir_in(&format!("{prefix}-{pkg}"), &base)?;
    let mut guard = SandboxGuard::new(sandbox_dir.clone());
    let view = crate::progress::session_view();
    if !installed_execution {
        println!(":: Temporary app workspace: {} ({:?}); downloads and extracted files are removed on exit.", sandbox_dir.display(), crate::scratch::policy());
    }
    let binary_path = if let Some((version, executables)) = installed {
        println!("{} Using installed '{}' ({version}){}.", "::".cyan(), pkg,
            if options.installed { " (--installed)" } else { "; no configured repository offers this target" });
        select_executable(executables, pkg, options.bin.as_deref())?
    } else {
        println!("{} Resolving download URLs for '{}' via ALPM...", "::".cyan(), pkg);
        if manager.installed_version(pkg).is_some() { println!(":: Testing the repository package; the installed package remains in place. Use --installed to run the local version."); }

    if resolved.len() > 1 {
        println!(
            "{} Found {} package(s) (1 target + {} missing dependencies to sandbox).",
            "::".cyan(),
            resolved.len(),
            resolved.len() - 1
        );
    }

    // Downloads are independent; extraction stays ordered so dependencies cannot
    // race while writing into the same root.
    let archives = fetch_targets(&resolved, options, &sandbox_dir, &mut guard)?;
    println!(":: Archive downloads and checksum verification complete.");
    let (unpacked_bytes, unpacked_inodes, owned_executables) = crate::progress::operation(crate::progress::Manager::Runtime, "Checking extraction capacity", || archives.iter().enumerate().try_fold((32 * 1024 * 1024u64, 128u64, Vec::new()), |(bytes, inodes, owned), (index, archive)| {
        // ALPM resolves dependencies first and the requested target last.
        // Inventory must come from that archive, not the merged dependency root.
        let (payload, paths, owned) = if index + 1 == archives.len() {
            crate::archive::executable_footprint(archive)?
        } else { let (payload, paths) = crate::archive::footprint(archive)?; (payload, paths, owned) };
        Ok::<_, String>((bytes.saturating_add(payload), inodes.saturating_add(paths), owned))
    }))?;
    let remaining = crate::scratch::capacity(&sandbox_dir)?;
    let available = crate::scratch::mem_available_bytes().unwrap_or(0);
    if !crate::scratch::fits(remaining, unpacked_bytes, unpacked_inodes, available) {
        return Err(format!("Insufficient scratch capacity before extraction: {}", crate::scratch::describe(remaining, unpacked_bytes, unpacked_inodes, available)));
    }
    let archive_count = archives.len();
    for (index, tarball_path) in archives.into_iter().enumerate() {
        println!(":: Extracting package {}/{archive_count}...", index + 1);
        crate::progress::operation(crate::progress::Manager::Runtime, &format!("Extracting package {}/{archive_count}", index + 1), || crate::archive::extract_package(&tarball_path, &sandbox_dir))?;
    }

    // 2. Find executable binary
        let paths = owned_executables.into_iter().map(|path| sandbox_dir.join(path))
            .filter(|path| is_executable_file(path)).collect();
        select_executable(paths, pkg, options.bin.as_deref())?
    };

    drop(view);
    super::launch::launch(&sandbox_dir, &binary_path, pkg, args, options, installed_execution, false)
}

pub(super) use crate::preparation::ephemeral_footprint;
#[cfg(test)]
pub(super) use crate::preparation::{download_targets_parallel, ephemeral_download_label};

pub(super) fn fetch_targets(resolved: &[crate::db::DownloadTarget], options: &TryOptions, sandbox_dir: &Path, guard: &mut SandboxGuard) -> Result<Vec<PathBuf>, String> {
    crate::preparation::fetch_targets(resolved, &options.preparation(), sandbox_dir, guard)
}
