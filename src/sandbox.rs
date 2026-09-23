// pacpin - Declarative Package Resolver & Upgrade Engine for Arch Linux / CachyOS
// Copyright (C) 2026 Gyan <330976822+gyanhavingsyntropy@users.noreply.github.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use colored::Colorize;
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{exit, Command};
use std::time::{SystemTime, UNIX_EPOCH};

struct SandboxGuard {
    sandbox_dir: PathBuf,
    downloaded_tarballs: Vec<PathBuf>,
}

impl Drop for SandboxGuard {
    fn drop(&mut self) {
        if self.sandbox_dir.exists() {
            let _ = fs::remove_dir_all(&self.sandbox_dir);
        }
        for tarball in &self.downloaded_tarballs {
            if tarball.exists() {
                let _ = fs::remove_file(tarball);
            }
        }
    }
}

pub fn parse_target(target: &str) -> (Option<&str>, &str) {
    if let Some((prefix, rest)) = target.split_once('/') {
        (Some(prefix), rest)
    } else if let Some((prefix, rest)) = target.split_once(':') {
        (Some(prefix), rest)
    } else if let Some(rest) = target.strip_prefix("nixpkgs#") {
        (Some("nix"), rest)
    } else {
        (None, target)
    }
}

#[derive(Debug, Clone, Default)]
pub struct TryOptions {
    pub no_sandbox: bool,
    pub share_net: bool,
    pub rw_cwd: bool,
    pub gui: bool,
    pub audio: bool,
    pub bin: Option<String>,
    pub allow_unverified: bool,
    pub is_run_mode: bool,
}

impl TryOptions {
    pub fn for_run() -> Self {
        Self {
            no_sandbox: true,
            share_net: true,
            rw_cwd: true,
            gui: false,
            audio: false,
            bin: None,
            allow_unverified: false,
            is_run_mode: true,
        }
    }
}

pub fn cmd_try(target: &str, args: &[String], options: &TryOptions) {
    let (prefix, pkg) = parse_target(target);

    let res = match prefix {
        Some("flatpak") => try_flatpak(pkg, args),
        Some("nix") | Some("nixpkgs") => try_nix(pkg, args),
        Some("pipx") => try_pipx(pkg, args),
        Some(repo) => try_pacman(Some(repo), pkg, args, options),
        None => try_pacman(None, pkg, args, options),
    };

    match res {
        Ok(code) => exit(code),
        Err(err) => {
            eprintln!("{}", format!("Error: {}", err).red().bold());
            exit(1);
        }
    }
}

fn try_flatpak(app_id: &str, args: &[String]) -> Result<i32, String> {
    if !is_safe_flatpak_id(app_id) {
        return Err(format!(
            "Invalid Flatpak Application ID '{}'. Flatpak IDs follow reverse-DNS notation, e.g. 'org.gnome.Calculator'.",
            app_id
        ));
    }

    if !crate::is_command_available("flatpak") {
        return Err("'flatpak' command not found on this system. Install flatpak or enable Flatpak integration in 'pacpin init'.".to_string());
    }

    // Check if the application is already installed on the system
    let is_already_installed = Command::new("flatpak")
        .args(["info", app_id])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    struct FlatpakGuard<'a> {
        app_id: &'a str,
        needs_cleanup: bool,
    }

    impl<'a> Drop for FlatpakGuard<'a> {
        fn drop(&mut self) {
            if self.needs_cleanup {
                println!("\n{} Purging ephemeral Flatpak '{}'...", "::".cyan(), self.app_id);
                let _ = Command::new("flatpak")
                    .args(["uninstall", "--user", "-y", self.app_id])
                    .status();

                if let Ok(home) = env::var("HOME") {
                    let app_data = Path::new(&home).join(".var").join("app").join(self.app_id);
                    if app_data.exists() {
                        let _ = fs::remove_dir_all(&app_data);
                    }
                }
                println!("✔ Ephemeral Flatpak '{}' removed.", self.app_id.green());
            }
        }
    }

    let mut guard = FlatpakGuard {
        app_id,
        needs_cleanup: false,
    };

    if !is_already_installed {
        println!(
            "{} Installing ephemeral Flatpak '{}' (--user)...",
            "::".cyan(),
            app_id.bold()
        );
        let install_status = Command::new("flatpak")
            .args(["install", "--user", "--noninteractive", "-y", "flathub", app_id])
            .status()
            .map_err(|e| format!("Failed to run flatpak install: {}", e))?;

        if install_status.success() {
            guard.needs_cleanup = true;
        } else {
            return Err(format!(
                "Flatpak installation failed with exit code {}.",
                install_status.code().unwrap_or(1)
            ));
        }
    } else {
        println!(
            "{} Flatpak '{}' is already installed. Running directly...",
            "::".cyan(),
            app_id.bold()
        );
    }

    println!(
        "{} Running Flatpak '{}'{}...\n",
        "::".cyan(),
        app_id.bold(),
        if guard.needs_cleanup { " (will be purged upon exit)" } else { "" }
    );

    let mut run_args = vec!["run", app_id];
    run_args.extend(args.iter().map(|s| s.as_str()));

    let child_status = Command::new("flatpak")
        .args(&run_args)
        .status()
        .map_err(|e| format!("Failed to execute flatpak run: {}", e))?;

    drop(guard);
    Ok(child_status.code().unwrap_or(0))
}

