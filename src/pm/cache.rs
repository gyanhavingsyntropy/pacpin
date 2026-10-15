//! Scoped cache cleanup and owned stale-download directory handling.
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use colored::Colorize;

use super::check_lock;
use crate::config::Config;
use crate::journal::TransactionJournal;

/// Cleans package cache via AUR helper or pacman, with support for scoping to native-only or AUR-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheScope { All, Native, Aur }

pub fn cache_scope(extra_args: &[String]) -> Result<CacheScope, String> {
    let native = extra_args.iter().any(|arg| arg == "--repo");
    let aur = extra_args.iter().any(|arg| arg == "--aur" || arg == "-a");
    match (native, aur) {
        (true, true) => Err("Cache cleanup cannot combine --repo with --aur".into()),
        (true, false) => Ok(CacheScope::Native),
        (false, true) => Ok(CacheScope::Aur),
        (false, false) => Ok(CacheScope::All),
    }
}

pub fn clean_scoped(
    config: &Config,
    full: bool,
    extra_args: &[String],
    force_native_only: bool,
) -> Result<i32, String> {
    // AUR helpers must never inherit a root pacpin process (e.g. `pkexec pin clean`).
    if unsafe { libc::geteuid() } == 0 {
        return Err("Do not run pacpin as root. Run `pin clean` as your user and set [options] privilege_tool = \"pkexec\" if needed.".into());
    }
    crate::config::validate_privilege_tool(&config.options.privilege_tool)?;
    check_lock()?;
    let is_aur_only = cache_scope(extra_args)? == CacheScope::Aur;
    if !is_aur_only {
        prune_download_directories(config, full)?;
    }
    let helper = &config.options.helper;
    let use_helper = !force_native_only && crate::is_command_available(helper);
    let flag = if full { "-Scc" } else { "-Sc" };
    let mut cmd = build_clean_command(config, full, extra_args, use_helper)?;
    if use_helper {
        if is_aur_only {
            println!(
                "{} Cleaning AUR build cache via {} {} --aur...",
                "::".cyan(),
                helper,
                flag
            );
        } else {
            println!(
                "{} Cleaning package cache via {} {}...",
                "::".cyan(),
                helper,
                flag
            );
        }
    } else {
        println!(
            "{} Cleaning native package cache via {} pacman {}...",
            "::".cyan(), config.options.privilege_tool, flag
        );
    }
    // `pacman -Scc --noconfirm` answers its destructive [y/N] prompt with No.
    // Answer only the known cache-clean questions after pacpin's own approval.
    let input = if full {
        // Queue the bounded answers before spawning. Writing to a child's
        // pipe after it fails early could terminate pacpin with SIGPIPE,
        // hiding the actual cleanup status and skipping restart advice.
        let input = cache_input(&cache_answers(TransactionJournal::get_cache_dirs().len(), is_aur_only))
            .map_err(|error| format!("Could not prepare cache-clean answers: {error}"))?;
        Stdio::from(std::os::fd::OwnedFd::from(input))
    } else { Stdio::inherit() };
    let label = if is_aur_only { "Cleaning AUR build cache" } else if force_native_only || cache_scope(extra_args)? == CacheScope::Native { "Cleaning native package cache" } else { "Cleaning package caches" };
    let status = crate::progress::run_with_stdin(&mut cmd, crate::progress::Manager::Cache, label, input);
    match status {
        Ok(s) => {
            if s.success() { crate::progress::completed(crate::progress::Manager::Cache); }
            Ok(s.code().unwrap_or(1))
        }
        Err(e) => Err(format!("Clean execution failed: {}", e)),
    }
}

pub fn clean(config: &Config, full: bool, extra_args: &[String]) -> Result<i32, String> {
    clean_scoped(config, full, extra_args, false)
}

fn cache_answers(native_dirs: usize, aur_only: bool) -> Vec<u8> {
    if aur_only { return b"y\n".repeat(64); }
    let mut answers = b"y\n".repeat(native_dirs);
    // Cache cleanup must not also remove sync databases.
    answers.extend_from_slice(b"n\n");
    answers.extend_from_slice(&b"y\n".repeat(64));
    answers
}

fn cache_input(answers: &[u8]) -> std::io::Result<std::os::unix::net::UnixStream> {
    if answers.len() > 8192 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Too many cache-clean prompts"));
    }
    let (input, mut output) = std::os::unix::net::UnixStream::pair()?;
    output.write_all(answers)?;
    output.shutdown(std::net::Shutdown::Write)?;
    Ok(input)
}

