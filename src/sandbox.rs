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
use std::os::unix::fs::PermissionsExt;
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

pub fn cmd_try(target: &str, args: &[String]) {
    let (prefix, pkg) = parse_target(target);

    match prefix {
        Some("flatpak") => try_flatpak(pkg, args),
        Some("nix") | Some("nixpkgs") => try_nix(pkg, args),
        Some("pipx") => try_pipx(pkg, args),
        Some(repo) => try_pacman(Some(repo), pkg, args),
        None => try_pacman(None, pkg, args),
    }
}

fn try_flatpak(app_id: &str, args: &[String]) {
    if !is_safe_flatpak_id(app_id) {
        eprintln!(
            "{}",
            format!("Error: Invalid Flatpak Application ID '{}'.", app_id).red().bold()
        );
        eprintln!("Flatpak IDs follow reverse-DNS notation, e.g. 'org.gnome.Calculator' or 'com.spotify.Client'.");
        exit(1);
    }

    if !crate::is_command_available("flatpak") {
        eprintln!("{}", "Error: 'flatpak' command not found on this system.".red().bold());
        eprintln!("Install flatpak or enable Flatpak integration in 'pacpin init'.");
        exit(1);
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
            .status();

        match install_status {
            Ok(s) if s.success() => {
                guard.needs_cleanup = true;
            }
            Ok(s) => {
                eprintln!(
                    "{}",
                    format!("Flatpak installation failed with exit code {}.", s.code().unwrap_or(1)).red()
                );
                exit(s.code().unwrap_or(1));
            }
            Err(e) => {
                eprintln!("{}", format!("Failed to run flatpak install: {}", e).red());
                exit(1);
            }
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

    let child_status = Command::new("flatpak").args(&run_args).status();

    drop(guard);

    match child_status {
        Ok(s) => exit(s.code().unwrap_or(0)),
        Err(e) => {
            eprintln!("{}", format!("Failed to execute flatpak run: {}", e).red());
            exit(1);
        }
    }
}

fn try_nix(pkg: &str, args: &[String]) {
    if !crate::journal::TransactionJournal::is_safe_pkg_name(pkg) {
        eprintln!(
            "{}",
            format!("Error: Invalid Nix package name '{}'.", pkg).red().bold()
        );
        exit(1);
    }

    if !crate::is_command_available("nix") {
        eprintln!("{}", "Error: 'nix' command not found on this system.".red().bold());
        exit(1);
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

    let status = Command::new("nix").args(&nix_args).status();

    match status {
        Ok(s) if s.success() => exit(0),
        Ok(s) => {
            // Fallback: nix-shell -p <pkg> --run "<pkg> [args...]"
            println!(
                "{} 'nix run' exited with {}. Trying 'nix-shell' fallback...",
                "::".yellow(),
                s.code().unwrap_or(1)
            );
            let mut cmd_parts = vec![crate::utils::shell_quote(pkg)];
            cmd_parts.extend(args.iter().map(|s| crate::utils::shell_quote(s)));
            let shell_cmd = cmd_parts.join(" ");
            let shell_status = Command::new("nix-shell")
                .args(["-p", pkg, "--run", &shell_cmd])
                .status();
            match shell_status {
                Ok(ss) => exit(ss.code().unwrap_or(0)),
                Err(e) => {
                    eprintln!("{}", format!("Failed to run nix-shell: {}", e).red());
                    exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("{}", format!("Failed to execute nix: {}", e).red());
            exit(1);
        }
    }
}

fn try_pipx(pkg: &str, args: &[String]) {
    if !crate::journal::TransactionJournal::is_safe_pkg_name(pkg) {
        eprintln!(
            "{}",
            format!("Error: Invalid Pipx package name '{}'.", pkg).red().bold()
        );
        exit(1);
    }

    if !crate::is_command_available("pipx") {
        eprintln!("{}", "Error: 'pipx' command not found on this system.".red().bold());
        exit(1);
    }

    println!(
        "{} Running ephemeral Pipx package '{}' (pipx run)...",
        "::".cyan(),
        pkg.bold()
    );

    let mut pipx_args = vec!["run", pkg];
    pipx_args.extend(args.iter().map(|s| s.as_str()));

    let status = Command::new("pipx").args(&pipx_args).status();

    match status {
        Ok(s) => exit(s.code().unwrap_or(0)),
        Err(e) => {
            eprintln!("{}", format!("Failed to execute pipx: {}", e).red());
            exit(1);
        }
    }
}

fn try_pacman(repo: Option<&str>, pkg: &str, args: &[String]) {
    if pkg.is_empty() || !crate::journal::TransactionJournal::is_safe_pkg_name(pkg) {
        eprintln!(
            "{}",
            format!("Error: Invalid package name '{}'.", pkg).red().bold()
        );
        exit(1);
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let sandbox_dir = env::temp_dir().join(format!("pacpin-try-{}-{}-{}", pkg, std::process::id(), timestamp));

    let mut guard = SandboxGuard {
        sandbox_dir: sandbox_dir.clone(),
        downloaded_tarballs: Vec::new(),
    };

    let cfg = crate::config::load_config();
    let manager = match crate::db::AlpmManager::with_repo_order(&cfg.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    println!("{} Resolving download URLs for '{}' via ALPM...", "::".cyan(), pkg);
    let resolved = match manager.resolve_download_urls(repo, pkg) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("{}", format!("Error: {}", err).red().bold());
            exit(1);
        }
    };

    if let Err(e) = fs::create_dir_all(&sandbox_dir) {
        eprintln!("{}", format!("Error creating sandbox directory: {}", e).red().bold());
        exit(1);
    }

    if resolved.len() > 1 {
        println!(
            "{} Found {} package(s) (1 target + {} missing dependencies to sandbox).",
            "::".cyan(),
            resolved.len(),
            resolved.len() - 1
        );
    }

    // 1. Download & Extract all resolved packages (dependencies first, then target)
    for target in &resolved {
        let tarball_path = match get_or_download_package(&target.name, &target.url, target.sha256.as_deref(), &mut guard) {
            Ok(path) => path,
            Err(err) => {
                eprintln!("{}", format!("Error: {}", err).red().bold());
                exit(1);
            }
        };

        let extract_status = Command::new("tar")
            .args(["-xf", tarball_path.to_str().unwrap(), "-C", sandbox_dir.to_str().unwrap()])
            .status();

        match extract_status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!("{}", format!("Extraction of '{}' failed with exit code {}.", target.name, s.code().unwrap_or(1)).red());
                exit(s.code().unwrap_or(1));
            }
            Err(e) => {
                eprintln!("{}", format!("Failed to execute tar on '{}': {}", target.name, e).red());
                exit(1);
            }
        }
    }

    // 2. Find executable binary
    let binary_path = match find_executable(&sandbox_dir, pkg) {
        Some(b) => b,
        None => {
            eprintln!(
                "{}",
                format!("Error: No executable found for package '{}' in sandbox (usr/bin).", pkg).red().bold()
            );
            exit(1);
        }
    };

    // 3. Construct isolated environment
    let usr_bin = sandbox_dir.join("usr").join("bin");
    let usr_sbin = sandbox_dir.join("usr").join("sbin");
    let usr_lib = sandbox_dir.join("usr").join("lib");
    let usr_lib64 = sandbox_dir.join("usr").join("lib64");
    let usr_share = sandbox_dir.join("usr").join("share");

    let current_path = env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}:{}", usr_bin.display(), usr_sbin.display(), current_path);

    let current_ld = env::var("LD_LIBRARY_PATH").unwrap_or_default();
    let new_ld = if current_ld.is_empty() {
        format!("{}:{}", usr_lib.display(), usr_lib64.display())
    } else {
        format!("{}:{}:{}", usr_lib.display(), usr_lib64.display(), current_ld)
    };

    let current_xdg = env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    let new_xdg = format!("{}:{}", usr_share.display(), current_xdg);

    println!(
        "{} Running ephemeral '{}' (sandbox will be destroyed upon exit)...\n",
        "::".cyan(),
        pkg.bold()
    );

    let child_status = Command::new(&binary_path)
        .args(args)
        .env("PATH", new_path)
        .env("LD_LIBRARY_PATH", new_ld)
        .env("XDG_DATA_DIRS", new_xdg)
        .status();

    // Guard will automatically clean up sandbox_dir and downloaded tarballs upon leaving scope
    drop(guard);

    match child_status {
        Ok(s) => exit(s.code().unwrap_or(0)),
        Err(e) => {
            eprintln!("{}", format!("Execution failed: {}", e).red());
            exit(1);
        }
    }
}

fn get_or_download_package(
    pkg: &str,
    url: &str,
    expected_sha: Option<&str>,
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
                println!("{} Found '{}' in local pacman cache.", "::".cyan(), pkg);
                return Ok(found);
            }
        }
    }

    // 2. Download to temporary file
    let ext = if url.ends_with(".pkg.tar.xz") {
        "pkg.tar.xz"
    } else {
        "pkg.tar.zst"
    };

    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    let temp_download = env::temp_dir().join(format!(
        "pacpin-download-{}-{}-{}.{}",
        pkg,
        std::process::id(),
        micros,
        ext
    ));
    guard.downloaded_tarballs.push(temp_download.clone());

    println!("{} Fetching '{}' from {}...", "::".cyan(), pkg, url.dimmed());
    let curl_status = Command::new("curl")
        .args(["-sSL", "-o", temp_download.to_str().unwrap(), url])
        .status()
        .map_err(|e| format!("Failed to execute curl: {}", e))?;

    if !curl_status.success() {
        return Err(format!("curl download failed with exit code {:?}", curl_status.code()));
    }

    // 3. Verify SHA256 integrity against ALPM database metadata
    if let Some(expected) = expected_sha {
        if let Ok(out) = Command::new("sha256sum").arg(&temp_download).output() {
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let computed = stdout.split_whitespace().next().unwrap_or("");
                if !computed.eq_ignore_ascii_case(expected) {
                    let _ = fs::remove_file(&temp_download);
                    return Err(format!(
                        "SHA256 checksum verification failed for '{}'!\n  Expected: {}\n  Computed: {}\nDownloaded package was discarded for security.",
                        pkg, expected, computed
                    ));
                }
            }
        }
    }

    Ok(temp_download)
}

