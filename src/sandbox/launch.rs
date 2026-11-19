//! Launch already prepared package roots; every package backend shares this policy.
use super::{
    mounts::{
        add_host_runtime_mounts, add_package_data_mounts, safe_wayland_socket, should_bind_cwd,
        supports_ro_overlay,
    },
    TryOptions,
};
use crate::execution::run_tracked_command;
use colored::Colorize;
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

pub(super) fn launch(
    sandbox_dir: &Path,
    binary_path: &Path,
    pkg: &str,
    args: &[String],
    options: &TryOptions,
    installed_execution: bool,
    package_view: bool,
) -> Result<i32, String> {
    // 3. Construct isolated environment paths
    let usr_bin = sandbox_dir.join("usr").join("bin");
    let usr_sbin = sandbox_dir.join("usr").join("sbin");
    let usr_lib = sandbox_dir.join("usr").join("lib");
    let usr_lib64 = sandbox_dir.join("usr").join("lib64");
    let usr_share = sandbox_dir.join("usr").join("share");

    let current_path =
        env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string());
    let new_path = format!(
        "{}:{}:{}",
        usr_bin.display(),
        usr_sbin.display(),
        current_path
    );
    let sandbox_path = format!(
        "{}:{}:/usr/local/bin:/usr/bin:/bin",
        usr_bin.display(),
        usr_sbin.display()
    );

    let current_ld = env::var("LD_LIBRARY_PATH").unwrap_or_default();
    let new_ld = if current_ld.is_empty() {
        format!("{}:{}", usr_lib.display(), usr_lib64.display())
    } else {
        format!(
            "{}:{}:{}",
            usr_lib.display(),
            usr_lib64.display(),
            current_ld
        )
    };
    let sandbox_ld = format!("{}:{}", usr_lib.display(), usr_lib64.display());

    let current_xdg =
        env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    let new_xdg = format!("{}:{}", usr_share.display(), current_xdg);
    let sandbox_xdg = format!("{}:/usr/local/share:/usr/share", usr_share.display());

    if options.no_sandbox {
        if installed_execution {
            println!(
                "{} Running installed '{}' on host...\n",
                "::".cyan().bold(),
                pkg.bold()
            );
        } else if options.is_run_mode {
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
            if options.share_net {
                "shared"
            } else {
                "isolated"
            },
            if options.rw_cwd {
                "read-write"
            } else {
                "read-only"
            }
        );
        println!(
            "{} Running {} '{}' (sandbox will be destroyed upon exit)...\n",
            "::".cyan(),
            if installed_execution {
                "installed"
            } else {
                "ephemeral"
            },
            pkg.bold()
        );
    }

    // Applications can own their terminal UI. Retire preparation's footer
    // before executing them, including interactive host programs.
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
        if package_view {
            if Path::new("/opt").exists() {
                bwrap_cmd.args(["--ro-bind", "/opt", "/opt"]);
            }
            crate::execution::package_mounts(&mut bwrap_cmd, sandbox_dir, supports_ro_overlay())?;
        } else {
            add_package_data_mounts(&mut bwrap_cmd, sandbox_dir, supports_ro_overlay())?;
        }

        // Virtual filesystems
        bwrap_cmd
            .arg("--proc")
            .arg("/proc")
            .arg("--dev")
            .arg("/dev")
            .arg("--tmpfs")
            .arg("/tmp");

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
        bwrap_cmd.arg("--ro-bind").arg(sandbox_dir).arg(sandbox_dir);

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
            if let (Ok(runtime), Ok(display)) =
                (env::var("XDG_RUNTIME_DIR"), env::var("WAYLAND_DISPLAY"))
            {
                if let Some(socket) = safe_wayland_socket(Path::new(&runtime), &display) {
                    bwrap_cmd
                        .arg("--ro-bind")
                        .arg(&socket)
                        .arg(Path::new("/tmp").join(&display));
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

        bwrap_cmd.arg("--").arg(binary_path).args(args);
        run_tracked_command(&mut bwrap_cmd)
    } else if package_view {
        // Run keeps the user's normal files and network. A mount view makes
        // /opt and compiled absolute paths work without copying into the host.
        let mut command = Command::new("bwrap");
        command.args(["--dev-bind", "/", "/", "--die-with-parent"]);
        crate::execution::package_mounts(&mut command, sandbox_dir, supports_ro_overlay())?;
        command
            .arg("--")
            .arg(binary_path)
            .args(args)
            .env("PATH", &new_path)
            .env("LD_LIBRARY_PATH", &new_ld)
            .env("XDG_DATA_DIRS", &new_xdg);
        run_tracked_command(&mut command)
    } else {
        run_tracked_command(
            Command::new(binary_path)
                .args(args)
                .env("PATH", &new_path)
                .env("LD_LIBRARY_PATH", &new_ld)
                .env("XDG_DATA_DIRS", &new_xdg),
        )
    };

    match child_status {
        Ok(s) => Ok(s.code().unwrap_or(1)),
        Err(e) => Err(format!("Execution failed: {}", e)),
    }
}