pub fn clean_preview(config: &Config, full: bool, extra_args: &[String]) -> Result<String, String> {
    let flags: Vec<_> = extra_args.iter().filter(|s| s.as_str() != "--dry-run" && s.as_str() != "-n").cloned().collect();
    let command = build_clean_command(config, full, &flags, crate::is_command_available(&config.options.helper))?;
    Ok(std::iter::once(command.get_program()).chain(command.get_args()).map(|s| crate::utils::shell_quote(&s.to_string_lossy())).collect::<Vec<_>>().join(" "))
}

pub fn helper_privilege_args<'a>(helper: &str, tool: &'a str) -> Result<Vec<&'a str>, String> {
    crate::config::validate_privilege_tool(tool)?;
    if tool == "sudo" { return Ok(Vec::new()); }
    if std::path::Path::new(helper).file_name().is_some_and(|name| name == "paru") {
        Ok(vec!["--sudo", tool])
    } else {
        Err(format!("Cannot configure {tool} for AUR helper '{helper}'; paru supports --sudo, other helpers require their own configuration"))
    }
}

/// Cleans only the caches affected by the completed transaction.
/// If only AUR packages were installed, native pacman cache is untouched.
/// If only native packages were installed, AUR cache is untouched.
/// If neither was installed (e.g. only sources), neither cache is purged.
pub fn clean_after_transaction(config: &Config, has_native: bool, has_aur: bool) -> Result<(), String> {
    if !has_native && !has_aur {
        return Ok(());
    }

    let clean_args = vec!["--noconfirm".to_string()];

    if has_native {
        println!("{}", "\n:: Purging native package cache (--no-cache)...".cyan());
        cleanup_status("Native", clean_scoped(config, true, &clean_args, true)?)?;
        println!("✔ Native cache cleaned successfully.");
    }
    if has_aur {
        let helper = &config.options.helper;
        if crate::is_command_available(helper) {
            println!("{}", "\n:: Purging AUR build cache (--no-cache)...".cyan());
            let aur_clean_args = vec!["--aur".to_string(), "--noconfirm".to_string()];
            cleanup_status("AUR", clean_scoped(config, true, &aur_clean_args, false)?)?;
            println!("✔ AUR cache cleaned successfully.");
        } else {
            return Err(format!("Cannot clean AUR cache: helper '{helper}' is unavailable"));
        }
    }

    Ok(())
}

fn cleanup_status(manager: &str, code: i32) -> Result<(), String> {
    if code == 0 { Ok(()) } else { Err(format!("{manager} cache cleanup exited with code {code}")) }
}

pub fn transaction_cache_cleanup(config: &Config, requested: bool, native: Option<bool>, aur: Option<bool>) -> bool {
    if !requested { return true; }
    let mut success = true;
    for (manager, outcome) in [("Native", native), ("AUR", aur)] {
        if outcome == Some(false) {
            println!(":: {manager} cache cleanup skipped: transaction failed; cached packages preserved for recovery.");
        }
        if outcome == Some(true) {
            if let Err(error) = clean_after_transaction(config, manager == "Native", manager == "AUR") {
                eprintln!(":: Warning: {manager} cache cleanup failed: {error}");
                success = false;
            }
        }
    }
    if native.is_none() && aur.is_none() {
        println!(":: --no-cache: no native or AUR cache cleanup needed for this transaction.");
    }
    success
}