fn find_executable(sandbox_dir: &Path, pkg: &str) -> Option<PathBuf> {
    let usr_bin = sandbox_dir.join("usr").join("bin");

    // 1. Direct match: usr/bin/<pkg>
    let direct = usr_bin.join(pkg);
    if is_executable_file(&direct) {
        return Some(direct);
    }

    // 2. Check all files in usr/bin
    if let Ok(entries) = fs::read_dir(&usr_bin) {
        let mut executables: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_executable_file(p))
            .collect();

        if executables.len() == 1 {
            return Some(executables.remove(0));
        }

        // Look for name containing pkg
        if let Some(pos) = executables
            .iter()
            .position(|p| p.file_name().map(|n| n.to_string_lossy().contains(pkg)).unwrap_or(false))
        {
            return Some(executables.remove(pos));
        }

        if !executables.is_empty() {
            return Some(executables.remove(0));
        }
    }

    // 3. Check usr/sbin
    let usr_sbin = sandbox_dir.join("usr").join("sbin");
    let direct_sbin = usr_sbin.join(pkg);
    if is_executable_file(&direct_sbin) {
        return Some(direct_sbin);
    }

    None
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

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    if let Ok(meta) = fs::metadata(path) {
        let mode = meta.permissions().mode();
        return (mode & 0o111) != 0;
    }
    false
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
}
