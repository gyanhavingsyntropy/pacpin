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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static CLEANUP_TARGETS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static ACTIVE_CHILDREN: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static TERMINATING: AtomicBool = AtomicBool::new(false);

pub fn is_terminating() -> bool {
    TERMINATING.load(Ordering::SeqCst)
}

pub fn run_tracked_command(command: &mut Command) -> io::Result<std::process::ExitStatus> {
    if is_terminating() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "pacpin is terminating"));
    }
    let mut child = command.spawn()?;
    let pid = child.id();
    if let Ok(mut lock) = ACTIVE_CHILDREN.lock() {
        lock.push(pid);
    }
    if is_terminating() {
        let _ = child.kill();
    }
    let status = child.wait();
    if let Ok(mut lock) = ACTIVE_CHILDREN.lock() {
        lock.retain(|&active| active != pid);
    }
    if is_terminating() {
        Err(io::Error::new(io::ErrorKind::Interrupted, "pacpin is terminating"))
    } else {
        status
    }
}

fn kill_active_children() {
    if let Ok(mut lock) = ACTIVE_CHILDREN.lock() {
        for pid in lock.drain(..) {
            #[cfg(unix)]
            unsafe { libc::kill(pid as i32, libc::SIGKILL); }
        }
    }
}

pub fn register_cleanup_target(path: PathBuf) {
    if is_terminating() {
        remove_cleanup_path(&path);
        return;
    }
    if let Ok(mut lock) = CLEANUP_TARGETS.lock() {
        if is_terminating() {
            drop(lock);
            remove_cleanup_path(&path);
        } else if !lock.contains(&path) {
            lock.push(path);
        }
    }
}

fn remove_cleanup_path(path: &Path) {
    if path.is_dir() {
        let _ = fs::remove_dir_all(path);
    } else if path.is_file() {
        let _ = fs::remove_file(path);
    }
}

pub fn unregister_cleanup_target(path: &Path) {
    if let Ok(mut lock) = CLEANUP_TARGETS.lock() {
        lock.retain(|p| p != path);
    }
}

pub fn cleanup_active_targets() {
    if let Ok(mut lock) = CLEANUP_TARGETS.lock() {
        for path in lock.drain(..) {
            remove_cleanup_path(&path);
        }
    }
}

pub fn clean_stale_sandboxes() {
    if let Ok(xdg_runtime) = env::var("XDG_RUNTIME_DIR") {
        for sub in &["run", "try"] {
            let dir = PathBuf::from(&xdg_runtime).join("pacpin").join(sub);
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() && should_clean_stale_sandbox(&p) {
                        let _ = fs::remove_dir_all(&p);
                    }
                }
            }
        }
    }

    // Also clean any fallback temp directories created in env::temp_dir()
    if let Ok(entries) = fs::read_dir(env::temp_dir()) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if (name.starts_with("pacpin-run-") || name.starts_with("pacpin-try-"))
                    && p.is_dir()
                    && should_clean_stale_sandbox(&p)
                {
                    let _ = fs::remove_dir_all(&p);
                }
            }
        }
    }
}

fn should_clean_stale_sandbox(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !name.starts_with("pacpin-run-") && !name.starts_with("pacpin-try-") {
        return false;
    }
    if let Some((_, suffix)) = name.rsplit_once("-pid") {
        if let Some(pid) = suffix.split('-').next().and_then(|s| s.parse::<i32>().ok()) {
            if pid > 0 {
                #[cfg(unix)]
                return unsafe { libc::kill(pid, 0) != 0 }
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            }
        }
    }
    // Older pacpin directories have no PID. Leave recent ones alone because they
    // may still belong to a live process.
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age >= std::time::Duration::from_secs(24 * 60 * 60))
}

pub fn init_signal_handlers() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = ctrlc::set_handler(move || {
            TERMINATING.store(true, Ordering::SeqCst);
            kill_active_children();
            cleanup_active_targets();
            std::process::exit(130);
        });
    });
}

struct SandboxGuard {
    sandbox_dir: PathBuf,
    downloaded_tarballs: Vec<PathBuf>,
}

impl SandboxGuard {
    pub fn new(sandbox_dir: PathBuf) -> Self {
        register_cleanup_target(sandbox_dir.clone());
        Self {
            sandbox_dir,
            downloaded_tarballs: Vec::new(),
        }
    }

    pub fn register_tarball(&mut self, tarball: &Path) {
        let p = tarball.to_path_buf();
        if self.downloaded_tarballs.contains(&p) {
            register_cleanup_target(p);
        }
    }
}

impl Drop for SandboxGuard {
    fn drop(&mut self) {
        unregister_cleanup_target(&self.sandbox_dir);
        if self.sandbox_dir.exists() {
            let _ = fs::remove_dir_all(&self.sandbox_dir);
        }
        for tarball in &self.downloaded_tarballs {
            unregister_cleanup_target(tarball);
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
    pub noconfirm: bool,
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
            noconfirm: false,
        }
    }
}