fn prune_download_directories(config: &Config, full: bool) -> Result<(), String> {
    let native_config = crate::pacman_config::snapshot()?;
    let mut empty = Vec::new();
    let mut stale = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let now = SystemTime::now();
    for line in native_config.option("CacheDir") {
        let path = std::path::Path::new(line);
        if !path.is_absolute() { return Err("Native cache path must be absolute".into()); }
        let cache_dir = match path.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("Cannot inspect native cache {}: {error}", path.display())),
        };
        if !seen.insert(cache_dir.clone()) { continue; }
        let entries = std::fs::read_dir(&cache_dir).map_err(|error| format!("Cannot scan native cache {}: {error}", cache_dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("Cannot scan native cache: {error}"))?;
            if !entry.file_name().to_string_lossy().starts_with("download-") || !entry.file_type().is_ok_and(|kind| kind.is_dir()) { continue; }
            if full && is_stale_download_directory(&entry, now) { stale.push(entry.path()); }
            else { empty.push(entry.path()); }
        }
    }
    for (paths, recursive) in [(&empty, false), (&stale, true)] {
        if paths.is_empty() { continue; }
        check_lock()?;
        let mut command = crate::privilege::command(&config.options.privilege_tool);
        if recursive {
            // Full native cleanup only: canonical cache children, no symlink
            // traversal or crossing filesystem boundaries. Recent/non-full
            // directories are handled only by rmdir, which cannot erase files.
            command.args(["/usr/bin/rm", "-rf", "--one-file-system", "--preserve-root=all"]);
        } else { command.arg("/usr/bin/rmdir"); }
        command.arg("--").args(paths);
        let status = crate::progress::run(&mut command, crate::progress::Manager::Cache, "Clearing native download directories", || {})
            .map_err(|error| format!("Download directory cleanup failed: {error}"))?;
        if !status.success() { return Err(format!("Download directory cleanup exited with {status}; recent or non-empty directories were preserved where removal was unsafe")); }
        for path in paths {
            match path.symlink_metadata() {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {},
                Err(error) => return Err(format!("Cannot verify download directory cleanup {}: {error}", path.display())),
                Ok(_) => return Err(format!("Download directory remains after cleanup: {}", path.display())),
            }
            println!(":: Removed inactive ALPM download directory {}", path.display());
        }
    }
    Ok(())
}

fn is_stale_download_directory(entry: &std::fs::DirEntry, now: SystemTime) -> bool {
    entry.file_name().to_string_lossy().starts_with("download-")
        && entry.file_type().is_ok_and(|kind| kind.is_dir())
        && entry.metadata().ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age.as_secs() >= 120)
}

fn build_clean_command(config: &Config, full: bool, extra_args: &[String], use_helper: bool) -> Result<Command, String> {
    let privilege_tool = crate::config::validate_privilege_tool(&config.options.privilege_tool)?;
    if extra_args.iter().any(|arg| arg == "--confirm") {
        return Err("--confirm conflicts with pacpin's single-confirmation clean flow".into());
    }
    let has_sudo_override = extra_args.iter().any(|arg| arg == "--sudo" || arg.starts_with("--sudo="));
    if !use_helper && has_sudo_override {
        return Err("--sudo is an AUR-helper option; set [options] privilege_tool for pacman fallback".into());
    }
    let flag = if full { "-Scc" } else { "-Sc" };
    let is_aur_only = cache_scope(extra_args)? == CacheScope::Aur;
    if is_aur_only && !use_helper { return Err("AUR cache cleanup requires an available AUR helper".into()); }
    let mut cmd = if use_helper {
        let mut cmd = Command::new(&config.options.helper);
        cmd.arg(flag);
        if !is_aur_only && !has_sudo_override && privilege_tool != "sudo" {
            let is_paru = std::path::Path::new(&config.options.helper).file_name()
                .is_some_and(|name| name == "paru");
            if !is_paru {
                return Err(format!(
                    "Cannot configure {privilege_tool} for helper '{}'; only paru's --sudo option is supported. Configure the helper itself or select paru.",
                    config.options.helper
                ));
            }
            cmd.args(["--sudo", privilege_tool]);
        }
        cmd
    } else {
        let mut cmd = crate::privilege::command(privilege_tool);
        cmd.arg("/usr/bin/pacman").arg(flag);
        cmd
    };
    cmd.args(extra_args.iter().filter(|arg| arg.as_str() != "--noconfirm" && (use_helper || arg.as_str() != "--repo")));
    if !full { cmd.arg("--noconfirm"); }
    Ok(cmd)
}

#[cfg(test)]
mod clean_tests {
    use super::*;
    #[test]
    fn cache_scope_is_explicit_and_native_fallback_consumes_helper_selector() {
        assert_eq!(cache_scope(&[]).unwrap(), CacheScope::All);
        assert_eq!(cache_scope(&["--repo".into()]).unwrap(), CacheScope::Native);
        assert_eq!(cache_scope(&["-a".into()]).unwrap(), CacheScope::Aur);
        assert!(cache_scope(&["--repo".into(), "--aur".into()]).is_err());
        let config = Config::default();
        let native = build_clean_command(&config, true, &["--repo".into()], false).unwrap();
        assert_eq!(argv(&native), ["sudo", "/usr/bin/pacman", "-Scc"]);
        assert!(build_clean_command(&config, true, &["--aur".into()], false).is_err());
    }
    #[test]
    fn aur_privilege_override_is_explicit_and_helper_specific() {
        assert_eq!(helper_privilege_args("/usr/bin/paru", "pkexec").unwrap(), ["--sudo", "pkexec"]);
        assert!(helper_privilege_args("yay", "pkexec").is_err());
        assert!(helper_privilege_args("yay", "sudo").unwrap().is_empty());
    }
    #[test]
    fn cache_confirmation_preserves_sync_databases() {
        let answers = cache_answers(2, false);
        assert!(answers.starts_with(b"y\ny\nn\n"));
        assert!(!cache_answers(2, true).contains(&b'n'));
    }