fn try_nix(pkg: &str, args: &[String]) -> Result<i32, String> {
    if !crate::journal::TransactionJournal::is_safe_pkg_name(pkg) {
        return Err(format!("Invalid Nix package name '{}'.", pkg));
    }

    if !crate::is_command_available("nix") {
        return Err("'nix' command not found on this system.".to_string());
    }

    println!(
        "{} Running ephemeral Nix package '{}' (nix run)...",
        "::".cyan(),
        pkg.bold()
    );

    let target = format!("nixpkgs#{}", pkg);
    let mut nix_args = vec!["run", target.as_str()];
    if !args.is_empty() {
        nix_args.push("--");
        nix_args.extend(args.iter().map(|s| s.as_str()));
    }

    let status = Command::new("nix")
        .args(&nix_args)
        .status()
        .map_err(|e| format!("Failed to execute nix: {}", e))?;

    if status.success() {
        return Ok(0);
    }

    // Fallback: nix-shell -p <pkg> --run "<pkg> [args...]"
    println!(
        "{} 'nix run' exited with {}. Trying 'nix-shell' fallback...",
        "::".yellow(),
        status.code().unwrap_or(1)
    );
    let mut cmd_parts = vec![crate::utils::shell_quote(pkg)];
    cmd_parts.extend(args.iter().map(|s| crate::utils::shell_quote(s)));
    let shell_cmd = cmd_parts.join(" ");
    let shell_status = Command::new("nix-shell")
        .args(["-p", pkg, "--run", &shell_cmd])
        .status()
        .map_err(|e| format!("Failed to run nix-shell: {}", e))?;

    Ok(shell_status.code().unwrap_or(0))
}

fn try_pipx(pkg: &str, args: &[String]) -> Result<i32, String> {
    if !crate::journal::TransactionJournal::is_safe_pkg_name(pkg) {
        return Err(format!("Invalid Pipx package name '{}'.", pkg));
    }

    if !crate::is_command_available("pipx") {
        return Err("'pipx' command not found on this system.".to_string());
    }

    println!(
        "{} Running ephemeral Pipx package '{}' (pipx run)...",
        "::".cyan(),
        pkg.bold()
    );

    let mut pipx_args = vec!["run", pkg];
    pipx_args.extend(args.iter().map(|s| s.as_str()));

    let status = Command::new("pipx")
        .args(&pipx_args)
        .status()
        .map_err(|e| format!("Failed to execute pipx: {}", e))?;

    Ok(status.code().unwrap_or(0))
}

fn create_secure_temp_dir(prefix: &str) -> Result<PathBuf, String> {
    let sub = if prefix.starts_with("pacpin-run") { "run" } else { "try" };
    let base_dir = if let Ok(xdg_runtime) = env::var("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(xdg_runtime).join("pacpin").join(sub);
        let _ = fs::create_dir_all(&p);
        if p.is_dir() {
            p
        } else {
            env::temp_dir()
        }
    } else {
        env::temp_dir()
    };

    #[cfg(unix)]
    use std::os::unix::fs::DirBuilderExt;

    for _ in 0..100 {
        let mut random_bytes = [0u8; 16];
        if let Ok(mut f) = fs::File::open("/dev/urandom") {
            use std::io::Read;
            let _ = f.read_exact(&mut random_bytes);
        } else {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let pid = std::process::id() as u128;
            random_bytes = (nanos ^ (pid << 64)).to_le_bytes();
        }
        let hex = random_bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>();
        let target = base_dir.join(format!("{}-{}", prefix, hex));

        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);

        if builder.create(&target).is_ok() {
            return Ok(target);
        }
    }
    Err("Failed to create secure sandbox directory with exclusive permissions".to_string())
}

