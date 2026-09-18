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

use crate::journal::TransactionJournal;
use colored::Colorize;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct AliasManager;

impl AliasManager {
    pub fn marker_path() -> PathBuf {
        TransactionJournal::state_dir().join("alias_notified")
    }

    pub fn is_notified() -> bool {
        Self::marker_path().exists()
    }

    pub fn mark_notified() {
        let path = Self::marker_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let now = chrono::Local::now().to_rfc3339();
        let _ = fs::write(path, now);
    }

    pub fn setup_aliases() -> Vec<String> {
        let home = match env::var("HOME") {
            Ok(h) => h,
            Err(_) => return Vec::new(),
        };
        let home_path = Path::new(&home);
        let mut modified = Vec::new();

        // 1. ~/.bashrc
        let bashrc = home_path.join(".bashrc");
        if bashrc.exists() {
            if let Ok(content) = fs::read_to_string(&bashrc) {
                if !content.contains("alias pin=") && !content.contains("alias pin ") {
                    if let Ok(mut file) = OpenOptions::new().append(true).open(&bashrc) {
                        let prefix = if content.ends_with('\n') { "" } else { "\n" };
                        let snippet = format!(
                            "{}# Added by pacpin\nalias pin=\"pacpin\"\nalias pacpin=\"pacpin\"\n",
                            prefix
                        );
                        if file.write_all(snippet.as_bytes()).is_ok() {
                            modified.push("~/.bashrc".to_string());
                        }
                    }
                }
            }
        }

        // 2. ~/.zshrc
        let zshrc = home_path.join(".zshrc");
        if zshrc.exists() {
            if let Ok(content) = fs::read_to_string(&zshrc) {
                if !content.contains("alias pin=") && !content.contains("alias pin ") {
                    if let Ok(mut file) = OpenOptions::new().append(true).open(&zshrc) {
                        let prefix = if content.ends_with('\n') { "" } else { "\n" };
                        let snippet = format!(
                            "{}# Added by pacpin\nalias pin=\"pacpin\"\nalias pacpin=\"pacpin\"\n",
                            prefix
                        );
                        if file.write_all(snippet.as_bytes()).is_ok() {
                            modified.push("~/.zshrc".to_string());
                        }
                    }
                }
            }
        }

        // 3. ~/.config/fish/config.fish
        let fish_conf = home_path.join(".config").join("fish").join("config.fish");
        if fish_conf.exists() {
            if let Ok(content) = fs::read_to_string(&fish_conf) {
                if !content.contains("alias pin") && !content.contains("abbr pin") {
                    if let Ok(mut file) = OpenOptions::new().append(true).open(&fish_conf) {
                        let prefix = if content.ends_with('\n') { "" } else { "\n" };
                        let snippet = format!(
                            "{}# Added by pacpin\nalias pin \"pacpin\"\nalias pacpin \"pacpin\"\n",
                            prefix
                        );
                        if file.write_all(snippet.as_bytes()).is_ok() {
                            modified.push("~/.config/fish/config.fish".to_string());
                        }
                    }
                }
            }
        }

        // 4. Ensure ~/.local/bin/pin symlink exists if ~/.local/bin/pacpin exists
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let local_bin = home_path.join(".local").join("bin");
            let pacpin_bin = local_bin.join("pacpin");
            let pin_bin = local_bin.join("pin");
            if pacpin_bin.exists() && !pin_bin.exists() {
                let _ = symlink("pacpin", &pin_bin);
            }

            if let Ok(exe) = env::current_exe() {
                if let Some(parent) = exe.parent() {
                    let pin_in_parent = parent.join("pin");
                    if !pin_in_parent.exists() {
                        let _ = symlink("pacpin", &pin_in_parent);
                    }
                }
            }
        }

        modified
    }

    pub fn check_and_notify_once() {
        if Self::is_notified() {
            return;
        }

        let modified_files = Self::setup_aliases();
        Self::mark_notified();

        let invoked_as = env::args()
            .next()
            .and_then(|p| Path::new(&p).file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "pacpin".to_string());

        if invoked_as != "pin" {
            println!(
                "{} {} {}",
                "::".cyan().bold(),
                "Tip:".bold(),
                "You can also use 'pin' as a fast shorthand for 'pacpin' (e.g., 'pin check', 'pin -Syu')!".green()
            );
            if !modified_files.is_empty() {
                println!(
                    "   {}",
                    format!(
                        "Configured aliases in {}.",
                        modified_files.join(", ")
                    )
                    .dimmed()
                );
            } else {
                println!(
                    "   {}",
                    "Aliases for 'pin' and 'pacpin' have been configured in your shell.".dimmed()
                );
            }
            println!();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_marker_path() {
        let path = AliasManager::marker_path();
        assert!(path.to_string_lossy().contains("alias_notified"));
    }
}
