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
    downloaded_tarball: Option<PathBuf>,
}

impl Drop for SandboxGuard {
    fn drop(&mut self) {
        if self.sandbox_dir.exists() {
            let _ = fs::remove_dir_all(&self.sandbox_dir);
        }
        if let Some(ref tarball) = self.downloaded_tarball {
            if tarball.exists() {
                let _ = fs::remove_file(tarball);
            }
        }
    }
}

pub fn cmd_try(pkg: &str, args: &[String]) {
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
        downloaded_tarball: None,
    };

    // 1. Locate or download package archive
    let tarball_path = match find_or_download_package(pkg, &mut guard) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("{}", format!("Error: {}", err).red().bold());
            exit(1);
        }
    };

    // 2. Extract into isolated sandbox
    if let Err(e) = fs::create_dir_all(&sandbox_dir) {
        eprintln!("{}", format!("Error creating sandbox directory: {}", e).red().bold());
        exit(1);
    }

    println!("{} Extracting '{}' to ephemeral sandbox...", "::".cyan(), pkg);
    let extract_status = Command::new("tar")
        .args(["-xf", tarball_path.to_str().unwrap(), "-C", sandbox_dir.to_str().unwrap()])
        .status();

    match extract_status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("{}", format!("Extraction failed with exit code {}.", s.code().unwrap_or(1)).red());
            exit(s.code().unwrap_or(1));
        }
        Err(e) => {
            eprintln!("{}", format!("Failed to execute tar: {}", e).red());
            exit(1);
        }
    }

    // 3. Find executable binary
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

    // 4. Construct isolated environment
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

    // Guard will automatically clean up sandbox_dir upon leaving scope
    drop(guard);

    match child_status {
        Ok(s) => exit(s.code().unwrap_or(0)),
        Err(e) => {
            eprintln!("{}", format!("Execution failed: {}", e).red());
            exit(1);
        }
    }
}

fn find_or_download_package(pkg: &str, guard: &mut SandboxGuard) -> Result<PathBuf, String> {
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
                // Sort by modification time or name, take latest
                matching.sort_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok());
                let found = matching.last().unwrap().clone();
                println!("{} Found '{}' in local pacman cache.", "::".cyan(), pkg);
                return Ok(found);
            }
        }
    }

    // 2. Query pacman for download URL via `pacman -Sp <pkg>`
    println!("{} Resolving download URL for '{}' (pacman -Sp)...", "::".cyan(), pkg);
    let output = Command::new("pacman")
        .args(["-Sp", pkg])
        .output()
        .map_err(|e| format!("Failed to run pacman -Sp: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "Package '{}' could not be resolved from configured repositories.",
            pkg
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let url = stdout
        .lines()
        .find(|l| l.starts_with("http://") || l.starts_with("https://") || l.starts_with("file://") || l.starts_with("ftp://"))
        .ok_or_else(|| format!("No download URL returned by pacman for '{}'.", pkg))?
        .trim();

    // 3. Download to temporary file
    let ext = if url.ends_with(".pkg.tar.xz") {
        "pkg.tar.xz"
    } else {
        "pkg.tar.zst"
    };

    let temp_download = env::temp_dir().join(format!("pacpin-download-{}-{}.{}", pkg, std::process::id(), ext));
    guard.downloaded_tarball = Some(temp_download.clone());

    println!("{} Fetching '{}' from {}...", "::".cyan(), pkg, url.dimmed());
    let curl_status = Command::new("curl")
        .args(["-sSL", "-o", temp_download.to_str().unwrap(), url])
        .status()
        .map_err(|e| format!("Failed to execute curl: {}", e))?;

    if !curl_status.success() {
        return Err(format!("curl download failed with exit code {:?}", curl_status.code()));
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
