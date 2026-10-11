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
    SessionComponent(String),
    Process { name: String, pid: u32 },
}

#[derive(Debug, Clone)]
pub struct ProcessRestartInfo {
    pub service: ServiceType,
    pub deleted_files: BTreeSet<String>,
}

pub struct RestartInspector;

impl RestartInspector {
    pub fn check_kernel_update(
        updated_pkgs: &[crate::resolver::ResolvedPackage],
    ) -> Option<KernelUpdateInfo> {
        let running = Self::get_running_kernel()?;
        let modules_dir = Path::new("/usr/lib/modules");

        // 1. Check if the running kernel's module directory was removed/replaced
        let running_module_path = modules_dir.join(&running);
        let running_modules_missing = !running_module_path.exists();

        // 2. Check if any kernel package was updated
        let mut kernel_pkg_updated = None;
        for p in updated_pkgs {
            let name = &p.name;
            if crate::utils::is_kernel_package(name) {
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
                let latest = installed_versions
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "newer".to_string());
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

    pub fn is_session_component(comm: &str, service_name: &str) -> bool {
        let comm_lower = comm.to_ascii_lowercase();
        let s_lower = service_name.to_ascii_lowercase();
        comm_lower == "gnome-shell"
            || comm_lower == "mutter"
            || comm_lower.starts_with("mutter-")
            || comm_lower == "kwin"
            || comm_lower == "kwin_wayland"
            || comm_lower == "kwin_x11"
            || comm_lower == "sway"
            || comm_lower == "hyprland"
            || comm_lower == "wayfire"
            || comm_lower == "cosmic-comp"
            || comm_lower == "gdm"
            || comm_lower == "sddm"
            || comm_lower == "lightdm"
            || comm_lower == "xorg"
            || comm_lower == "xwayland"
            || s_lower.contains("gnome.shell")
            || s_lower.contains("kwin")
            || s_lower.starts_with("gdm")
            || s_lower.starts_with("sddm")
            || s_lower.starts_with("lightdm")
    }

    pub fn parse_cgroup_service(cgroup_content: &str, comm: &str, pid: u32) -> ServiceType {
        for cg_line in cgroup_content.lines() {
            let parts: Vec<&str> = cg_line.split('/').filter(|p| !p.is_empty()).collect();
            // Find the most specific (leaf-most) service in the cgroup path,
            // strictly ignoring the top-level user@<uid>.service session container.
            let candidate_service = parts
                .iter()
                .rev()
                .find(|p| p.ends_with(".service") && !p.starts_with("user@"));

            if let Some(srv) = candidate_service {
                if Self::is_session_component(comm, srv) {
                    return ServiceType::SessionComponent(srv.to_string());
                } else if cg_line.contains("/user.slice/") {
                    return ServiceType::SystemdUser(srv.to_string());
                } else {
                    return ServiceType::SystemdSystem(srv.to_string());
                }
            } else if parts.iter().any(|p| p.starts_with("user@")) {
                let leaf = parts.last().copied().unwrap_or("");
                if Self::is_session_component(comm, leaf) {
                    return ServiceType::SessionComponent(comm.to_string());
                } else if leaf == "init.scope" || leaf.starts_with("user@") {
                    return ServiceType::SystemdUser("systemd --user".to_string());
                } else {
                    return ServiceType::Process {
                        name: comm.to_string(),
                        pid,
                    };
                }
            } else if let Some(leaf) = parts.last() {
                if leaf.ends_with(".service") {
                    if Self::is_session_component(comm, leaf) {
                        return ServiceType::SessionComponent(leaf.to_string());
                    } else {
                        return ServiceType::SystemdSystem(leaf.to_string());
                    }
                }
            }
        }

        ServiceType::Process {
            name: comm.to_string(),
            pid,
        }
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

            // Skip ourselves or kernel threads
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

            // Skip pacman, paru, yay, sudo, pacpin, pin
            if comm == "pacman"
                || comm == "paru"
                || comm == "yay"
                || comm == "sudo"
                || comm == "pacpin"
                || comm == "pin"
            {
                continue;
            }

            let cgroup_content = fs::read_to_string(&cgroup_path).unwrap_or_default();
            let service_type = Self::parse_cgroup_service(&cgroup_content, &comm, pid);

            let key = match &service_type {
                ServiceType::SystemdSystem(s) => format!("sys:{}", s),
                ServiceType::SystemdUser(s) => format!("usr:{}", s),
                ServiceType::SessionComponent(s) => format!("session:{}", s),
                ServiceType::Process { name, pid } => format!("proc:{}:{}", name, pid),
            };

            let entry = map
                .entry(key)
                .or_insert_with(|| (service_type, BTreeSet::new()));
            entry.1.extend(deleted_libs);
        }

        map.into_values()
            .map(|(service, deleted_files)| ProcessRestartInfo {
                service,
                deleted_files,
            })
            .collect()
    }

    pub fn print_restart_advisory(
        updated_pkgs: &[crate::resolver::ResolvedPackage],
        privilege_tool: &str,
    ) {
        let (kernel_update, services) = crate::progress::operation(
            crate::progress::Manager::Restart, "Checking restart requirements",
            || (Self::check_kernel_update(updated_pkgs), Self::check_deleted_libraries()),
        );

        // Zero-noise guarantee: if nothing requires attention, print nothing
        if kernel_update.is_none() && services.is_empty() {
            return;
        }

        for line in render_advisory(
            kernel_update.as_ref(),
            &services,
            crate::ui::get_terminal_width(),
            privilege_tool,
        ) {
            println!("{line}");
        }
    }
}

fn wrap_field(text: &str, prefix: &str, continuation: &str, width: usize) -> Vec<String> {
    crate::ui::wrap_field(text, prefix, continuation, width)
}

fn command_lines(tokens: &[String], width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = "      ".to_string();
    for token in tokens {
        if !line.trim().is_empty() && crate::ui::str_width(&line) + token.len() + 3 > width {
            line.push_str(" \\");
            lines.push(line);
            line = "      ".into();
        }
        if !line.ends_with(' ') {
            line.push(' ');
        }
        line.push_str(token);
    }
    lines.push(line);
    // Keep shell arguments intact. An argument wider than the terminal soft
    // wraps naturally; inserting a newline inside its quotes changes the unit.
    lines
}

pub(crate) fn render_advisory(
    kernel: Option<&KernelUpdateInfo>,
    services: &[ProcessRestartInfo],
    width: usize,
    privilege_tool: &str,
) -> Vec<String> {
    if kernel.is_none() && services.is_empty() {
        return Vec::new();
    }
    let width = width.max(8);
    let mut lines = vec![String::new()];
    lines.extend(
        wrap_field(":: Post-Upgrade Restart Advisory:", "", "", width)
            .into_iter()
            .map(|line| line.yellow().bold().to_string()),
    );
    if let Some(kernel) = kernel {
        lines.extend(wrap_field(
            "Kernel updated; reboot required.",
            "  ",
            "  ",
            width,
        ));
        lines.extend(wrap_field(
            &format!("Running: {}", kernel.running),
            "    ",
            "    ",
            width,
        ));
        lines.extend(wrap_field(
            &format!("Installed: {}", kernel.installed),
            "    ",
            "    ",
            width,
        ));
    }
    if !services.is_empty() {
        lines.extend(wrap_field(
            "Services using outdated libraries:",
            "  ",
            "  ",
            width,
        ));
    }
    for info in services {
        let name = match &info.service {
            ServiceType::SystemdSystem(name)
            | ServiceType::SystemdUser(name)
            | ServiceType::SessionComponent(name) => name.clone(),
            ServiceType::Process { name, pid } => format!("{name} (PID {pid})"),
        };
        lines.extend(
            wrap_field(&name, "    - ", "      ", width)
                .into_iter()
                .map(|line| line.cyan().bold().to_string()),
        );
        let sample = info
            .deleted_files
            .iter()
            .next()
            .map(|name| {
                Path::new(name)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| name.clone())
            })
            .unwrap_or_default();
        let libraries = if info.deleted_files.len() > 1 {
            format!("{sample} (+{} others)", info.deleted_files.len() - 1)
        } else {
            sample
        };
        lines.extend(
            wrap_field(&format!("Library: {libraries}"), "      ", "      ", width)
                .into_iter()
                .map(|line| line.dimmed().to_string()),
        );
        let tokens = match &info.service {
            ServiceType::SystemdSystem(name) => Some(vec![
                privilege_tool.into(),
                "systemctl".into(),
                "restart".into(),
                crate::utils::shell_quote(name),
            ]),
            ServiceType::SystemdUser(name) if name == "systemd --user" => Some(vec![
                "systemctl".into(),
                "--user".into(),
                "daemon-reexec".into(),
            ]),
            ServiceType::SystemdUser(name) => Some(vec![
                "systemctl".into(),
                "--user".into(),
                "restart".into(),
                crate::utils::shell_quote(name),
            ]),
            ServiceType::SessionComponent(_) => {
                lines.extend(
                    wrap_field(
                        "Log out and back in, or reboot (desktop session).",
                        "      ",
                        "      ",
                        width,
                    )
                    .into_iter()
                    .map(|line| line.yellow().to_string()),
                );
                None
            }
            ServiceType::Process { .. } => {
                lines.extend(wrap_field(
                    "Restart the application.",
                    "      ",
                    "      ",
                    width,
                ));
                None
            }
        };
        if let Some(tokens) = tokens {
            lines.extend(
                command_lines(&tokens, width)
                    .into_iter()
                    .map(|line| line.green().to_string()),
            );
        }
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advisory_wraps_long_session_names_and_library_fields() {
        let service = ProcessRestartInfo {
            service: ServiceType::SessionComponent(
                "dbus-:1.1-org.gnome.Shell.Notifications@0.service".into(),
            ),
            deleted_files: ["/usr/lib/libupower-glib.so.3.1.2".into()]
                .into_iter()
                .collect(),
        };
        for width in [24, 32, 40, 60, 80, 120] {
            let lines = render_advisory(None, std::slice::from_ref(&service), width, "pkexec");
            assert!(
                lines.iter().all(|line| crate::ui::str_width(line) <= width),
                "overflow at {width}: {lines:?}"
            );
            assert!(!lines.iter().any(|line| line.contains("systemctl")));
        }
        assert!(render_advisory(None, &[], 40, "pkexec").is_empty());
    }

    #[test]
    fn advisory_commands_preserve_units_and_runtime_privilege_tool() {
        let services = [
            ProcessRestartInfo {
                service: ServiceType::SystemdSystem("a-long-system-service-name.service".into()),
                deleted_files: BTreeSet::new(),
            },
            ProcessRestartInfo {
                service: ServiceType::SystemdUser("org.gnome.SettingsDaemon.Power.service".into()),
                deleted_files: BTreeSet::new(),
            },
        ];
        let text = render_advisory(None, &services, 40, "pkexec").join("\n");
        assert!(text.contains("pkexec systemctl restart"));
        assert!(!text.contains("sudo"));
        assert!(text.contains("'org.gnome.SettingsDaemon.Power.service'"));
        assert!(text.contains("systemctl --user restart"));
        assert!(text.contains(" \\\n"));
    }

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

        assert_eq!(
            versions,
            vec![
                "6.9.2-arch1-1",
                "6.9.9-arch1-1",
                "6.9.10-arch1-1",
                "6.10.0-arch1-1",
            ]
        );
    }