    #[test]
    fn cache_input_has_complete_answers_and_eof_before_a_child_exists() {
        use std::io::Read;
        let answers = cache_answers(2, false);
        let mut input = cache_input(&answers).unwrap();
        let mut read = Vec::new();
        input.read_to_end(&mut read).unwrap();
        assert_eq!(read, answers);
        assert!(cache_input(&vec![0; 8193]).is_err());
    }

    #[test]
    fn failed_cleanup_exit_status_is_never_success() {
        assert!(cleanup_status("Native", 0).is_ok());
        assert!(cleanup_status("Native", 255).unwrap_err().contains("255"));
    }

    fn argv(command: &Command) -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn normal_clean_uses_configured_escalation_and_no_second_prompt() {
        let mut config = Config::default();
        config.options.privilege_tool = "pkexec".into();
        let helper = build_clean_command(&config, false, &[], true).unwrap();
        assert_eq!(argv(&helper), ["paru", "-Sc", "--sudo", "pkexec", "--noconfirm"]);
        let pacman = build_clean_command(&config, false, &[], false).unwrap();
        assert_eq!(argv(&pacman), ["pkexec", "/usr/bin/pacman", "-Sc", "--noconfirm"]);
    }

    #[test]
    fn full_clean_preserves_scc_and_does_not_choose_default_no() {
        let config = Config::default();
        let flags = vec!["--noconfirm".to_string()];
        let helper = build_clean_command(&config, true, &flags, true).unwrap();
        assert_eq!(argv(&helper), ["paru", "-Scc"]);
        let pacman = build_clean_command(&config, true, &flags, false).unwrap();
        assert_eq!(argv(&pacman), ["sudo", "/usr/bin/pacman", "-Scc"]);
    }

    #[test]
    fn aur_clean_does_not_use_privilege_escalation() {
        let mut config = Config::default();
        config.options.privilege_tool = "pkexec".into();
        let flags = vec!["--aur".to_string(), "--noconfirm".to_string()];
        let helper = build_clean_command(&config, true, &flags, true).unwrap();
        assert_eq!(argv(&helper), ["paru", "-Scc", "--aur"]);
    }

    #[test]
    fn native_only_clean_runs_pacman_directly() {
        let mut config = Config::default();
        config.options.privilege_tool = "pkexec".into();
        let flags = vec!["--noconfirm".to_string()];
        let pacman = build_clean_command(&config, true, &flags, false).unwrap();
        assert_eq!(argv(&pacman), ["pkexec", "/usr/bin/pacman", "-Scc"]);
    }

    #[test]
    fn clean_rejects_conflicting_prompt_and_escalation_flags() {
        let config = Config::default();
        assert!(build_clean_command(&config, false, &["--confirm".into()], true).is_err());
        assert!(build_clean_command(&config, false, &["--sudo=pkexec".into()], false).is_err());
        assert!(crate::config::validate_privilege_tool("sudo --bad-flag").is_err());
    }

    #[test]
    fn stale_download_dir_filter_ignores_new_dirs_and_files() {
        let root = std::env::temp_dir().join(format!("pacpin_clean_dirs_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(root.join("download-old")).unwrap();
        std::fs::create_dir_all(root.join("ordinary")).unwrap();
        std::fs::write(root.join("download-file"), b"not a directory").unwrap();
        let entries = std::fs::read_dir(&root).unwrap().map(Result::unwrap).collect::<Vec<_>>();
        let now = SystemTime::now();
        assert!(entries.iter().all(|entry| !is_stale_download_directory(entry, now)));
        let later = now + std::time::Duration::from_secs(121);
        let matched = entries.iter().filter(|entry| is_stale_download_directory(entry, later))
            .map(|entry| entry.file_name().to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(matched, ["download-old"]);
        std::fs::remove_dir_all(root).unwrap();
    }
}
