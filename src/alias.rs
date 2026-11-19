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
    pub fn marker_path() -> Option<PathBuf> {
        TransactionJournal::state_dir().ok().map(|d| d.join("alias_notified"))
    }

    #[allow(dead_code)]
    pub fn is_notified() -> bool {
        Self::marker_path().map(|p| p.exists()).unwrap_or(false)
    }

    pub fn try_acquire_first_notification() -> bool {
        let path = match Self::marker_path() {
            Some(p) => p,
            None => return false,
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let now = chrono::Local::now().to_rfc3339();
                let _ = writeln!(file, "{}", now);
                true
            }
            Err(_) => false,
        }
    }

    pub fn mark_notified() {
        let path = match Self::marker_path() {
            Some(p) => p,
            None => return,
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let now = chrono::Local::now().to_rfc3339();
        let _ = fs::write(path, now);
    }

    pub fn setup_aliases() -> Vec<String> {
        Self::setup_aliases_with_config(&crate::config::load_config())
    }

    pub fn setup_aliases_with_config(cfg: &crate::config::Config) -> Vec<String> {
        if !cfg.features.shell_aliases {
            return Vec::new();
        }

        let home = match env::var("HOME") {
            Ok(h) => h,
            Err(_) => return Vec::new(),
        };
        let home_path = Path::new(&home);
        let mut modified = Vec::new();

        let has_pin_alias = |content: &str, is_fish: bool| -> bool {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with('#') {
                    continue;
                }
                if is_fish {
                    if trimmed.starts_with("alias pin ")
                        || trimmed.starts_with("alias pin=")
                        || trimmed.starts_with("abbr pin ")
                        || trimmed.starts_with("abbr pin=")
                    {
                        return true;
                    }
                } else if trimmed.starts_with("alias pin=") || trimmed.starts_with("alias pin ") {
                    return true;
                }
            }
            false
        };

        // 1. ~/.bashrc
        let bashrc = home_path.join(".bashrc");
        if bashrc.exists() {
            if let Ok(content) = fs::read_to_string(&bashrc) {
                if !has_pin_alias(&content, false) {
                    if let Ok(mut file) = OpenOptions::new().append(true).open(&bashrc) {
                        let prefix = if content.ends_with('\n') { "" } else { "\n" };
                        let snippet = format!(
                            "{}# Added by pacpin\nalias pin=\"pacpin\"\n",
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
                if !has_pin_alias(&content, false) {
                    if let Ok(mut file) = OpenOptions::new().append(true).open(&zshrc) {
                        let prefix = if content.ends_with('\n') { "" } else { "\n" };
                        let snippet = format!(
                            "{}# Added by pacpin\nalias pin=\"pacpin\"\n",
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
                if !has_pin_alias(&content, true) {
                    if let Ok(mut file) = OpenOptions::new().append(true).open(&fish_conf) {
                        let prefix = if content.ends_with('\n') { "" } else { "\n" };
                        let snippet = format!(
                            "{}# Added by pacpin\nalias pin \"pacpin\"\n",
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
        let cfg = crate::config::load_config();
        if !cfg.features.shell_aliases {
            return;
        }

        if !Self::try_acquire_first_notification() {
            return;
        }

        let modified_files = Self::setup_aliases_with_config(&cfg);

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
        let path = AliasManager::marker_path().expect("marker_path should resolve when HOME is set");
        assert!(path.to_string_lossy().contains("alias_notified"));
    }
}
