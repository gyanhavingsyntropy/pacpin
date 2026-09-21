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

use chrono::Local;
use colored::Colorize;
use glob::glob;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionRecord {
    pub id: usize,
    pub timestamp: String,
    pub action: String,
    pub command: String,
    pub total_packages: usize,
    pub packages: Vec<serde_json::Value>,
}

pub struct TransactionJournal;

impl TransactionJournal {
    pub fn state_dir() -> Result<PathBuf, String> {
        let home = std::env::var("HOME").map_err(|_| "Environment variable HOME is not set".to_string())?;
        Ok(Path::new(&home).join(".local").join("state").join("pacpin"))
    }

    pub fn history_file() -> Result<PathBuf, String> {
        Ok(Self::state_dir()?.join("history.jsonl"))
    }

    pub fn get_cache_dirs() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Ok(output) = Command::new("pacman-conf").arg("CacheDir").output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        dirs.push(PathBuf::from(trimmed));
                    }
                }
            }
        }
        if dirs.is_empty() {
            dirs.push(PathBuf::from("/var/cache/pacman/pkg"));
        }
        dirs
    }

    pub fn is_safe_pkg_name(name: &str) -> bool {
        crate::utils::is_safe_pkg_name(name)
    }

    pub fn is_safe_version(ver: &str) -> bool {
        crate::utils::is_safe_version(ver)
    }

    pub fn find_cached_package(pkg_name: &str, version: &str) -> Option<PathBuf> {
        let ver_no_epoch = version.split(':').last().unwrap_or(version);
        if !Self::is_safe_pkg_name(pkg_name) || !Self::is_safe_version(ver_no_epoch) {
            return None;
        }
        let cache_dirs = Self::get_cache_dirs();

        let versions_to_check = if ver_no_epoch != version && Self::is_safe_version(version) {
            vec![version, ver_no_epoch]
        } else {
            vec![ver_no_epoch]
        };

        for c_dir in &cache_dirs {
            if !c_dir.exists() {
                continue;
            }
            let c_dir_canon = c_dir.canonicalize().unwrap_or_else(|_| c_dir.clone());
            for v in &versions_to_check {
                for ext in &["zst", "xz", "gz"] {
                    let pattern_str = format!(
                        "{}/{}-{}-*.pkg.tar.{}",
                        c_dir.display(),
                        pkg_name,
                        v,
                        ext
                    );
                if let Ok(paths) = glob(&pattern_str) {
                    for entry in paths.flatten() {
                        let name_str = entry.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        if !name_str.ends_with(".sig") {
                            if let Ok(canon) = entry.canonicalize() {
                                if canon.starts_with(&c_dir_canon) {
                                    return Some(entry);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
    }

    pub fn get_last_transaction_id(path: &Path) -> Option<usize> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = fs::File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        if len == 0 {
            return None;
        }
        let read_size = std::cmp::min(len, 8192);
        file.seek(SeekFrom::End(-(read_size as i64))).ok()?;
        let mut buf = vec![0u8; read_size as usize];
        file.read_exact(&mut buf).ok()?;
        let s = String::from_utf8_lossy(&buf);
        for line in s.lines().rev() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                if let Ok(record) = serde_json::from_str::<TransactionRecord>(trimmed) {
                    return Some(record.id);
                }
            }
        }
        None
    }

    pub fn record_transaction(
        action: &str,
        packages: Vec<serde_json::Value>,
        command: &str,
    ) -> Result<usize, std::io::Error> {
        let state_dir = Self::state_dir().map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e))?;
        fs::create_dir_all(&state_dir)?;
        let history_file = Self::history_file().map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e))?;

        let next_id = if history_file.exists() {
            Self::get_last_transaction_id(&history_file).map(|id| id + 1).unwrap_or(1)
        } else {
            1
        };

        let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let record = TransactionRecord {
            id: next_id,
            timestamp: now,
            action: action.to_string(),
            command: command.to_string(),
            total_packages: packages.len(),
            packages,
        };

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&history_file)?;
        let line = serde_json::to_string(&record).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        writeln!(file, "{}", line)?;

        Ok(next_id)
    }

    pub fn list_transactions(limit: usize) -> Vec<TransactionRecord> {
        let history_file = match Self::history_file() {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        if !history_file.exists() {
            return Vec::new();
        }

        let mut txs = Vec::new();
        if let Ok(file) = fs::File::open(&history_file) {
            let reader = BufReader::new(file);
            for line in reader.lines().flatten() {
                if let Ok(record) = serde_json::from_str::<TransactionRecord>(&line) {
                    txs.push(record);
                }
            }
        }

        if txs.len() > limit {
            txs.split_off(txs.len() - limit)
        } else {
            txs
        }
    }

    pub fn get_transaction(tx_id: usize) -> Option<TransactionRecord> {
        let history_file = match Self::history_file() {
            Ok(f) => f,
            Err(_) => return None,
        };
        if !history_file.exists() {
            return None;
        }

        if let Ok(file) = fs::File::open(&history_file) {
            let reader = BufReader::new(file);
            for line in reader.lines().flatten() {
                if let Ok(record) = serde_json::from_str::<TransactionRecord>(&line) {
                    if record.id == tx_id {
                        return Some(record);
                    }
                }
            }
        }
        None
    }

    pub fn rollback(tx_id: Option<usize>, dry_run: bool, allow_partial: bool) {
        let txs = Self::list_transactions(100);
        if txs.is_empty() {
            println!("{}", "No transaction history found in history.jsonl.".yellow());
            return;
        }

        let target_tx = if let Some(id) = tx_id {
            match Self::get_transaction(id) {
                Some(tx) => tx,
                None => {
                    eprintln!("{}", format!("Error: Transaction #{} not found.", id).red());
                    return;
                }
            }
        } else {
            txs.iter()
                .rev()
                .find(|t| t.action == "upgrade" || t.action == "install")
                .cloned()
                .unwrap_or_else(|| txs.last().unwrap().clone())
        };

        let t_id = target_tx.id;
        println!(
            "\n{} ({} — {})",
            format!("Rolling Back Transaction #{}", t_id).bold(),
            target_tx.timestamp,
            target_tx.action
        );

        let mut pkgs_to_restore = Vec::new();
        let mut missing = Vec::new();

        for p in &target_tx.packages {
            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let from_ver = p.get("from_version").and_then(|v| v.as_str());

            if let Some(ver) = from_ver {
                if let Some(path) = Self::find_cached_package(name, ver) {
                    pkgs_to_restore.push((name.to_string(), ver.to_string(), path));
                } else {
                    missing.push((name.to_string(), ver.to_string()));
                }
            }
        }

        if !missing.is_empty() {
            println!(
                "\n{}",
                format!("Warning: {} package archive(s) not found in cache:", missing.len()).yellow()
            );
            for (name, ver) in &missing {
                println!("  • {}", format!("{} {}", name, ver).red());
            }

            if !allow_partial {
                eprintln!(
                    "\n{}",
                    "Error: Rollback aborted to prevent broken dependencies and partial downgrade.".red().bold()
                );
                eprintln!(
                    "  {} package archive(s) are missing from the pacman cache.",
                    missing.len()
                );
                eprintln!("  To force a partial rollback of only available packages, pass --allow-partial.");
                return;
            }
        }

        if pkgs_to_restore.is_empty() {
            eprintln!(
                "\n{}",
                format!("Error: No restorable cached package archives available for Transaction #{}.", t_id).red()
            );
            return;
        }

        println!(
            "\n{}",
            format!("Packages to Restore ({}):", pkgs_to_restore.len()).bold()
        );
        for (name, ver, path) in &pkgs_to_restore {
            let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            println!(
                "  ✔ {:<28} ➔ {} ({})",
                name.cyan(),
                ver.green(),
                fname
            );
        }

        let mut cmd_args: Vec<String> = vec![
            "pacman".to_string(),
            "-U".to_string(),
            "--needed".to_string(),
        ];
        for (_, _, path) in &pkgs_to_restore {
            cmd_args.push(path.display().to_string());
        }

        let full_cmd = format!("sudo {}", cmd_args.join(" "));

        if dry_run {
            println!("\n{}", "Dry-Run: Synthesized Rollback Command:".bold());
            println!("  ➔ {}", full_cmd.cyan());
            return;
        }

        if Path::new("/var/lib/pacman/db.lck").exists() {
            eprintln!("{}", "Error: Pacman database is locked (/var/lib/pacman/db.lck).".red());
            std::process::exit(1);
        }

        use std::io::{self, Write};
        print!("\nProceed with rollback? [y/N]: ");
        let _ = io::stdout().flush();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() || !input.trim().eq_ignore_ascii_case("y") {
            println!("Rollback cancelled.");
            return;
        }

        println!("\n{} {}", ":: Executing rollback:".cyan(), "sudo pacman -U ...".bold());
        let mut child_args = vec!["pacman", "-U", "--needed"];
        let path_strs: Vec<String> = pkgs_to_restore.iter().map(|(_, _, p)| p.display().to_string()).collect();
        for p in &path_strs {
            child_args.push(p);
        }

        let status = Command::new("sudo")
            .args(&child_args)
            .status();

        match status {
            Ok(s) if s.success() => {
                let rollback_pkgs: Vec<serde_json::Value> = pkgs_to_restore
                    .iter()
                    .map(|(name, ver, path)| {
                        serde_json::json!({
                            "name": name,
                            "restored_version": ver,
                            "path": path.display().to_string()
                        })
                    })
                    .collect();
                let _ = Self::record_transaction("rollback", rollback_pkgs, &full_cmd);
                println!(
                    "\n{}",
                    format!("✔ Rollback of Transaction #{} completed successfully.", t_id).green()
                );
            }
            Ok(s) => {
                std::process::exit(s.code().unwrap_or(1));
            }
            Err(e) => {
                eprintln!("{}", format!("Failed to execute sudo pacman: {}", e).red());
                std::process::exit(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safe_pkg_name() {
        assert!(TransactionJournal::is_safe_pkg_name("linux"));
        assert!(TransactionJournal::is_safe_pkg_name("linux-cachyos"));
        assert!(TransactionJournal::is_safe_pkg_name("lib32-glibc"));
        assert!(TransactionJournal::is_safe_pkg_name("python@3.12"));
        assert!(TransactionJournal::is_safe_pkg_name("gcc-libs"));

        assert!(!TransactionJournal::is_safe_pkg_name(""));
        assert!(!TransactionJournal::is_safe_pkg_name("../etc/passwd"));
        assert!(!TransactionJournal::is_safe_pkg_name("foo/bar"));
        assert!(!TransactionJournal::is_safe_pkg_name("foo\\bar"));
        assert!(!TransactionJournal::is_safe_pkg_name("foo;rm"));
        assert!(!TransactionJournal::is_safe_pkg_name("foo*"));
        assert!(!TransactionJournal::is_safe_pkg_name("pkg name"));
    }

    #[test]
    fn test_is_safe_version() {
        assert!(TransactionJournal::is_safe_version("1.2.3"));
        assert!(TransactionJournal::is_safe_version("20260916-1"));
        assert!(TransactionJournal::is_safe_version("1:2.3-1"));
        assert!(TransactionJournal::is_safe_version("2.4.0~rc1"));

        assert!(!TransactionJournal::is_safe_version(""));
        assert!(!TransactionJournal::is_safe_version("../../foo"));
        assert!(!TransactionJournal::is_safe_version("1.0/2"));
        assert!(!TransactionJournal::is_safe_version("1.0;rm -rf"));
    }

    #[test]
    fn test_get_last_transaction_id() {
        let tmp = std::env::temp_dir().join("test_tx_history.jsonl");
        let sample = "{\"id\":1,\"timestamp\":\"2026-09-20 12:00:00\",\"action\":\"install\",\"command\":\"pacpin -S foo\",\"total_packages\":1,\"packages\":[]}\n{\"id\":42,\"timestamp\":\"2026-09-20 12:10:00\",\"action\":\"upgrade\",\"command\":\"pacpin upgrade\",\"total_packages\":1,\"packages\":[]}\n";
        std::fs::write(&tmp, sample).unwrap();

        assert_eq!(TransactionJournal::get_last_transaction_id(&tmp), Some(42));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_find_cached_package_rejection() {
        // Non-existent or traversal versions/names are rejected immediately
        assert_eq!(TransactionJournal::find_cached_package("../etc", "1.0"), None);
        assert_eq!(TransactionJournal::find_cached_package("pacpin-nonexistent-xyz-pkg", "99.99"), None);
    }
}
