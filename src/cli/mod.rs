//! Invocation startup, option routing and command dispatch.
use colored::Colorize;
use crate::commands::{
    cmd_capture, cmd_check, cmd_clean, cmd_delay, cmd_history, cmd_info,
    cmd_install, cmd_keep, cmd_list, cmd_orphans, cmd_pin, cmd_print_uris,
    cmd_rebuild, cmd_remove, cmd_repos, cmd_reset, cmd_run, cmd_search,
    cmd_sync_groups, cmd_sync_list, cmd_try, cmd_undelay, cmd_unpin, cmd_upgrade,
    is_refresh_only_invocation, is_upgrade_invocation, pacman_files_needs_sudo,
    parse_pacman_cli_args, parse_pin_args,
};
use crate::config::load_config;
use crate::db::AlpmManager;
use crate::journal::TransactionJournal;
use crate::{config, integrations, manager_config, pm, privilege, progress, restart, runtime, sandbox, scratch, wizard, is_command_available};
use std::env;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::{exit, Command};
use help::{print_help, print_version};
mod help;
#[cfg(test)]
mod tests;

fn parse_clean_args(args: &[String]) -> (bool, Vec<String>) {
    let mut clean_flags = 0usize;
    let mut rest = Vec::new();
    for arg in args {
        if arg.starts_with("-S") && arg[2..].chars().all(|c| c == 'c') && arg.len() > 2 {
            clean_flags += arg.len() - 2;
        } else if arg == "-c" || arg == "--clean" {
            clean_flags += 1;
        } else if arg != "clean" && arg != "clean-cache" && arg != "-S" {
            rest.push(arg.clone());
        }
    }
    (clean_flags >= 2, rest)
}

fn is_clean_invocation(args: &[String]) -> bool {
    let Some(cmd) = args.first().map(String::as_str) else { return false; };
    cmd == "clean"
        || cmd == "clean-cache"
        || cmd == "-Sc"
        || cmd == "-Scc"
        || (cmd.starts_with("-S")
            && (cmd.contains('c') || args.iter().any(|a| a == "-c" || a == "--clean"))
            && !is_upgrade_invocation(args)
            && !args.iter().skip(1).any(|a| !a.starts_with('-')))
}

