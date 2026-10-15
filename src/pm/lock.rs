//! Native database lock inspection and bounded waiting.
use colored::Colorize;

use crate::db::AlpmManager;

/// Information regarding the pacman database lock.
#[derive(Debug, Clone)]
pub struct LockInfo {
    pub path: std::path::PathBuf,
    pub pid: Option<i32>,
    pub process_name: Option<String>,
    pub is_alive: bool,
}

/// Inspects the pacman database lock file and the process holding it (if any).
pub fn get_lock_info() -> Option<LockInfo> {
    let lock_path = AlpmManager::get_dbpath().join("db.lck");
    get_lock_info_at(lock_path)
}

pub(super) fn get_lock_info_at(lock_path: std::path::PathBuf) -> Option<LockInfo> {
    if !lock_path.exists() {
        return None;
    }

    let mut pid_opt = None;
    if let Ok(content) = std::fs::read_to_string(&lock_path) {
        if let Some(token) = content.split_whitespace().next() {
            if let Ok(pid) = token.parse::<i32>() {
                if pid > 0 {
                    pid_opt = Some(pid);
                }
            }
        }
    }

    let (is_alive, process_name) = if let Some(pid) = pid_opt {
        #[cfg(unix)]
        let alive = unsafe { libc::kill(pid, 0) == 0 };
        #[cfg(not(unix))]
        let alive = true;

        let name = if alive {
            std::fs::read_to_string(format!("/proc/{}/comm", pid))
                .ok()
                .map(|s| s.trim().to_string())
        } else {
            None
        };
        (alive, name)
    } else {
        // Pacman creates db.lck with mode 0000 and 0 bytes without writing a PID.
        // On Linux, scan /proc for active package manager processes.
        let mut active_pm_found = None;
        let my_pid = std::process::id() as i32;

        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if let Ok(pid) = name.parse::<i32>() {
                        if pid == my_pid {
                            continue;
                        }
                        if let Ok(comm) = std::fs::read_to_string(format!("/proc/{}/comm", pid)) {
                            let trimmed = comm.trim();
                            if matches!(
                                trimmed,
                                "pacman"
                                    | "paru"
                                    | "yay"
                                    | "pamac-daemon"
                                    | "packagekitd"
                                    | "pamac"
                                    | "pacpin"
                                    | "pin"
                            ) {
                                active_pm_found = Some((pid, trimmed.to_string()));
                                break;
                            }
                        }
                    }
                }
            }
        }

        if let Some((pid, name)) = active_pm_found {
            pid_opt = Some(pid);
            (true, Some(name))
        } else {
            // db.lck exists, but no package manager process is running on the system!
            // It is definitively a stale lock.
            (false, None)
        }
    };

    Some(LockInfo {
        path: lock_path,
        pid: pid_opt,
        process_name,
        is_alive,
    })
}

/// Checks if pacman database is currently locked.
pub fn check_lock() -> Result<(), String> {
    crate::pacman_config::snapshot()?;
    if let Some(info) = get_lock_info() {
        if !info.is_alive {
            if let Some(pid) = info.pid {
                return Err(format!(
                    "Pacman database lock exists ({}), but process PID {} is no longer running (stale lock).\n\
                    If no other package manager is active, remove it with: sudo rm {}",
                    info.path.display(),
                    pid,
                    info.path.display()
                ));
            } else {
                return Err(format!(
                    "Pacman database lock exists ({}), but no active package manager process (pacman, paru, yay) is running (stale lock).\n\
                    If no other package manager is active, remove it with: sudo rm {}",
                    info.path.display(),
                    info.path.display()
                ));
            }
        }

        if let Some(ref name) = info.process_name {
            if let Some(pid) = info.pid {
                return Err(format!(
                    "Pacman database is locked ({}). Process '{}' (PID {}) is currently running.",
                    info.path.display(),
                    name,
                    pid
                ));
            } else {
                return Err(format!(
                    "Pacman database is locked ({}). Process '{}' is currently running.",
                    info.path.display(),
                    name
                ));
            }
        }

        if let Some(pid) = info.pid {
            return Err(format!(
                "Pacman database is locked ({}). Process PID {} is currently running.",
                info.path.display(),
                pid
            ));
        }

        return Err(format!(
            "Pacman database is locked ({}). Another package management process is currently running.",
            info.path.display()
        ));
    }
    Ok(())
}

/// Waits for pacman database lock to be released, or times out.
pub fn wait_for_lock(max_wait_secs: u64) -> Result<(), String> {
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(max_wait_secs);
    let mut warned = false;

    loop {
        match check_lock() {
            Ok(()) => return Ok(()),
            Err(e) => {
                // If it's a STALE lock, waiting won't help; fail immediately!
                if let Some(info) = get_lock_info() {
                    if !info.is_alive {
                        return Err(e);
                    }
                }
                if start.elapsed() >= timeout {
                    return Err(format!(
                        "Timed out waiting for pacman database lock after {}s: {}",
                        max_wait_secs, e
                    ));
                }
                if !warned {
                    eprintln!(
                        "{}",
                        format!(
                            ":: Waiting for pacman database lock to be released (timeout: {}s)...",
                            max_wait_secs
                        )
                        .yellow()
                    );
                    warned = true;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
    }
}
