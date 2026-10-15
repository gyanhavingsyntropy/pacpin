//! Native app preparation and launch; scratch placement does not relocate app output.
use colored::Colorize;
use std::{env, fs};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use crate::execution::run_tracked_command;
use crate::utils::is_executable_file;
use super::TryOptions;
use super::workspace::{SandboxGuard, create_secure_temp_dir_in};
use super::entrypoints::select_executable;
use super::mounts::{check_bwrap_capability, add_host_runtime_mounts, supports_ro_overlay, add_package_data_mounts, safe_wayland_socket, should_bind_cwd};

pub(super) fn try_pacman(repo: Option<&str>, pkg: &str, args: &[String], options: &TryOptions) -> Result<i32, String> {
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
                            let mut config = crate::config::load_config();
                            let install_opts = crate::pm::InstallOptions {
                                needed: true,
                                noconfirm: false,
                                refresh: false,
                                sysupgrade: false,
                                dry_run: false,
                                forwarded_flags: Vec::new(),
                                channel: None,
                                no_cache: false,
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

    let cfg = crate::config::load_config();
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
    let total = resolved.iter().fold(0u64, |sum, target| sum.saturating_add(target.download_bytes));
    let cache_bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let measured_cache = cache_bytes.clone();
    let measured_dir = sandbox_dir.clone();
    let archives = crate::progress::operation_dynamic(crate::progress::Manager::Runtime, move || {
        let downloaded = fs::read_dir(&measured_dir).into_iter().flatten().flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("pacpin-download-"))
            .filter_map(|entry| entry.metadata().ok().map(|meta| meta.len()))
            .fold(measured_cache.load(Ordering::Relaxed), u64::saturating_add);
        ephemeral_download_label(downloaded, total)
    }, || download_targets_parallel(&resolved, options.allow_unverified, &mut guard, |target, allow_unverified, downloaded| {
        let urls = if target.candidate_urls.is_empty() {
            vec![target.url.clone()]
        } else {
            target.candidate_urls.clone()
        };
        let archive = crate::download::fetch_package_archive(
            &target.name,
            &urls,
            target.sha256.as_deref(),
            allow_unverified,
            downloaded,
            &sandbox_dir,
            !options.no_cache,
        )?;
        if archive.parent() != Some(sandbox_dir.as_path()) { cache_bytes.fetch_add(target.download_bytes, Ordering::Relaxed); }
        Ok(archive)
    }))?;
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
        crate::progress::operation(crate::progress::Manager::Runtime, &format!("Extracting package {}/{archive_count}", index + 1), || crate::download::extract_archive(&tarball_path, &sandbox_dir))?;
    }

    // 2. Find executable binary
        let paths = owned_executables.into_iter().map(|path| sandbox_dir.join(path))
            .filter(|path| is_executable_file(path)).collect();
        select_executable(paths, pkg, options.bin.as_deref())?
    };

    // 3. Construct isolated environment paths
    let usr_bin = sandbox_dir.join("usr").join("bin");
    let usr_sbin = sandbox_dir.join("usr").join("sbin");
    let usr_lib = sandbox_dir.join("usr").join("lib");
    let usr_lib64 = sandbox_dir.join("usr").join("lib64");
    let usr_share = sandbox_dir.join("usr").join("share");

    let current_path = env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string());
    let new_path = format!("{}:{}:{}", usr_bin.display(), usr_sbin.display(), current_path);
    let sandbox_path = format!("{}:{}:/usr/local/bin:/usr/bin:/bin", usr_bin.display(), usr_sbin.display());

    let current_ld = env::var("LD_LIBRARY_PATH").unwrap_or_default();
    let new_ld = if current_ld.is_empty() {
        format!("{}:{}", usr_lib.display(), usr_lib64.display())
    } else {
        format!("{}:{}:{}", usr_lib.display(), usr_lib64.display(), current_ld)
    };
    let sandbox_ld = format!("{}:{}", usr_lib.display(), usr_lib64.display());

    let current_xdg = env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    let new_xdg = format!("{}:{}", usr_share.display(), current_xdg);
    let sandbox_xdg = format!("{}:/usr/local/share:/usr/share", usr_share.display());

    if options.no_sandbox {
        if installed_execution {
            println!("{} Running installed '{}' on host...\n", "::".cyan().bold(), pkg.bold());
        } else if options.is_run_mode {
            println!(
                "{} Running ephemeral '{}' on host (will be removed upon exit)...\n",
                "::".cyan().bold(),
                pkg.bold()
            );
        } else {
            println!(
                "{} Running ephemeral '{}' on host without sandboxing (--no-sandbox; will be removed upon exit)...\n",
                "::".yellow().bold(),
                pkg.bold()
            );
        }
    } else {
        println!(
            "{} Spawning ephemeral sandbox in unshared container (bubblewrap)...",
            "::".cyan()
        );
        println!(
            "  Container policy: clearenv, private tmpfs $HOME, {} network, {} CWD.",
            if options.share_net { "shared" } else { "isolated" },
            if options.rw_cwd { "read-write" } else { "read-only" }
        );
        println!(
            "{} Running {} '{}' (sandbox will be destroyed upon exit)...\n",
            "::".cyan(),
            if installed_execution { "installed" } else { "ephemeral" },
            pkg.bold()
        );
    }

    // Applications can own their terminal UI. Retire preparation's footer
    // before executing them, including interactive host programs.
    drop(view);
    let child_status = if !options.no_sandbox {
        let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let home = env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let user = env::var("USER").unwrap_or_else(|_| "sandbox".to_string());
        let logname = env::var("LOGNAME").unwrap_or_else(|_| user.clone());
        let lang = env::var("LANG").unwrap_or_else(|_| "C.UTF-8".to_string());
        let lc_all = env::var("LC_ALL").unwrap_or_default();
        let term = env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());

        let mut bwrap_cmd = Command::new("bwrap");

        // 1. Clear full environment for security
        bwrap_cmd.arg("--clearenv");
        bwrap_cmd.args(["--die-with-parent", "--new-session"]);

        // 2. Unshare all namespaces by default (IPC, PID, Net, UTS, Cgroup)
        bwrap_cmd.arg("--unshare-all");

        // 3. Network isolation: unshared by default, opt-in via --net / --network
        if options.share_net {
            bwrap_cmd.arg("--share-net");
        }

        // 4. Mount essential host directories read-only
        add_host_runtime_mounts(&mut bwrap_cmd);
        add_package_data_mounts(&mut bwrap_cmd, &sandbox_dir, supports_ro_overlay())?;

        // Virtual filesystems
        bwrap_cmd
            .arg("--proc").arg("/proc")
            .arg("--dev").arg("/dev")
            .arg("--tmpfs").arg("/tmp");

        // 5. Mount private, isolated tmpfs on $HOME so host credentials (~/.ssh, ~/.gnupg, config) are protected
        let home_path = Path::new(&home);
        if home_path.exists() {
            bwrap_cmd.arg("--tmpfs").arg(&home);
        }

        // 6. Never mount home or system directories through the current directory.
        if should_bind_cwd(&cwd, home_path) {
            if options.rw_cwd {
                bwrap_cmd.arg("--bind").arg(&cwd).arg(&cwd);
            } else {
                bwrap_cmd.arg("--ro-bind").arg(&cwd).arg(&cwd);
            }
            bwrap_cmd.arg("--chdir").arg(&cwd);
        } else {
            bwrap_cmd.args(["--chdir", "/tmp"]);
            eprintln!(
                "{}",
                ":: Note: Current directory is not mounted into the sandbox; using private /tmp."
                    .yellow()
            );
        }

        // 7. Mount sandbox directory containing extracted packages read-only
        bwrap_cmd.arg("--ro-bind").arg(&sandbox_dir).arg(&sandbox_dir);

        // 8. GUI opt-in
        if options.gui {
            let x11_socket = Path::new("/tmp/.X11-unix");
            if x11_socket.exists() {
                bwrap_cmd.arg("--ro-bind").arg(x11_socket).arg(x11_socket);
            }
            let dri_dir = Path::new("/dev/dri");
            if dri_dir.exists() {
                bwrap_cmd.arg("--dev-bind").arg(dri_dir).arg(dri_dir);
            }
            if let Ok(disp) = env::var("DISPLAY") {
                bwrap_cmd.args(["--setenv", "DISPLAY", &disp]);
            }
            if let (Ok(runtime), Ok(display)) = (env::var("XDG_RUNTIME_DIR"), env::var("WAYLAND_DISPLAY")) {
                if let Some(socket) = safe_wayland_socket(Path::new(&runtime), &display) {
                    bwrap_cmd.arg("--ro-bind").arg(&socket).arg(Path::new("/tmp").join(&display));
                    bwrap_cmd.args(["--setenv", "XDG_RUNTIME_DIR", "/tmp"]);
                    bwrap_cmd.args(["--setenv", "WAYLAND_DISPLAY", &display]);
                } else {
                    eprintln!("{}", ":: Warning: Wayland socket was not shared; its path or permissions are unsafe.".yellow());
                }
            }
        }

        // 9. Audio opt-in
        if options.audio {
            let snd_dir = Path::new("/dev/snd");
            if snd_dir.exists() {
                bwrap_cmd.arg("--dev-bind").arg(snd_dir).arg(snd_dir);
            }
        }

        // 10. Pass strictly allowlisted environment variables
        bwrap_cmd.args(["--setenv", "PATH", &sandbox_path]);
        bwrap_cmd.args(["--setenv", "LD_LIBRARY_PATH", &sandbox_ld]);
        bwrap_cmd.args(["--setenv", "XDG_DATA_DIRS", &sandbox_xdg]);
        bwrap_cmd.args(["--setenv", "USER", &user]);
        bwrap_cmd.args(["--setenv", "LOGNAME", &logname]);
        bwrap_cmd.args(["--setenv", "HOME", &home]);
        bwrap_cmd.args(["--setenv", "LANG", &lang]);
        if !lc_all.is_empty() {
            bwrap_cmd.args(["--setenv", "LC_ALL", &lc_all]);
        }
        bwrap_cmd.args(["--setenv", "TERM", &term]);

        bwrap_cmd.arg(&binary_path).args(args);
        run_tracked_command(&mut bwrap_cmd)
    } else {
        run_tracked_command(
            Command::new(&binary_path)
                .args(args)
                .env("PATH", &new_path)
                .env("LD_LIBRARY_PATH", &new_ld)
                .env("XDG_DATA_DIRS", &new_xdg)
        )
    };

    drop(guard);

    match child_status {
        Ok(s) => Ok(s.code().unwrap_or(1)),
        Err(e) => Err(format!("Execution failed: {}", e)),
    }
}

