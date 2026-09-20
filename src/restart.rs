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
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelUpdateInfo {
    pub running: String,
    pub installed: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceType {
    SystemdSystem(String),
    SystemdUser(String),
    Process { name: String, pid: u32 },
}

#[derive(Debug, Clone)]
pub struct ProcessRestartInfo {
    pub service: ServiceType,
    pub deleted_files: BTreeSet<String>,
}

pub struct RestartInspector;

impl RestartInspector {
    pub fn check_kernel_update(updated_pkgs: &[crate::resolver::ResolvedPackage]) -> Option<KernelUpdateInfo> {
        let running = Self::get_running_kernel()?;
        let modules_dir = Path::new("/usr/lib/modules");

        // 1. Check if the running kernel's module directory was removed/replaced
        let running_module_path = modules_dir.join(&running);
        let running_modules_missing = !running_module_path.exists();

        // 2. Check if any kernel package was updated
        let mut kernel_pkg_updated = None;
        for p in updated_pkgs {
            let name = &p.name;
            if name == "linux"
                || name.starts_with("linux-cachyos")
                || name.starts_with("linux-zen")
                || name.starts_with("linux-lts")
                || name.starts_with("linux-hardened")
            {
                if let Some(cand) = &p.candidate {
                    kernel_pkg_updated = Some(cand.version.clone());
                    break;
                }
            }
        }

        // 3. Find installed versions in /usr/lib/modules
        if let Ok(entries) = fs::read_dir(modules_dir) {
            let mut installed_versions: Vec<String> = entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            installed_versions.sort_by(|a, b| alpm::vercmp(a.as_str(), b.as_str()));

            // If running modules are missing, or a kernel was upgraded
            if running_modules_missing || kernel_pkg_updated.is_some() {
                // Find best matching or latest installed module
                let latest = installed_versions.last().cloned().unwrap_or_else(|| "newer".to_string());
                if latest != running {
                    return Some(KernelUpdateInfo {
                        running,
                        installed: latest,
                    });
                }
            }
        }

        None
    }

    pub fn get_running_kernel() -> Option<String> {
        if let Ok(content) = fs::read_to_string("/proc/sys/kernel/osrelease") {
            let trimmed = content.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
        None
    }

    pub fn check_deleted_libraries() -> Vec<ProcessRestartInfo> {
        let mut map: BTreeMap<String, (ServiceType, BTreeSet<String>)> = BTreeMap::new();
        let my_pid = std::process::id();

        let proc_dir = Path::new("/proc");
        let entries = match fs::read_dir(proc_dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let pid: u32 = match name_str.parse() {
                Ok(p) => p,
                Err(_) => continue,
            };

            // Skip ourselves or kernel threads (PID 1 / 2 can be handled carefully)
            if pid == my_pid || pid == 0 {
                continue;
            }

            let maps_path = entry.path().join("maps");
            let maps_content = match fs::read_to_string(&maps_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let mut deleted_libs = BTreeSet::new();
            for line in maps_content.lines() {
                if line.ends_with("(deleted)") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if let Some(path) = parts.get(5) {
                        if (path.starts_with("/usr/lib") || path.starts_with("/usr/bin"))
                            && path.contains(".so")
                        {
                            deleted_libs.insert(path.to_string());
                        }
                    }
                }
            }

            if deleted_libs.is_empty() {
                continue;
            }

            // Identify process or systemd service
            let cgroup_path = entry.path().join("cgroup");
            let comm_path = entry.path().join("comm");
            let comm = fs::read_to_string(&comm_path)
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| format!("PID {}", pid));

            // Skip pacman, paru, yay, sudo
            if comm == "pacman" || comm == "paru" || comm == "yay" || comm == "sudo" || comm == "pacpin" {
                continue;
            }

            let mut service_type = ServiceType::Process {
                name: comm.clone(),
                pid,
            };

            if let Ok(cgroup_content) = fs::read_to_string(&cgroup_path) {
                for cg_line in cgroup_content.lines() {
                    if cg_line.contains(".service") {
                        let parts: Vec<&str> = cg_line.split('/').collect();
                        for p in parts {
                            if p.ends_with(".service") {
                                if cg_line.contains("/user.slice/") {
                                    service_type = ServiceType::SystemdUser(p.to_string());
                                } else {
                                    service_type = ServiceType::SystemdSystem(p.to_string());
                                }
                                break;
                            }
                        }
                    }
                }
            }

            let key = match &service_type {
                ServiceType::SystemdSystem(s) => format!("sys:{}", s),
                ServiceType::SystemdUser(s) => format!("usr:{}", s),
                ServiceType::Process { name, pid } => format!("proc:{}:{}", name, pid),
            };

            let entry = map.entry(key).or_insert_with(|| (service_type, BTreeSet::new()));
            entry.1.extend(deleted_libs);
        }

        map.into_values()
            .map(|(service, deleted_files)| ProcessRestartInfo {
                service,
                deleted_files,
            })
            .collect()
    }

    pub fn print_restart_advisory(updated_pkgs: &[crate::resolver::ResolvedPackage]) {
        let kernel_update = Self::check_kernel_update(updated_pkgs);
        let services = Self::check_deleted_libraries();

        // Zero-noise guarantee: if nothing requires attention, print nothing
        if kernel_update.is_none() && services.is_empty() {
            return;
        }

        println!();
        println!(
            "{}",
            ":: Post-Upgrade Restart Advisory:".yellow().bold()
        );

        if let Some(k) = kernel_update {
            println!(
                "  {} {} (running: {}, installed: {}) ➔ {}",
                "•".yellow(),
                "Kernel updated:".bold(),
                k.running.cyan(),
                k.installed.green(),
                "System reboot required to boot into new kernel".yellow().bold()
            );
        }

        if !services.is_empty() {
            println!(
                "  {} {}",
                "•".yellow(),
                "Services running outdated shared libraries in RAM:".bold()
            );

            for info in &services {
                let sample_lib = info
                    .deleted_files
                    .iter()
                    .next()
                    .map(|s| {
                        Path::new(s)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| s.clone())
                    })
                    .unwrap_or_default();

                let lib_count = info.deleted_files.len();
                let lib_summary = if lib_count > 1 {
                    format!("{} (+{} others)", sample_lib, lib_count - 1)
                } else {
                    sample_lib
                };

                match &info.service {
                    ServiceType::SystemdSystem(s) => {
                        let restart_cmd = format!("sudo systemctl restart {}", s);
                        println!(
                            "    - {:<30} [{}] ➔ {}",
                            s.cyan().bold(),
                            lib_summary.dimmed(),
                            restart_cmd.green()
                        );
                    }
                    ServiceType::SystemdUser(s) => {
                        let restart_cmd = format!("systemctl --user restart {}", s);
                        println!(
                            "    - {:<30} [{}] ➔ {}",
                            s.cyan().bold(),
                            lib_summary.dimmed(),
                            restart_cmd.green()
                        );
                    }
                    ServiceType::Process { name, pid } => {
                        println!(
                            "    - {:<30} [{}] ➔ {}",
                            format!("{} (PID {})", name, pid).cyan(),
                            lib_summary.dimmed(),
                            "restart application".dimmed()
                        );
                    }
                }
            }
        }
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_running_kernel() {
        let k = RestartInspector::get_running_kernel();
        if Path::new("/proc/sys/kernel/osrelease").exists() {
            assert!(k.is_some());
        }
    }

    #[test]
    fn test_service_type_equality() {
        let s1 = ServiceType::SystemdSystem("dbus.service".to_string());
        let s2 = ServiceType::SystemdSystem("dbus.service".to_string());
        let s3 = ServiceType::SystemdUser("pipewire.service".to_string());
        assert_eq!(s1, s2);
        assert_ne!(s1, s3);
    }

    #[test]
    fn test_kernel_version_sort_semver() {
        let mut versions = vec![
            "6.9.10-arch1-1".to_string(),
            "6.9.9-arch1-1".to_string(),
            "6.10.0-arch1-1".to_string(),
            "6.9.2-arch1-1".to_string(),
        ];
        versions.sort_by(|a, b| alpm::vercmp(a.as_str(), b.as_str()));

        assert_eq!(versions, vec![
            "6.9.2-arch1-1",
            "6.9.9-arch1-1",
            "6.9.10-arch1-1",
            "6.10.0-arch1-1",
        ]);
    }
}