pub fn cmd_try(target: &str, args: &[String], options: &TryOptions) {
    let (prefix, pkg) = parse_target(target);

    let res = match prefix {
        Some("flatpak") => try_flatpak(pkg, args),
        provider if host_only_provider(provider) && !options.no_sandbox => Err(
            "Nix and Pipx run their programs on the host. Use 'pin run' or pass --no-sandbox to opt in to host execution."
                .to_string(),
        ),
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

fn host_only_provider(prefix: Option<&str>) -> bool {
    matches!(prefix, Some("nix" | "nixpkgs" | "pipx"))
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
                // Flatpak app data may predate this invocation. Never delete it here.
                println!("✔ Ephemeral Flatpak '{}' uninstalled.", self.app_id.green());
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
    let base_dir = env::var_os("XDG_RUNTIME_DIR")
        .and_then(|runtime| private_runtime_subdir(Path::new(&runtime), sub).ok())
        .unwrap_or_else(env::temp_dir);

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
        let target = base_dir.join(format!("{}-pid{}-{}", prefix, std::process::id(), hex));

        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);

        if builder.create(&target).is_ok() {
            return Ok(target);
        }
    }
    Err("Failed to create secure sandbox directory with exclusive permissions".to_string())
}

fn private_runtime_subdir(runtime: &Path, sub: &str) -> Result<PathBuf, String> {
    #[cfg(unix)]
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    #[cfg(unix)]
    fn private_owned_dir(path: &Path) -> bool {
        let Ok(meta) = fs::symlink_metadata(path) else { return false; };
        meta.is_dir()
            && !meta.file_type().is_symlink()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.permissions().mode() & 0o077 == 0
    }

    #[cfg(not(unix))]
    fn private_owned_dir(path: &Path) -> bool {
        path.is_dir()
    }

    if !runtime.is_absolute() || !private_owned_dir(runtime) {
        return Err("XDG_RUNTIME_DIR must be a private directory owned by the current user".into());
    }
    let parent = runtime.join("pacpin");
    let child = parent.join(sub);
    for dir in [&parent, &child] {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        match builder.create(dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(format!("Cannot create private runtime directory: {err}")),
        }
        if !private_owned_dir(dir) {
            return Err(format!("Unsafe runtime directory: {}", dir.display()));
        }
    }
    Ok(child)
}