pub(super) fn ephemeral_download_label(downloaded: u64, total: u64) -> String {
    let (scale, unit) = if total.max(downloaded) < 1_048_576 { (1024.0, "KiB") } else { (1_048_576.0, "MiB") };
    if total == 0 { return format!("Download/verify {:.1} {unit}", downloaded as f64 / scale); }
    format!("Download/verify {:.1}/{:.1} {unit} ({}%)", downloaded as f64 / scale, total as f64 / scale, downloaded.saturating_mul(100).checked_div(total).unwrap_or(0).min(100))
}

pub(super) fn ephemeral_footprint(targets: &[crate::db::DownloadTarget]) -> (u64, u64) {
    // Archives and extraction coexist. Metadata sizes provide an estimate,
    // not a reservation or a limit on the running app's memory.
    let bytes = targets.iter().fold(32 * 1024 * 1024u64, |sum, target| {
        sum.saturating_add(target.download_bytes)
            .saturating_add(target.installed_bytes.saturating_mul(2))
    });
    // Before downloading we know the number of archive files, not the number
    // of extracted paths. Count those through archive::footprint afterwards.
    (bytes, (targets.len() as u64).saturating_mul(2).saturating_add(128))
}

pub(super) fn download_targets_parallel<F>(
    targets: &[crate::db::DownloadTarget],
    allow_unverified: bool,
    guard: &mut SandboxGuard,
    fetch: F,
) -> Result<Vec<PathBuf>, String>
where
    F: Fn(&crate::db::DownloadTarget, bool, &mut Vec<PathBuf>) -> Result<PathBuf, String> + Sync,
{
    const MAX_PARALLEL_DOWNLOADS: usize = 2;
    let mut archives = Vec::with_capacity(targets.len());
    for batch in targets.chunks(MAX_PARALLEL_DOWNLOADS) {
        let mut first_error = None;
        std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|target| {
                    let fetch = &fetch;
                    scope.spawn(move || {
                        let mut downloaded = Vec::new();
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            fetch(target, allow_unverified, &mut downloaded)
                        }))
                        .unwrap_or_else(|_| Err("Package download worker panicked".to_string()));
                        (result, downloaded)
                    })
                })
                .collect();
            for handle in handles {
                match handle.join() {
                    Ok((result, downloaded)) => {
                        for path in downloaded {
                            guard.downloaded_tarballs.push(path.clone());
                            guard.register_tarball(&path);
                        }
                        match result {
                            Ok(path) => archives.push(path),
                            Err(error) if first_error.is_none() => first_error = Some(error),
                            Err(_) => {}
                        }
                    }
                    Err(_) if first_error.is_none() => {
                        first_error = Some("Package download worker panicked".to_string());
                    }
                    Err(_) => {}
                }
            }
        });
        if let Some(error) = first_error {
            return Err(error);
        }
    }
    Ok(archives)
}