pub(crate) fn run() {
    let _progress_view = progress::session_view();
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    crate::execution::init_signal_handlers();
    sandbox::clean_stale_sandboxes();

    if std::env::var_os("NO_COLOR").is_some()
        || (!io::stdout().is_terminal() && std::env::var_os("CLICOLOR_FORCE").is_none())
    {
        colored::control::set_override(false);
    }

    let mut args: Vec<String> = env::args().skip(1).collect();

    // Setup has no runtime package operation: validate its own options before
    // invocation-only privilege/scratch flags can be consumed silently.
    if args.first().is_some_and(|arg| matches!(arg.as_str(), "init" | "setup")) {
        match wizard::parse_args(&args) {
            Ok(wizard::Action::Help) => wizard::print_help(),
            Ok(wizard::Action::Setup { reset }) => {
                if let Err(error) = wizard::run(reset) { eprintln!("Error: {error}"); exit(1); }
            }
            Err(error) => { eprintln!("Error: {error}"); exit(2); }
        }
        exit(0);
    }

    let cli_privilege_override = match runtime::extract_privilege_override(&mut args) {
        Ok(tool) => tool.map(str::to_string),
        Err(error) => { eprintln!("Error: {error}"); exit(2); }
    };

    match runtime::parse(&mut args) {
        Ok(policy) => scratch::initialize(policy),
        Err(error) => { eprintln!("Error: {error}"); exit(2); }
    }

    if is_clean_invocation(&args) && unsafe { libc::geteuid() } == 0 {
        eprintln!("Error: Do not run pacpin as root. Run pacpin as your user (e.g. `pin clean` or `pin clean --pkexec`); it will elevate privileges automatically.");
        exit(1);
    }

    if args
        .first()
        .map(|s| s == "-v" || s == "-V" || s == "--version")
        .unwrap_or(false)
    {
        print_version();
        exit(0);
    }

    if args
        .first()
        .map(|s| s == "-h" || s == "--help" || s == "help")
        .unwrap_or(false)
    {
        print_help();
        exit(0);
    }

    if args
        .first()
        .map(|s| s == "reset" || s == "config-reset")
        .unwrap_or(false)
        || (args.len() >= 2 && args[0] == "config" && args[1] == "reset")
    {
        let force = args
            .iter()
            .any(|a| a == "-f" || a == "--force" || a == "-y");
        let noconfirm = args.iter().any(|a| a == "--noconfirm");
        cmd_reset(force, noconfirm);
        exit(0);
    }

    if args.as_slice() == ["config", "init"] {
        let path = config::get_config_path();
        match manager_config::initialize(path.parent().unwrap_or_else(|| Path::new("."))) {
            Ok(created) => {
                for path in &created { println!(":: Created {path}"); }
                if created.is_empty() { println!(":: Configuration files already exist; preserved them."); }
            }
            Err(error) => { eprintln!("Error: {error}"); exit(1); }
        }
        exit(0);
    }

    // Explicit operations must not create configuration as an onboarding side effect.
    let (mut config, just_initialized) = if args.is_empty() && !config::config_exists() && io::stdin().is_terminal() {
        match wizard::run(false) {
            Ok(Some(cfg)) => (cfg, true),
            Ok(None) => exit(0),
            Err(error) => { eprintln!("Error: {error}"); exit(1); }
        }
    } else {
        (load_config(), false)
    };

    if let Some(tool) = cli_privilege_override {
        config.options.privilege_tool = tool;
    }

    privilege::initialize(&config.options.privilege_tool, &args);
    progress::begin_transaction();

    if just_initialized && args.is_empty() {
        exit(0);
    }

    if args.is_empty() {
        print_help();
        exit(0);
    }

    let cmd = &args[0];
    if args.as_slice() == ["config", "validate"] {
        println!(":: Configuration valid: {} manager definitions, {} Sources recipes, {} hooks.", config.manager_file.managers.len(), config.sources.len(), config.hooks.len());
    } else if cmd == "manager" {
        let Some(name) = args.get(1) else { eprintln!("Usage: pin manager NAME --FLAG [--dry-run] [--noconfirm]"); exit(2); };
        let flags: Vec<String> = args[2..].iter().filter(|arg| !matches!(arg.as_str(), "--dry-run" | "-n" | "--noconfirm")).cloned().collect();
        let commands = match integrations::IntegrationsManager::configured_actions(&config, name, &flags, false) {
            Ok(commands) => commands, Err(error) => { eprintln!("Error: {error}"); exit(2); }
        };
        for command in commands { println!(":: {name} action: {command}"); }
        if args.iter().any(|arg| matches!(arg.as_str(), "--dry-run" | "-n")) { return; }
        if !args.iter().any(|arg| arg == "--noconfirm") {
            use std::io::Write;
            if !io::stdin().is_terminal() { eprintln!("A terminal confirmation or --noconfirm is required"); exit(2); }
            print!(":: Run these configured actions? [y/N] "); let _ = io::stdout().flush();
            let mut answer = String::new();
            if io::stdin().read_line(&mut answer).is_err() || !answer.trim().eq_ignore_ascii_case("y") { println!(":: Actions cancelled."); return; }
        }
        match integrations::IntegrationsManager::configured_actions(&config, name, &flags, true) {
            Ok(_) => println!("✔ {name}: configured actions completed."), Err(error) => { eprintln!("Error: {error}"); exit(1); }
        }
    } else if cmd == "repos" || cmd == "repo-order" || cmd == "priority" {
        cmd_repos(config);
        exit(0);
    } else if cmd == "capture" {
        if let Err(error) = cmd_capture(config, &args[1..]) {
            eprintln!("{}", format!("Error: {}", error).red());
            exit(1);
        }
    } else if cmd == "rebuild" {
        if let Err(error) = cmd_rebuild(config, &args[1..]) {
            eprintln!("{}", format!("Error: {}", error).red());
            exit(1);
        }
    } else if cmd == "check" || cmd == "-Qu" || (cmd.starts_with("-Q") && cmd.contains('u')) {
        let mut check_args = Vec::new();
        if cmd.starts_with("-Q") && cmd.contains('q') {
            check_args.push("-q".to_string());
        }
        check_args.extend_from_slice(&args[1..]);
        cmd_check(&config, &check_args);
    } else if is_refresh_only_invocation(&args) {
        if args.iter().any(|arg| arg == "-n" || arg == "--dry-run") {
            println!(":: Would refresh package databases only: {} pacman {}", config.options.privilege_tool, args.join(" "));
            exit(0);
        }
        match pm::forward_pacman(&config, &args, true) {
            Ok(code) => exit(code),
            Err(error) => {
                eprintln!("{}", format!("Database refresh failed: {error}").red());
                exit(1);
            }
        }
    } else if is_upgrade_invocation(&args) {
        let (targets, extra_flags) = parse_pacman_cli_args(&args[1..]);
        let dry_run = args.iter().any(|a| a == "-n" || a == "--dry-run")
            || args
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--") && a[1..].contains('n'));
        let refresh = args.iter().any(|a| a == "-y" || a == "--refresh")
            || args
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--") && a[1..].contains('y'));
        let noconfirm = args.iter().any(|a| a == "--noconfirm");
        let autoremove = args
            .iter()
            .any(|a| a == "-c" || a == "--clean" || a == "--autoremove")
            || args
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--") && a[1..].contains('c'));
        cmd_upgrade(
            &config,
            dry_run,
            refresh,
            noconfirm,
            autoremove,
            &extra_flags,
            &targets,
        );
    } else if cmd == "search"
        || cmd == "-Ss"
        || (cmd.starts_with("-S")
            && (cmd.contains('s') || args.iter().any(|a| a == "-s" || a == "--search")))
    {
        let query_args: Vec<String> = if cmd.starts_with("-S") {
            let mut q = Vec::new();
            if cmd.len() > 3 && cmd.starts_with("-Ss") {
                q.push(cmd[3..].to_string());
            }
            q.extend(
                args.iter()
                    .skip(1)
                    .filter(|a| *a != "-s" && *a != "--search" && !a.starts_with("-Ss"))
                    .cloned(),
            );
            q
        } else {
            args.iter().skip(1).cloned().collect()
        };
        cmd_search(&config, &query_args);
    } else if cmd == "info"
        || cmd == "-Si"
        || (cmd.starts_with("-S")
            && (cmd.contains('i') || args.iter().any(|a| a == "-i" || a == "--info")))
    {
        let pkg_args: Vec<String> = if cmd.starts_with("-S") {
            let mut p = Vec::new();
            if cmd.len() > 3 && cmd.starts_with("-Si") {
                p.push(cmd[3..].to_string());
            }
            p.extend(
                args.iter()
                    .skip(1)
                    .filter(|a| *a != "-i" && *a != "--info" && !a.starts_with("-Si"))
                    .cloned(),
            );
            p
        } else {
            args.iter().skip(1).cloned().collect()
        };
        cmd_info(&config, &pkg_args);
    } else if is_clean_invocation(&args) {
        let (full, extra_args) = parse_clean_args(&args);
        cmd_clean(&config, full, &extra_args);
    } else if cmd.starts_with("-Sw")
        || (cmd.starts_with("-S") && args.iter().any(|a| a == "-w" || a == "--downloadonly"))
    {
        match pm::forward_pacman(&config, &args, true) {
            Ok(code) => exit(code),
            Err(e) => {
                eprintln!("{}", format!("Failed to run {} pacman: {}", config.options.privilege_tool, e).red());
                exit(1);
            }
        }
    } else if cmd == "-Sp"
        || (cmd.starts_with("-S") && cmd.contains('p'))
        || (cmd.starts_with("-S")
            && args
                .iter()
                .any(|a| a == "-p" || a == "--print-uris" || a == "--print"))
    {
        let (targets, _) = parse_pacman_cli_args(&args[1..]);
        cmd_print_uris(&config, &targets);
        exit(0);
    } else if cmd == "-Sl"
        || (cmd.starts_with("-S") && cmd.contains('l'))
        || (cmd.starts_with("-S") && args.iter().any(|a| a == "-l" || a == "--list"))
    {
        let (repos, _) = parse_pacman_cli_args(&args[1..]);
        cmd_sync_list(&config, &repos);
        exit(0);
    } else if cmd == "-Sg"
        || (cmd.starts_with("-S") && cmd.contains('g'))
        || (cmd.starts_with("-S") && args.iter().any(|a| a == "-g" || a == "--groups"))
    {
        let (groups, _) = parse_pacman_cli_args(&args[1..]);
        cmd_sync_groups(&config, &groups);
        exit(0);
    } else if cmd.starts_with("-T") {
        match pm::forward_pacman(&config, &args, false) {
            Ok(code) => exit(code),
            Err(e) => {
                eprintln!("{}", format!("Failed to run pacman: {}", e).red());
                exit(1);
            }
        }
    } else if cmd == "-S"
        || cmd == "install"
        || (cmd.starts_with("-S") && {
            let (t, _) = parse_pacman_cli_args(&args[1..]);
            !t.is_empty()
        })
    {
        let (targets, mut flags) = parse_pacman_cli_args(&args[1..]);
        if cmd.len() > 2 && cmd.starts_with("-S") {
            let subflags = &cmd[2..];
            if subflags.contains("yy") {
                flags.push("-yy".to_string());
            } else if subflags.contains('y') {
                flags.push("-y".to_string());
            }
            if subflags.contains("uu") {
                flags.push("-uu".to_string());
            } else if subflags.contains('u') {
                flags.push("-u".to_string());
            }
            if subflags.contains('n') {
                flags.push("-n".to_string());
            }
        }
        cmd_install(config, &targets, &flags);
    } else if cmd == "remove" || cmd == "rm" || cmd.starts_with("-R") {
        let is_friendly = cmd == "remove" || cmd == "rm";
        let sub_args = if is_friendly {
            args[1..].to_vec()
        } else {
            args.clone()
        };
        cmd_remove(config, &sub_args, is_friendly);
    } else if cmd.starts_with("-Q") {
        match pm::forward_pacman(&config, &args, false) {
            Ok(code) => exit(code),
            Err(e) => {
                eprintln!("{}", format!("Failed to run pacman: {}", e).red());
                exit(1);
            }
        }
    } else if cmd.starts_with("-F") {
        let needs_sudo = pacman_files_needs_sudo(&args);
        match pm::forward_pacman(&config, &args, needs_sudo) {
            Ok(code) => exit(code),
            Err(e) => {
                eprintln!("{}", format!("Failed to run pacman: {}", e).red());
                exit(1);
            }
        }
    } else if cmd.starts_with("-U") || cmd.starts_with("-D") {
        match pm::forward_pacman(&config, &args, true) {
            Ok(code) => exit(code),
            Err(e) => {
                eprintln!("{}", format!("Failed to run {} pacman: {}", config.options.privilege_tool, e).red());
                exit(1);
            }
        }
    } else if cmd == "history" {
        let tx_id = args.get(1).and_then(|s| s.parse::<usize>().ok());
        cmd_history(tx_id);
    } else if cmd == "rollback" {
        let mut tx_id = None;
        let dry_run = args.iter().any(|a| a == "-n" || a == "--dry-run");
        let allow_partial = args
            .iter()
            .any(|a| a == "--allow-partial" || a == "--partial" || a == "-f" || a == "--force");
        let noconfirm = args.iter().any(|a| a == "--noconfirm");
        for a in &args[1..] {
            if let Ok(id) = a.parse::<usize>() {
                tx_id = Some(id);
            }
        }
        TransactionJournal::rollback(tx_id, dry_run, allow_partial, noconfirm, &config.options.privilege_tool);
    } else if cmd == "run" {
        cmd_run(&args[1..]);
    } else if cmd == "try" {
        cmd_try(&args[1..]);
    } else if cmd == "needrestart" || cmd == "restart-check" {
        restart::RestartInspector::print_restart_advisory(&[], &config.options.privilege_tool);
        exit(0);
    } else if cmd == "delay" {
        if args.len() < 3 || args[2].parse::<u32>().is_err() {
            eprintln!(
                "{}",
                "Error: 'pacpin delay' requires <pkg> and <days>.".red()
            );
            eprintln!("Usage: pacpin delay <pkg> <days> (e.g. pacpin delay openssl 3)");
            exit(1);
        }
        cmd_delay(&args[1], args[2].parse::<u32>().unwrap());
    } else if cmd == "undelay" {
        if args.len() < 2 {
            eprintln!("{}", "Error: 'pacpin undelay' requires <pkg>.".red());
            eprintln!("Usage: pacpin undelay <pkg>");
            exit(1);
        }
        cmd_undelay(&args[1]);
    } else if cmd == "list" {
        cmd_list(&config);
    } else if cmd == "orphans" || cmd == "autoremove" {
        let clean = args.iter().any(|a| a == "-c" || a == "--clean") || cmd == "autoremove";
        let noconfirm = args.iter().any(|a| a == "--noconfirm");
        cmd_orphans(&config, clean, noconfirm);
    } else if cmd == "keep" || cmd == "adopt" {
        cmd_keep(&config, &args[1..]);
    } else if cmd == "pin" || cmd == "add-pin" || cmd == "add-pins" {
        let mut known_repos = AlpmManager::discover_repos();
        known_repos.push("aur".to_string());
        known_repos.extend(config.sources.keys().cloned());

        match parse_pin_args(&args[1..], &known_repos) {
            Ok((repo, patterns)) => cmd_pin(config, repo, patterns),
            Err(e) => {
                eprintln!("{}", format!("Error: {}", e).red());
                eprintln!("Usage: pacpin pin <repo> <pkg1> [pkg2...]");
                eprintln!("       pacpin pin <repo> -f <packages.txt>");
                eprintln!("       pacpin pin <pkg> <repo>");
                exit(1);
            }
        }
    } else if cmd == "unpin" {
        if args.len() < 2 {
            eprintln!("{}", "Error: 'pacpin unpin' requires a pattern.".red());
            eprintln!("Usage: pacpin unpin <pattern> (e.g. pacpin unpin 'linux-firmware*')");
            exit(1);
        }
        cmd_unpin(&args[1]);
    } else if cmd.starts_with('-') {
        let helper = &config.options.helper;
        let status = if is_command_available(helper) {
            Command::new(helper).args(&args).status()
        } else {
            Command::new("pacman").args(&args).status()
        };
        match status {
            Ok(s) => exit(s.code().unwrap_or(1)),
            Err(e) => {
                eprintln!("{}", format!("Failed to run command: {}", e).red());
                exit(1);
            }
        }
    } else {
        eprintln!("{}", format!("Unknown command: {}", cmd).red());
        eprintln!("Did you mean:");
        eprintln!("  pacpin search {}  (search repositories and AUR)", cmd);
        eprintln!("  pacpin -S {}      (install package)", cmd);
        eprintln!("Run 'pacpin --help' for full command usage.");
        exit(1);
    }
}