fn create_secure_temp_file(dir: &Path, prefix: &str, ext: &str) -> Result<PathBuf, String> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    for _ in 0..100 {
        let mut random_bytes = [0u8; 16];
        if let Ok(mut f) = fs::File::open("/dev/urandom") {
            use std::io::Read;
            let _ = f.read_exact(&mut random_bytes);
        } else {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let pid = std::process::id() as u128;
            random_bytes = (nanos ^ (pid << 64)).to_le_bytes();
        }
        let hex = random_bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>();
        let target = dir.join(format!("{}-{}.{}", prefix, hex, ext));

        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600);

        if opts.open(&target).is_ok() {
            return Ok(target);
        }
    }
    Err("Failed to create secure temporary file".to_string())
}

pub fn is_safe_tar_entry(entry: &str) -> bool {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Absolute paths or drive letters
    if trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return false;
    }
    // Path traversal components
    for component in trimmed.split(['/', '\\']) {
        if component == ".." {
            return false;
        }
    }
    true
}

pub fn is_safe_link_destination(src: &str, dst: &str) -> bool {
    let trimmed_dst = dst.trim();
    if trimmed_dst.is_empty() {
        return false;
    }
    // No absolute paths or drive letters
    if trimmed_dst.starts_with('/') || trimmed_dst.starts_with('\\') {
        return false;
    }
    if trimmed_dst.len() >= 2
        && trimmed_dst.chars().next().unwrap().is_ascii_alphabetic()
        && trimmed_dst.chars().nth(1) == Some(':')
    {
        return false;
    }

    // Calculate directory depth of the link's source relative to sandbox root
    let src_trimmed = src.trim().trim_start_matches(['/', '\\']);
    let parent_components: Vec<&str> = src_trimmed
        .split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    let mut depth: isize = if parent_components.is_empty() {
        0
    } else {
        (parent_components.len() - 1) as isize
    };

    for comp in trimmed_dst.split(['/', '\\']) {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." {
            depth -= 1;
            if depth < 0 {
                // Link traverses above the archive root!
                return false;
            }
        } else {
            depth += 1;
        }
    }

    true
}

pub fn is_safe_tar_listing_line(line: &str) -> Result<(String, u64), String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.is_empty() {
        return Ok((String::new(), 0));
    }
    if tokens.len() < 6 {
        // Shorter output: fallback to raw entry check
        let entry = tokens.last().unwrap_or(&"");
        if !is_safe_tar_entry(entry) {
            return Err(format!("Unsafe archive path: {}", entry));
        }
        return Ok((entry.to_string(), 0));
    }

    let size = tokens[2].parse::<u64>().unwrap_or(0);
    let full_path = tokens[5..].join(" ");

    if let Some((src, dst)) = full_path.split_once(" -> ") {
        if !is_safe_tar_entry(src) {
            return Err(format!("Unsafe symlink source entry: {}", src));
        }
        if !is_safe_link_destination(src, dst) {
            return Err(format!("Unsafe symlink destination '{}' points outside sandbox root", dst));
        }
        Ok((src.to_string(), size))
    } else if let Some((src, dst)) = full_path.split_once(" link to ") {
        if !is_safe_tar_entry(src) {
            return Err(format!("Unsafe hardlink source entry: {}", src));
        }
        if !is_safe_link_destination(src, dst) {
            return Err(format!("Unsafe hardlink destination '{}' points outside sandbox root", dst));
        }
        Ok((src.to_string(), size))
    } else {
        if !is_safe_tar_entry(&full_path) {
            return Err(format!("Unsafe path traversal entry: {}", full_path));
        }
        Ok((full_path, size))
    }
}