    #[test]
    fn test_parse_cgroup_service() {
        // 1. User service under user@1000.service
        let cg_user =
            "0::/user.slice/user-1000.slice/user@1000.service/session.slice/wireplumber.service";
        let res = RestartInspector::parse_cgroup_service(cg_user, "wireplumber", 2388);
        assert_eq!(
            res,
            ServiceType::SystemdUser("wireplumber.service".to_string())
        );

        // 2. System service under system.slice
        let cg_sys = "0::/system.slice/bluetooth.service";
        let res = RestartInspector::parse_cgroup_service(cg_sys, "bluetoothd", 1234);
        assert_eq!(
            res,
            ServiceType::SystemdSystem("bluetooth.service".to_string())
        );

        // 3. Application in user scope
        let cg_scope = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-org.kde.kdeconnect.daemon-2774.scope";
        let res = RestartInspector::parse_cgroup_service(cg_scope, "kdeconnectd", 2774);
        assert_eq!(
            res,
            ServiceType::Process {
                name: "kdeconnectd".to_string(),
                pid: 2774
            }
        );

        // 4. Desktop session compositor
        let cg_shell = "0::/user.slice/user-1000.slice/user@1000.service/session.slice/org.gnome.Shell@user.service";
        let res = RestartInspector::parse_cgroup_service(cg_shell, "gnome-shell", 2586);
        assert_eq!(
            res,
            ServiceType::SessionComponent("org.gnome.Shell@user.service".to_string())
        );

        // 5. systemd --user manager itself
        let cg_init = "0::/user.slice/user-1000.slice/user@1000.service/init.scope";
        let res = RestartInspector::parse_cgroup_service(cg_init, "systemd", 2372);
        assert_eq!(res, ServiceType::SystemdUser("systemd --user".to_string()));
    }
}
