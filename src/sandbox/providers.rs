//! Backend-specific ephemeral operations; these are distinct from upgrade adapters.
use colored::Colorize;
use std::process::Command;
use crate::execution::run_tracked_command;

pub(super) fn try_flatpak(app_id: &str, args: &[String]) -> Result<i32, String> {
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
    let is_already_installed = crate::execution::capture_output(Command::new("flatpak").args(["info", app_id]))
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
                let status = run_tracked_command(Command::new("flatpak").args(["uninstall", "--user", "-y", self.app_id]));
                // Flatpak app data may predate this invocation. Never delete it here.
                match status {
                    Ok(status) if status.success() => println!("✔ Ephemeral Flatpak '{}' uninstalled.", self.app_id.green()),
                    _ => eprintln!(":: Warning: Ephemeral Flatpak '{}' cleanup failed; the app may remain installed.", self.app_id),
                }
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
        let install_status = run_tracked_command(Command::new("flatpak").args(["install", "--user", "--noninteractive", "-y", "flathub", app_id]))
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

    let child_status = run_tracked_command(Command::new("flatpak").args(&run_args))
        .map_err(|e| format!("Failed to execute flatpak run: {}", e))?;

    drop(guard);
    Ok(child_status.code().unwrap_or(1))
}

pub(super) fn try_nix(pkg: &str, args: &[String]) -> Result<i32, String> {
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

    let status = run_tracked_command(Command::new("nix").args(&nix_args))
        .map_err(|e| format!("Failed to execute nix: {}", e))?;

    Ok(status.code().unwrap_or(1))
}

pub(super) fn try_pipx(pkg: &str, args: &[String], no_cache: bool) -> Result<i32, String> {
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

    let mut pipx_args = vec!["run"];
    if no_cache { pipx_args.push("--no-cache"); }
    pipx_args.push(pkg);
    pipx_args.extend(args.iter().map(|s| s.as_str()));

    // Pipx's freshness flag alone can defer environment deletion. Own the
    // entire run cache when no-cache is requested, without replacing HOME or
    // touching previously installed environments. Footprint is unknown: this
    // is a starting capacity check, not a complete Python build guarantee.
    let workspace = if no_cache { Some(crate::scratch::Workspace::create(crate::scratch::Policy::Auto, 64 * 1024 * 1024)?) } else { None };
    let mut command = Command::new("pipx");
    command.args(&pipx_args);
    if let Some(workspace) = &workspace {
        println!(":: --no-cache: private Pipx workspace {}; removed on exit. Python build footprint is unknown.", workspace.path.display());
        for (key, directory) in [("PIPX_HOME", "home"), ("PIPX_BIN_DIR", "bin"), ("PIPX_MAN_DIR", "man"), ("PIPX_SHARED_LIBS", "shared"), ("PIPX_VENV_CACHEDIR", "venvs"), ("PIP_CACHE_DIR", "pip-cache"), ("UV_CACHE_DIR", "uv-cache")] {
            command.env(key, workspace.path.join(directory));
        }
        command.env("TMPDIR", &workspace.path);
    }
    let status = run_tracked_command(&mut command)
        .map_err(|e| format!("Failed to execute pipx: {}", e))?;

    Ok(status.code().unwrap_or(1))
}

pub(super) fn is_safe_flatpak_id(id: &str) -> bool {
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