fn safe_wayland_socket(runtime: &Path, display: &str) -> Option<PathBuf> {
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

fn should_bind_cwd(cwd: &Path, home: &Path) -> bool {
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
pub use crate::download::{
    compute_sha256, create_secure_temp_file, extract_archive, is_safe_link_destination,
    is_safe_tar_entry, is_safe_tar_listing_line, MAX_ARCHIVE_ENTRIES, MAX_UNPACKED_BYTES,
};

fn add_host_runtime_mounts(command: &mut Command) {
    for dir in &["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc"] {
        let path = Path::new(dir);
        if path.exists() {
            command.arg("--ro-bind").arg(path).arg(path);
        }
    }
}

fn supports_ro_overlay() -> bool {
    let mut probe = Command::new("bwrap");
    probe.args(["--unshare-all", "--die-with-parent", "--new-session"]);
    add_host_runtime_mounts(&mut probe);
    probe.args([
        "--overlay-src", "/usr/share", "--overlay-src", "/etc",
        "--ro-overlay", "/usr/share", "--proc", "/proc", "--dev", "/dev",
        "--tmpfs", "/tmp", "--", "/usr/bin/true",
    ]);
    probe.output().is_ok_and(|output| output.status.success())
}

fn add_package_data_mounts(
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
    let test_res = probe
        .args(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--", "/usr/bin/true"])
        .output();

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

fn try_pacman(repo: Option<&str>, pkg: &str, args: &[String], options: &TryOptions) -> Result<i32, String> {
    if pkg.is_empty() || !crate::journal::TransactionJournal::is_safe_pkg_name(pkg) {
        return Err(format!("Invalid package name '{}'.", pkg));
    }

    init_signal_handlers();

    if !options.no_sandbox {
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
                        sudo pacman -S bubblewrap\n  \
                        To bypass sandboxing and run directly on your host (UNSAFE), pass --no-sandbox."
                            .to_string(),
                    );
                }
            } else {
                return Err(err_reason);
            }
        }
    }

    let prefix = if options.is_run_mode { "pacpin-run" } else { "pacpin-try" };
    let sandbox_dir = create_secure_temp_dir(&format!("{}-{}", prefix, pkg))?;

    let mut guard = SandboxGuard::new(sandbox_dir.clone());

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

    // Downloads are independent; extraction stays ordered so dependencies cannot
    // race while writing into the same root.
    let archives = download_targets_parallel(&resolved, options.allow_unverified, &mut guard, |target, allow_unverified, downloaded| {
        let urls = if target.candidate_urls.is_empty() {
            vec![target.url.clone()]
        } else {
            target.candidate_urls.clone()
        };
        crate::download::fetch_package_archive(
            &target.name,
            &urls,
            target.sha256.as_deref(),
            allow_unverified,
            downloaded,
        )
    })?;
    for tarball_path in archives {
        crate::download::extract_archive(&tarball_path, &sandbox_dir)?;
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
        bwrap_cmd.status()
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

fn download_targets_parallel<F>(
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

fn find_executable(sandbox_dir: &Path, pkg: &str, requested_bin: Option<&str>) -> Result<PathBuf, String> {
    let usr_bin = sandbox_dir.join("usr").join("bin");
    let usr_sbin = sandbox_dir.join("usr").join("sbin");

    if let Some(bin_name) = requested_bin {
        if bin_name.is_empty()
            || bin_name == "."
            || bin_name == ".."
            || bin_name.starts_with('-')
            || bin_name.contains(['/', '\\', '\0'])
        {
            return Err("--bin must name a single executable within the package".to_string());
        }
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
    fn test_host_only_providers_require_explicit_opt_in() {
        for target in ["nix/fastfetch", "nixpkgs#ripgrep", "pipx/cowsay"] {
            assert!(host_only_provider(parse_target(target).0));
        }
        assert!(!host_only_provider(parse_target("flatpak/org.gnome.Calculator").0));
        assert!(!host_only_provider(parse_target("extra/jq").0));
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
        assert!(!opts.noconfirm);
    }

    #[test]
    fn test_cleanup_registry() {
        let temp_dir = std::env::temp_dir().join(format!("pacpin-test-cleanup-reg-{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_dir);
        assert!(temp_dir.exists());

        register_cleanup_target(temp_dir.clone());
        cleanup_active_targets();

        assert!(!temp_dir.exists());
    }

    #[test]
    fn test_guard_preserves_cached_archive() {
        let sandbox_dir = create_secure_temp_dir("pacpin-try-cache-test").unwrap();
        let cached = crate::download::create_secure_temp_file(&env::temp_dir(), "pacpin-cache-test", "pkg.tar.zst").unwrap();
        let mut guard = SandboxGuard::new(sandbox_dir.clone());
        guard.register_tarball(&cached);
        drop(guard);
        assert!(cached.exists());
        assert!(!sandbox_dir.exists());
        fs::remove_file(cached).unwrap();
    }

    #[test]
    fn test_stale_cleanup_keeps_live_sandbox() {
        let live = create_secure_temp_dir("pacpin-run-live-test").unwrap();
        assert!(!should_clean_stale_sandbox(&live));
        fs::remove_dir_all(live).unwrap();

        let dead = env::temp_dir().join(format!("pacpin-run-dead-test-pid{}-123", i32::MAX));
        fs::create_dir(&dead).unwrap();
        assert!(should_clean_stale_sandbox(&dead));
        fs::remove_dir_all(dead).unwrap();
    }

    #[test]
    fn test_cwd_mount_policy_protects_home_and_system() {
        let home = env::temp_dir().join(format!("pacpin-home-policy-{}", std::process::id()));
        let nested = home.join("private");
        fs::create_dir_all(&nested).unwrap();
        assert!(!should_bind_cwd(Path::new("/"), &home));
        assert!(!should_bind_cwd(Path::new("/etc"), &home));
        assert!(!should_bind_cwd(&home, &home));
        assert!(!should_bind_cwd(&nested, &home));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn test_package_data_mounts_preserve_absolute_lookup_paths() {
        let root = create_secure_temp_dir("pacpin-data-mount-test").unwrap();
        fs::create_dir_all(root.join("usr/share/figlet")).unwrap();
        fs::create_dir_all(root.join("etc/figlet")).unwrap();
        let mut command = Command::new("bwrap");
        add_package_data_mounts(&mut command, &root, false).unwrap();
        let args: Vec<_> = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert!(args.windows(3).any(|w| w[0] == "--ro-bind" && w[2] == "/usr/share"));
        assert!(!args.windows(3).any(|w| w[0] == "--ro-bind" && w[2] == "/etc/figlet"));
        let mut overlay = Command::new("bwrap");
        add_package_data_mounts(&mut overlay, &root, true).unwrap();
        let overlay_args: Vec<_> = overlay.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert!(overlay_args.windows(2).any(|w| w == ["--ro-overlay", "/usr/share"]));
        assert!(overlay_args.windows(2).any(|w| w == ["--ro-overlay", "/etc"]));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_runtime_directory_rejects_symlink_and_public_permissions() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let runtime = create_secure_temp_dir("pacpin-runtime-test").unwrap();
        let private = private_runtime_subdir(&runtime, "run").unwrap();
        assert!(private.is_dir());

        let link = runtime.with_extension("link");
        symlink(&runtime, &link).unwrap();
        assert!(private_runtime_subdir(&link, "run").is_err());
        fs::remove_file(link).unwrap();

        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_runtime_subdir(&runtime, "run").is_err());
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(runtime).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_wayland_socket_must_be_real_socket_with_safe_basename() {
        use std::os::unix::net::UnixListener;
        use std::os::unix::fs::symlink;
        let runtime = create_secure_temp_dir("pacpin-wayland-test").unwrap();
        let socket = runtime.join("wayland-0");
        let _listener = UnixListener::bind(&socket).unwrap();
        assert_eq!(safe_wayland_socket(&runtime, "wayland-0"), Some(socket.clone()));
        for display in ["", ".", "..", "../passwd", "/etc/passwd", "nested/wayland-0"] {
            assert_eq!(safe_wayland_socket(&runtime, display), None);
        }
        symlink(&socket, runtime.join("wayland-link")).unwrap();
        assert_eq!(safe_wayland_socket(&runtime, "wayland-link"), None);
        fs::remove_dir_all(runtime).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_active_download_children_are_killed_before_cleanup() {
        let worker = std::thread::spawn(|| {
            run_tracked_command(Command::new("sleep").arg("30")).unwrap()
        });
        for _ in 0..100 {
            if ACTIVE_CHILDREN.lock().unwrap().len() == 1 { break; }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(ACTIVE_CHILDREN.lock().unwrap().len(), 1);
        kill_active_children();
        assert!(!worker.join().unwrap().success());
        assert!(ACTIVE_CHILDREN.lock().unwrap().is_empty());
    }

    #[test]
    fn test_downloads_overlap_but_preserve_extraction_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let targets: Vec<crate::db::DownloadTarget> = (0..3)
            .map(|i| crate::db::DownloadTarget {
                name: format!("pkg-{}", i), url: String::new(),
                candidate_urls: Vec::new(), sha256: None,
            })
            .collect();
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let mut guard = SandboxGuard::new(create_secure_temp_dir("pacpin-run-parallel-test").unwrap());
        let archives = download_targets_parallel(&targets, false, &mut guard, |target, _, _| {
            let count = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(count, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(40));
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(PathBuf::from(&target.name))
        }).unwrap();
        assert_eq!(archives, vec![PathBuf::from("pkg-0"), PathBuf::from("pkg-1"), PathBuf::from("pkg-2")]);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_parallel_download_error_cleans_owned_files() {
        let sandbox = create_secure_temp_dir("pacpin-run-download-error-test").unwrap();
        let mut guard = SandboxGuard::new(sandbox.clone());
        let target = crate::db::DownloadTarget {
            name: "broken".into(), url: String::new(), candidate_urls: Vec::new(), sha256: None,
        };
        let mut downloaded_path = None;
        let result = download_targets_parallel(&[target], false, &mut guard, |_, _, downloaded| {
            let temp = crate::download::create_secure_temp_file(&env::temp_dir(), "pacpin-download-error-test", "pkg.tar.zst")?;
            downloaded.push(temp.clone());
            Err(format!("test failure at {}", temp.display()))
        });
        if let Err(message) = &result {
            downloaded_path = message.strip_prefix("test failure at ").map(PathBuf::from);
        }
        assert!(result.is_err());
        let downloaded_path = downloaded_path.unwrap();
        assert!(downloaded_path.exists());
        drop(guard);
        assert!(!downloaded_path.exists());
        assert!(!sandbox.exists());
    }

    #[test]
    fn test_requested_binary_cannot_escape_package_root() {
        let root = create_secure_temp_dir("pacpin-try-bin-test").unwrap();
        for name in ["/bin/sh", "../../bin/sh", "../sh", "-sh", "\\bin\\sh"] {
            assert!(find_executable(&root, "test", Some(name))
                .unwrap_err()
                .contains("single executable"));
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_check_bwrap_capability_runs() {
        // Must execute cleanly without panic
        let _ = check_bwrap_capability();
    }

    #[test]
    fn test_cwd_is_home_logic() {
        let temp_home = std::env::temp_dir().join(format!("pacpin-test-home-{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_home);
        let sub_dir = temp_home.join("workspace");
        let _ = fs::create_dir_all(&sub_dir);

        let is_home = temp_home.canonicalize().ok() == temp_home.canonicalize().ok();
        assert!(is_home);

        let sub_is_home = sub_dir.canonicalize().ok() == temp_home.canonicalize().ok();
        assert!(!sub_is_home);

        let _ = fs::remove_dir_all(&temp_home);
    }
}