fn try_pacman(repo: Option<&str>, pkg: &str, args: &[String], options: &TryOptions) -> Result<i32, String> {
    if pkg.is_empty() || !crate::journal::TransactionJournal::is_safe_pkg_name(pkg) {
        return Err(format!("Invalid package name '{}'.", pkg));
    }

    let mut bwrap_available = crate::is_command_available("bwrap");

    if !options.no_sandbox && !bwrap_available {
        let is_interactive = io::stdin().is_terminal();
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
                    let install_opts = crate::installer::InstallOptions {
                        needed: true,
                        noconfirm: false,
                        refresh: false,
                        dry_run: false,
                        forwarded_flags: Vec::new(),
                    };
                    match crate::installer::install_packages(
                        &mut config,
                        &["bubblewrap".to_string()],
                        &install_opts,
                    ) {
                        Ok(_) => {
                            bwrap_available = crate::is_command_available("bwrap");
                            if !bwrap_available {
                                return Err(
                                    "Package installation completed but 'bwrap' executable was not found in PATH."
                                        .to_string(),
                                );
                            }
                            println!(
                                "{}",
                                "✔ 'bubblewrap' installed successfully. Resuming sandbox..."
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
                sudo pacman -S bubblewrap\n  \
                To bypass sandboxing and run directly on your host (UNSAFE), pass --no-sandbox."
                    .to_string(),
            );
        }
    }

    let prefix = if options.is_run_mode { "pacpin-run" } else { "pacpin-try" };
    let sandbox_dir = create_secure_temp_dir(&format!("{}-{}", prefix, pkg))?;

    let mut guard = SandboxGuard {
        sandbox_dir: sandbox_dir.clone(),
        downloaded_tarballs: Vec::new(),
    };

    let cfg = crate::config::load_config();
    let manager = crate::db::AlpmManager::with_repo_order(&cfg.repo_order)
        .map_err(|e| format!("Error initializing ALPM: {}", e))?;

    println!("{} Resolving download URLs for '{}' via ALPM...", "::".cyan(), pkg);
    let resolved = manager.resolve_download_urls(repo, pkg)
        .map_err(|err| format!("Error resolving download URLs: {}", err))?;

    if resolved.len() > 1 {
        println!(
            "{} Found {} package(s) (1 target + {} missing dependencies to sandbox).",
            "::".cyan(),
            resolved.len(),
            resolved.len() - 1
        );
    }

    const MAX_ARCHIVE_ENTRIES: usize = 50_000;
    const MAX_UNPACKED_BYTES: u64 = 5 * 1024 * 1024 * 1024; // 5 GB limit

    // 1. Download, verify, and safely inspect archives before extraction
    for target in &resolved {
        let candidate_urls = if target.candidate_urls.is_empty() {
            vec![target.url.clone()]
        } else {
            target.candidate_urls.clone()
        };
        let tarball_path = get_or_download_package(
            &target.name,
            &candidate_urls,
            target.sha256.as_deref(),
            options.allow_unverified,
            &mut guard,
        )?;

        // Tar Path Traversal & Expansion Bomb Guard: inspect entries and link targets before extraction
        let tar_tvf = Command::new("tar")
            .args(["-tvf", tarball_path.to_str().unwrap()])
            .output()
            .map_err(|e| format!("Failed to inspect archive '{}': {}", tarball_path.display(), e))?;

        if !tar_tvf.status.success() {
            return Err(format!(
                "Failed to list archive contents for '{}' (exit code {:?})",
                tarball_path.display(),
                tar_tvf.status.code()
            ));
        }

        let listing = String::from_utf8_lossy(&tar_tvf.stdout);
        let mut total_entries = 0usize;
        let mut total_size = 0u64;

        for line in listing.lines() {
            let line_trimmed = line.trim();
            if line_trimmed.is_empty() {
                continue;
            }
            total_entries += 1;
            if total_entries > MAX_ARCHIVE_ENTRIES {
                return Err(format!(
                    "Security violation: archive '{}' exceeds maximum allowed file count ({})",
                    tarball_path.display(),
                    MAX_ARCHIVE_ENTRIES
                ));
            }

            let (_entry, size) = is_safe_tar_listing_line(line_trimmed)
                .map_err(|e| format!("Security violation in archive '{}': {}. Extraction aborted.", tarball_path.display(), e))?;

            total_size = total_size.saturating_add(size);
            if total_size > MAX_UNPACKED_BYTES {
                return Err(format!(
                    "Security violation: archive '{}' uncompressed size exceeds safety limit of 5 GB. Extraction aborted.",
                    tarball_path.display()
                ));
            }
        }

        let extract_status = Command::new("tar")
            .args([
                "-xf",
                tarball_path.to_str().unwrap(),
                "-C",
                sandbox_dir.to_str().unwrap(),
                "--no-same-owner",
                "--no-same-permissions",
                "--delay-directory-restore",
            ])
            .status()
            .map_err(|e| format!("Failed to execute tar on '{}': {}", target.name, e))?;

        if !extract_status.success() {
            return Err(format!(
                "Extraction of '{}' failed with exit code {}.",
                target.name,
                extract_status.code().unwrap_or(1)
            ));
        }
    }

    // 2. Find executable binary
    let binary_path = find_executable(&sandbox_dir, pkg, options.bin.as_deref())?;

    // 3. Construct isolated environment paths
    let usr_bin = sandbox_dir.join("usr").join("bin");
    let usr_sbin = sandbox_dir.join("usr").join("sbin");
    let usr_lib = sandbox_dir.join("usr").join("lib");
    let usr_lib64 = sandbox_dir.join("usr").join("lib64");
    let usr_share = sandbox_dir.join("usr").join("share");

    let current_path = env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string());
    let new_path = format!("{}:{}:{}", usr_bin.display(), usr_sbin.display(), current_path);

    let current_ld = env::var("LD_LIBRARY_PATH").unwrap_or_default();
    let new_ld = if current_ld.is_empty() {
        format!("{}:{}", usr_lib.display(), usr_lib64.display())
    } else {
        format!("{}:{}:{}", usr_lib.display(), usr_lib64.display(), current_ld)
    };

    let current_xdg = env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    let new_xdg = format!("{}:{}", usr_share.display(), current_xdg);

    if options.no_sandbox {
        if options.is_run_mode {
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
            "{} Running ephemeral '{}' (sandbox will be destroyed upon exit)...\n",
            "::".cyan(),
            pkg.bold()
        );
    }

    let child_status = if !options.no_sandbox && bwrap_available {
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

        // 2. Unshare all namespaces by default (IPC, PID, Net, UTS, Cgroup)
        bwrap_cmd.arg("--unshare-all");

        // 3. Network isolation: unshared by default, opt-in via --net / --network
        if options.share_net {
            bwrap_cmd.arg("--share-net");
        }

        // 4. Mount essential host directories read-only
        for dir in &["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc"] {
            let p = Path::new(dir);
            if p.exists() {
                bwrap_cmd.arg("--ro-bind").arg(p).arg(p);
            }
        }

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

        // 6. Current working directory: read-only by default, read-write only if --rw
        if cwd.exists() && !cwd.starts_with("/tmp") {
            if options.rw_cwd {
                bwrap_cmd.arg("--bind").arg(&cwd).arg(&cwd);
            } else {
                bwrap_cmd.arg("--ro-bind").arg(&cwd).arg(&cwd);
            }
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
            if let Ok(wayland_disp) = env::var("WAYLAND_DISPLAY") {
                bwrap_cmd.args(["--setenv", "WAYLAND_DISPLAY", &wayland_disp]);
            }
            if let Ok(xdg_runtime) = env::var("XDG_RUNTIME_DIR") {
                let wl_sock = Path::new(&xdg_runtime).join(env::var("WAYLAND_DISPLAY").unwrap_or_default());
                if wl_sock.exists() {
                    bwrap_cmd.arg("--ro-bind").arg(&wl_sock).arg(&wl_sock);
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
        bwrap_cmd.args(["--setenv", "PATH", &new_path]);
        bwrap_cmd.args(["--setenv", "LD_LIBRARY_PATH", &new_ld]);
        bwrap_cmd.args(["--setenv", "XDG_DATA_DIRS", &new_xdg]);
        bwrap_cmd.args(["--setenv", "USER", &user]);
        bwrap_cmd.args(["--setenv", "LOGNAME", &logname]);
        bwrap_cmd.args(["--setenv", "HOME", &home]);
        bwrap_cmd.args(["--setenv", "LANG", &lang]);
        if !lc_all.is_empty() {
            bwrap_cmd.args(["--setenv", "LC_ALL", &lc_all]);
        }
        bwrap_cmd.args(["--setenv", "TERM", &term]);

        bwrap_cmd.arg(&binary_path).args(args);
        bwrap_cmd.status()
    } else {
        Command::new(&binary_path)
            .args(args)
            .env("PATH", &new_path)
            .env("LD_LIBRARY_PATH", &new_ld)
            .env("XDG_DATA_DIRS", &new_xdg)
            .status()
    };

    drop(guard);

    match child_status {
        Ok(s) => Ok(s.code().unwrap_or(0)),
        Err(e) => Err(format!("Execution failed: {}", e)),
    }
}

fn get_or_download_package(
    pkg: &str,
    candidate_urls: &[String],
    expected_sha: Option<&str>,
    allow_unverified: bool,
    guard: &mut SandboxGuard,
) -> Result<PathBuf, String> {
    // 1. Check local pacman cache first (/var/cache/pacman/pkg/)
    let cache_dir = Path::new("/var/cache/pacman/pkg");
    if cache_dir.exists() {
        if let Ok(entries) = fs::read_dir(cache_dir) {
            let mut matching: Vec<PathBuf> = entries
                .flatten()
                .filter(|e| {
                    let file_name = e.file_name();
                    let name_str = file_name.to_string_lossy();
                    (name_str.ends_with(".pkg.tar.zst") || name_str.ends_with(".pkg.tar.xz"))
                        && (name_str.starts_with(&format!("{}-", pkg)))
                })
                .map(|e| e.path())
                .collect();

            if !matching.is_empty() {
                matching.sort_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok());
                let found = matching.last().unwrap().clone();

                let cache_valid = if let Some(expected) = expected_sha {
                    if let Ok(out) = Command::new("sha256sum").arg(&found).output() {
                        if out.status.success() {
                            let stdout = String::from_utf8_lossy(&out.stdout);
                            let computed = stdout.split_whitespace().next().unwrap_or("");
                            computed.eq_ignore_ascii_case(expected)
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    allow_unverified
                };

                if cache_valid {
                    println!("{} Found verified '{}' in local pacman cache.", "::".cyan(), pkg);
                    return Ok(found);
                } else if expected_sha.is_some() {
                    println!(
                        "{} Cached package for '{}' failed checksum verification or was outdated; downloading fresh...",
                        "::".yellow(),
                        pkg
                    );
                }
            }
        }
    }

    // 2. Fail-closed: verify that checksum is available unless user explicitly opted out
    let expected = match expected_sha {
        Some(s) => s,
        None if allow_unverified => {
            println!(
                "{} Warning: No SHA256 checksum in repository metadata for '{}'; proceeding (--allow-unverified).",
                "::".yellow(),
                pkg
            );
            ""
        }
        None => {
            return Err(format!(
                "Security violation: Repository metadata for '{}' does not provide a SHA256 checksum.\n  \
                Fail-closed policy rejects unverified package downloads.\n  \
                To bypass integrity verification at your own risk, re-run with --allow-unverified.",
                pkg
            ));
        }
    };

    if candidate_urls.is_empty() {
        return Err(format!("No download URLs configured for package '{}'", pkg));
    }

    // 3. Download with automatic mirror failover and SHA256 integrity verification
    let mut last_err = String::new();
    for (i, url) in candidate_urls.iter().enumerate() {
        if i > 0 {
            println!(
                "{} Retrying download from backup mirror {}...",
                "::".yellow(),
                url.dimmed()
            );
        } else {
            println!("{} Fetching '{}' from {}...", "::".cyan(), pkg, url.dimmed());
        }

        let ext = if url.ends_with(".pkg.tar.xz") {
            "pkg.tar.xz"
        } else {
            "pkg.tar.zst"
        };

        let temp_download = create_secure_temp_file(
            &env::temp_dir(),
            &format!("pacpin-download-{}", pkg),
            ext,
        )?;
        guard.downloaded_tarballs.push(temp_download.clone());

        // -f (--fail) ensures curl immediately fails on HTTP 4xx/5xx rather than writing HTML error pages
        let curl_status = Command::new("curl")
            .args(["-sSLf", "-o", temp_download.to_str().unwrap(), url])
            .status();

        match curl_status {
            Ok(s) if s.success() => {
                if !expected.is_empty() {
                    let out = Command::new("sha256sum").arg(&temp_download).output();
                    match out {
                        Ok(o) if o.status.success() => {
                            let stdout = String::from_utf8_lossy(&o.stdout);
                            let computed = stdout.split_whitespace().next().unwrap_or("");
                            if computed.eq_ignore_ascii_case(expected) {
                                return Ok(temp_download);
                            } else {
                                last_err = format!(
                                    "SHA256 checksum mismatch (expected: {}, computed: {})",
                                    expected, computed
                                );
                                let _ = fs::remove_file(&temp_download);
                                println!(
                                    "{} Warning: Mirror '{}' returned invalid checksum. Trying next mirror...",
                                    "::".yellow(),
                                    url
                                );
                            }
                        }
                        _ => {
                            last_err = "Failed to run sha256sum".to_string();
                            let _ = fs::remove_file(&temp_download);
                        }
                    }
                } else {
                    return Ok(temp_download);
                }
            }
            Ok(s) => {
                last_err = format!("curl failed with HTTP error or status {:?}", s.code());
                let _ = fs::remove_file(&temp_download);
                println!(
                    "{} Warning: Mirror '{}' failed (HTTP error). Trying next mirror...",
                    "::".yellow(),
                    url
                );
            }
            Err(e) => {
                last_err = format!("Failed to execute curl: {}", e);
                let _ = fs::remove_file(&temp_download);
            }
        }
    }

    Err(format!(
        "All configured mirrors failed to download verified package '{}': {}",
        pkg, last_err
    ))
}

fn find_executable(sandbox_dir: &Path, pkg: &str, requested_bin: Option<&str>) -> Result<PathBuf, String> {
    let usr_bin = sandbox_dir.join("usr").join("bin");
    let usr_sbin = sandbox_dir.join("usr").join("sbin");

    if let Some(bin_name) = requested_bin {
        let p1 = usr_bin.join(bin_name);
        if is_executable_file(&p1) {
            return Ok(p1);
        }
        let p2 = usr_sbin.join(bin_name);
        if is_executable_file(&p2) {
            return Ok(p2);
        }
        return Err(format!(
            "Requested binary '{}' not found in package usr/bin or usr/sbin.",
            bin_name
        ));
    }

    // 1. Direct match: usr/bin/<pkg>
    let direct = usr_bin.join(pkg);
    if is_executable_file(&direct) {
        return Ok(direct);
    }
    let direct_sbin = usr_sbin.join(pkg);
    if is_executable_file(&direct_sbin) {
        return Ok(direct_sbin);
    }

    // 2. Scan all executables in usr/bin and usr/sbin
    let mut executables: Vec<PathBuf> = Vec::new();
    for dir in &[&usr_bin, &usr_sbin] {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if is_executable_file(&p) {
                    executables.push(p);
                }
            }
        }
    }
    executables.sort();
    executables.dedup();

    if executables.is_empty() {
        return Err(format!(
            "No executable found for package '{}' in sandbox (usr/bin or usr/sbin).",
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

fn is_safe_flatpak_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains(';')
        && !id.contains('&')
        && !id.contains('|')
        && !id.contains('`')
        && !id.contains('$')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

pub fn is_executable_file(path: &Path) -> bool {
    crate::utils::is_executable_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_target() {
        assert_eq!(parse_target("flatpak/org.gnome.Calculator"), (Some("flatpak"), "org.gnome.Calculator"));
        assert_eq!(parse_target("flatpak:org.gnome.Calculator"), (Some("flatpak"), "org.gnome.Calculator"));
        assert_eq!(parse_target("nix/fastfetch"), (Some("nix"), "fastfetch"));
        assert_eq!(parse_target("nix:fastfetch"), (Some("nix"), "fastfetch"));
        assert_eq!(parse_target("nixpkgs#ripgrep"), (Some("nix"), "ripgrep"));
        assert_eq!(parse_target("pipx/cowsay"), (Some("pipx"), "cowsay"));
        assert_eq!(parse_target("pipx:cowsay"), (Some("pipx"), "cowsay"));
        assert_eq!(parse_target("extra/tree"), (Some("extra"), "tree"));
        assert_eq!(parse_target("tree"), (None, "tree"));
    }

    #[test]
    fn test_is_safe_flatpak_id() {
        assert!(is_safe_flatpak_id("org.gnome.Calculator"));
        assert!(is_safe_flatpak_id("com.spotify.Client"));
        assert!(is_safe_flatpak_id("com.github.tchx84.Flatseal"));
        assert!(!is_safe_flatpak_id("org.gnome/calc"));
        assert!(!is_safe_flatpak_id("org.gnome;rm -rf"));
    }

    #[test]
    fn test_is_executable_file() {
        let sh_path = Path::new("/bin/sh");
        if sh_path.exists() {
            assert!(is_executable_file(sh_path));
        }
        let non_exec = Path::new("/etc/passwd");
        if non_exec.exists() {
            assert!(!is_executable_file(non_exec));
        }
    }

    #[test]
    fn test_is_safe_tar_entry() {
        assert!(is_safe_tar_entry(".PKGINFO"));
        assert!(is_safe_tar_entry(".BUILDINFO"));
        assert!(is_safe_tar_entry("usr/bin/jq"));
        assert!(is_safe_tar_entry("./usr/bin/jq"));
        assert!(is_safe_tar_entry("usr/share/man/man1/jq.1.gz"));
        assert!(is_safe_tar_entry(".hidden/file"));
        assert!(is_safe_tar_entry(""));

        // Unsafe entries
        assert!(!is_safe_tar_entry("/etc/shadow"));
        assert!(!is_safe_tar_entry("/usr/bin/jq"));
        assert!(!is_safe_tar_entry("../../../etc/shadow"));
        assert!(!is_safe_tar_entry("usr/bin/../../etc/passwd"));
        assert!(!is_safe_tar_entry("usr/bin/.."));
        assert!(!is_safe_tar_entry(".."));
        assert!(!is_safe_tar_entry("\\etc\\shadow"));
        assert!(!is_safe_tar_entry("..\\..\\windows\\system32"));
    }

    #[test]
    fn test_is_safe_tar_listing_line() {
        // Safe regular file
        let regular = "-rwxr-xr-x 0/0 12345 2026-09-01 12:00 usr/bin/jq";
        let res = is_safe_tar_listing_line(regular).unwrap();
        assert_eq!(res.0, "usr/bin/jq");
        assert_eq!(res.1, 12345);

        // Safe relative symlink
        let symlink = "lrwxrwxrwx 0/0 0 2026-09-01 12:00 usr/bin/jq-sym -> jq";
        let res = is_safe_tar_listing_line(symlink).unwrap();
        assert_eq!(res.0, "usr/bin/jq-sym");

        // Safe relative hardlink
        let hardlink = "hrwxr-xr-x 0/0 0 2026-09-01 12:00 usr/bin/jq-link link to usr/bin/jq";
        let res = is_safe_tar_listing_line(hardlink).unwrap();
        assert_eq!(res.0, "usr/bin/jq-link");

        // Unsafe absolute symlink
        let bad_symlink = "lrwxrwxrwx 0/0 0 2026-09-01 12:00 usr/bin/bad -> /etc/shadow";
        assert!(is_safe_tar_listing_line(bad_symlink).is_err());

        // Unsafe path traversal symlink (exceeds root)
        let escaping_symlink = "lrwxrwxrwx 0/0 0 2026-09-01 12:00 usr/bin/bad -> ../../../etc/passwd";
        assert!(is_safe_tar_listing_line(escaping_symlink).is_err());

        // Safe deep internal relative symlink (e.g. from official rust package)
        let deep_internal_symlink = "lrwxrwxrwx 0/0 0 2026-09-01 12:00 usr/lib/rustlib/x86_64-unknown-linux-gnu/bin/gcc-ld/ld.lld -> ../../../../../bin/ld.lld";
        assert!(is_safe_tar_listing_line(deep_internal_symlink).is_ok());

        // Unsafe escaping symlink from deep directory
        let deep_escaping_symlink = "lrwxrwxrwx 0/0 0 2026-09-01 12:00 usr/lib/rustlib/x86_64-unknown-linux-gnu/bin/gcc-ld/ld.lld -> ../../../../../../../etc/shadow";
        assert!(is_safe_tar_listing_line(deep_escaping_symlink).is_err());

        // Unsafe hardlink
        let bad_hardlink = "hrwxr-xr-x 0/0 0 2026-09-01 12:00 usr/bin/bad link to /etc/shadow";
        assert!(is_safe_tar_listing_line(bad_hardlink).is_err());
    }

    #[test]
    fn test_try_options_default() {
        let opts = TryOptions::default();
        assert!(!opts.no_sandbox);
        assert!(!opts.share_net);
        assert!(!opts.rw_cwd);
        assert!(!opts.gui);
        assert!(!opts.audio);
        assert!(opts.bin.is_none());
        assert!(!opts.allow_unverified);
        assert!(!opts.is_run_mode);
    }

    #[test]
    fn test_try_options_for_run() {
        let opts = TryOptions::for_run();
        assert!(opts.no_sandbox);
        assert!(opts.share_net);
        assert!(opts.rw_cwd);
        assert!(!opts.gui);
        assert!(!opts.audio);
        assert!(opts.bin.is_none());
        assert!(!opts.allow_unverified);
        assert!(opts.is_run_mode);
    }
}
