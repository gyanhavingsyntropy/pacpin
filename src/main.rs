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

mod alias;
mod aur;
pub mod commands;
mod config;
mod db;
mod download;
pub mod pm;
mod integrations;
mod journal;
mod orphans;
mod repo_menu;
mod resolver;
mod restart;
mod sandbox;
mod tui_select;
mod ui;
mod utils;
mod wizard;

use colored::Colorize;
use commands::*;
pub use commands::upgrade::parse_install_flags;
use config::{load_config, save_config, Config};
use db::AlpmManager;
use journal::TransactionJournal;
use resolver::ResolverEngine;
use std::collections::BTreeMap;
use std::env;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::{exit, Command};
use ui::print_banner;


pub fn is_command_available(cmd: &str) -> bool {
    let p = Path::new(cmd);
    if p.is_absolute() {
        return crate::utils::is_executable_file(p);
    }

    if let Ok(path) = env::var("PATH") {
        for dir in env::split_paths(&path) {
            let candidate = dir.join(cmd);
            if crate::utils::is_executable_file(&candidate) {
                return true;
            }
        }
    }
    false
}

pub fn find_matching_pin(
    pkg: &str,
    pins: &BTreeMap<String, String>,
) -> Option<(String, String, bool)> {
    let glob_pins = ResolverEngine::compile_glob_pins(pins);
    ResolverEngine::find_matching_pin_rule(pkg, pins, &glob_pins)
}

pub(crate) fn check_and_prompt_smart_unpin(mut config: Config, removed_pkgs: &[String], noconfirm: bool) {
    let mut config_dirty = false;
    let interactive = io::stdin().is_terminal();

    for pkg in removed_pkgs {
        if let Some((pattern, repo, is_exact)) = find_matching_pin(pkg, &config.pins) {
            let should_remove = if noconfirm {
                // A non-interactive removal may clean up a direct pin, but a
                // wildcard rule can apply to many packages and must survive.
                is_exact
            } else if interactive {
                if is_exact {
                    print!(
                        "\n:: Package '{}' is pinned to repository [{}]. Remove this pin rule from config? [Y/n] ",
                        pkg.cyan().bold(),
                        repo.green()
                    );
                } else {
                    print!(
                        "\n:: Package '{}' matched wildcard pin '{}' ➔ [{}]. Remove this pin rule from config? [Y/n] ",
                        pkg.cyan().bold(),
                        pattern.yellow(),
                        repo.green()
                    );
                }
                io::stdout().flush().unwrap();
                let mut input = String::new();
                if io::stdin().read_line(&mut input).is_ok() {
                    let trimmed = input.trim().to_lowercase();
                    trimmed.is_empty() || trimmed == "y" || trimmed == "yes"
                } else {
                    false
                }
            } else {
                false
            };

            if should_remove {
                config.pins.remove(&pattern);
                println!(
                    "{}",
                    format!("✔ Removed pin rule '{}' ➔ [{}].", pattern, repo).green()
                );
                config_dirty = true;
            }
        }

        if let Some(days) = config.delay.get(pkg).copied() {
            let should_remove = if noconfirm {
                true
            } else if interactive {
                print!(
                    "\n:: Package '{}' has an active {}-day stability delay. Remove this delay rule? [Y/n] ",
                    pkg.cyan().bold(),
                    days
                );
                io::stdout().flush().unwrap();
                let mut input = String::new();
                if io::stdin().read_line(&mut input).is_ok() {
                    let trimmed = input.trim().to_lowercase();
                    trimmed.is_empty() || trimmed == "y" || trimmed == "yes"
                } else {
                    false
                }
            } else {
                false
            };

            if should_remove {
                config.delay.remove(pkg);
                println!(
                    "{}",
                    format!("✔ Removed {}-day stability delay on '{}'.", days, pkg).green()
                );
                config_dirty = true;
            }
        }
    }

    if config_dirty {
        if let Err(e) = save_config(&config) {
            eprintln!(
                "{}",
                format!("Warning: Failed to save updated config: {}", e).yellow()
            );
        } else {
            println!("{}", ":: Configuration updated successfully.".dimmed());
        }
    }
}


fn print_help() {
    print_banner();
    println!("\nUsage: pacpin <command> [options]");
    println!();
    println!("{}", "Core Package Operations:".bold());
    println!("  check, -Qu                   Check pending updates & verify pin protections");
    println!("  upgrade, -Syu [flags]        Execute safe upgrade with True Resolver");
    println!("    Flags:");
    println!("      -n, --dry-run            Show transaction summary without executing");
    println!("      -y, --refresh            Refresh sync databases (sudo pacman -Sy) first");
    println!("      -c, --clean              Prompt to clean all orphans during upgrade");
    println!("      --noconfirm              Bypass interactive confirmation prompt");
    println!("  -S [flags] [repo/]pkg        Install packages (auto-pins repo/pkg with companion cascade)");
    println!("  -Sp, --print-uris <pkg...>   Print package and dependency download URIs");
    println!("  -Sl, --list [repo...]        List packages in sync repositories");
    println!("  -Sg, --groups [group...]     List package groups or packages in a group");
    println!("  remove, rm <pkg...>          Remove package(s) and unneeded dependencies (-Rns + smart unpin)");
    println!("  search, -Ss <query...>       Search official repositories and AUR simultaneously");
    println!("  info, -Si <pkg...>           Show detailed package information (-Si / AUR)");
    println!("  clean, -Sc                   Clean pacman and AUR build cache");
    println!();
    println!("{}", "Declarative Rules & Configuration:".bold());
    println!(
        "  repos                        Interactive BIOS-style repository search priority menu"
    );
    println!(
        "  pin <repo> <pkg...>          Lock package(s) or wildcards to a designated repository"
    );
    println!(
        "  pin <repo> -f <file>         Batch pin packages from a text file (one package per line)"
    );
    println!("  unpin <pkg>                  Remove a repository pin rule");
    println!("  delay <pkg> <days>           Set a stability delay buffer on a package");
    println!("  undelay <pkg>                Remove a package stability delay rule");
    println!(
        "  list                         List active pins, exclusions, delays, and repo priority"
    );
    println!("  reset [-f]                   Reset all configurations, pins, and delays to default (-f force)");
    println!("  init [--reset]               Interactive onboarding wizard (use --reset for clean slate)");
    println!();
    println!("{}", "System Maintenance & Hygiene:".bold());
    println!(
        "  orphans [-c]                 Inspect or remove orphaned dependencies (-c to clean)"
    );
    println!("  autoremove                   Alias for 'pacpin orphans -c'");
    println!("  keep, adopt <pkg...>         Mark package(s) as explicitly installed (silences orphan warnings)");
    println!("  needrestart                  Inspect processes holding outdated libraries or kernel in RAM");
    println!();
    println!("{}", "Power Tools & Ephemeral Execution:".bold());
    println!("  run [options] [repo/]pkg     Run package directly on host without installing (like 'nix run')");
    println!("  try [options] [repo/]pkg     Run package in isolated ephemeral sandbox (air-gapped bwrap)");
    println!(
        "  history [id]                 View transaction history timeline or inspect a transaction"
    );
    println!("  rollback [id] [-n]           Restore previous package versions from cache");
    println!();
    println!("{}", "Pacman & AUR Drop-in Aliases:".bold());
    println!("  pacpin -Syu                  Alias for 'pacpin upgrade -y'");
    println!("  pacpin -Qu                   Alias for 'pacpin check'");
    println!("  pacpin -Ss <query...>        Search repositories and AUR");
    println!("  pacpin -Si <pkg...>          Show remote/AUR package metadata");
    println!("  pacpin -Qi <pkg...>          Show locally installed package info");
    println!("  pacpin -R / -Rns <pkg...>    Remove packages (with smart pin cleanup)");
    println!("  pacpin -Q / -Qo / -Ql        Pacman query and package inspection");
    println!("  pacpin -U <pkg.tar.zst>      Install local package archive");
    println!("  pacpin -F / -Fy              Pacman files database operations");
    println!("  pacpin -T <deps...>          Check dependency requirements");
    println!();
}

fn print_version() {
    println!("pacpin {}", env!("CARGO_PKG_VERSION"));
    println!("Copyright (C) 2026 Gyan <330976822+gyanhavingsyntropy@users.noreply.github.com>");
    println!("License GPLv3+: GNU GPL version 3 or later <https://gnu.org/licenses/gpl.html>");
    println!("This is free software: you are free to change and redistribute it.");
    println!("There is NO WARRANTY, to the extent permitted by law.");
}

pub fn parse_pacman_cli_args(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut targets = Vec::new();
    let mut flags = Vec::new();
    let mut after_double_dash = false;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        if after_double_dash {
            targets.push(arg.clone());
            i += 1;
            continue;
        }

        if arg == "--" {
            after_double_dash = true;
            i += 1;
            continue;
        }

        let takes_arg = arg == "--ignore"
            || arg == "--ignoregroup"
            || arg == "--config"
            || arg == "--cachedir"
            || arg == "--root"
            || arg == "-r"
            || arg == "--dbpath"
            || arg == "-b"
            || arg == "--logfile"
            || arg == "--gpgdir"
            || arg == "--hookdir"
            || arg == "--overwrite"
            || arg == "--assume-installed"
            || arg == "--color"
            || arg == "--arch"
            || arg == "--print-format";

        if arg == "--wait" {
            flags.push(arg.clone());
            if i + 1 < args.len() && args[i + 1].parse::<u64>().is_ok() {
                flags.push(args[i + 1].clone());
                i += 1;
            }
        } else if takes_arg {
            flags.push(arg.clone());
            if i + 1 < args.len() {
                flags.push(args[i + 1].clone());
                i += 1;
            }
        } else if arg.starts_with("--ignore=")
            || arg.starts_with("--ignoregroup=")
            || arg.starts_with("--config=")
            || arg.starts_with("--cachedir=")
            || arg.starts_with("--root=")
            || arg.starts_with("--dbpath=")
            || arg.starts_with("--logfile=")
            || arg.starts_with("--gpgdir=")
            || arg.starts_with("--hookdir=")
            || arg.starts_with("--overwrite=")
            || arg.starts_with("--assume-installed=")
            || arg.starts_with("--color=")
            || arg.starts_with("--arch=")
            || arg.starts_with("--print-format=")
            || arg.starts_with("--wait=")
            || arg.starts_with('-')
        {
            flags.push(arg.clone());
        } else {
            targets.push(arg.clone());
        }

        i += 1;
    }

    (targets, flags)
}

pub fn is_upgrade_invocation(args: &[String]) -> bool {
    if args.is_empty() {
        return false;
    }
    let first = &args[0];
    if first == "upgrade" || first == "up" {
        return true;
    }
    if !first.starts_with("-S") {
        return false;
    }

    let (targets, flags_list) = parse_pacman_cli_args(&args[1..]);
    if !targets.is_empty() {
        return false;
    }

    // Collect all short flag characters across first arg and flags_list
    let mut flags = std::collections::HashSet::new();
    for a in std::iter::once(first).chain(flags_list.iter()) {
        if a.starts_with('-') && !a.starts_with("--") {
            for c in a[1..].chars() {
                flags.insert(c);
            }
        }
    }

    // Reject if other -S sub-operations are requested:
    // s (search), i (info), w (downloadonly), p (print-uris), l (list), g (groups), c (clean without u)
    if flags.contains(&'s')
        || flags.contains(&'i')
        || flags.contains(&'w')
        || flags.contains(&'p')
        || flags.contains(&'l')
        || flags.contains(&'g')
        || (flags.contains(&'c') && !flags.contains(&'u'))
        || flags_list.iter().any(|a| {
            a == "--search"
                || a == "--info"
                || a == "--downloadonly"
                || a == "--print"
                || a == "--print-uris"
                || a == "--list"
                || a == "--groups"
        })
    {
        return false;
    }

    // It's an upgrade if flags contains 'u' or 'y' or long options
    flags.contains(&'u')
        || flags.contains(&'y')
        || flags_list
            .iter()
            .any(|a| a == "--sysupgrade" || a == "--refresh")
}

pub fn pacman_files_needs_sudo(args: &[String]) -> bool {
    let (_, flags) = parse_pacman_cli_args(args);
    flags.iter().any(|a| {
        if a == "--refresh" {
            true
        } else if a.starts_with('-') && !a.starts_with("--") {
            a.contains('y')
        } else {
            false
        }
    })
}

fn main() {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    sandbox::init_signal_handlers();
    sandbox::clean_stale_sandboxes();

    if std::env::var_os("NO_COLOR").is_some()
        || (!io::stdout().is_terminal() && std::env::var_os("CLICOLOR_FORCE").is_none())
    {
        colored::control::set_override(false);
    }

    let args: Vec<String> = env::args().skip(1).collect();

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

    if args
        .first()
        .map(|s| s == "init" || s == "setup")
        .unwrap_or(false)
    {
        let reset = args.iter().any(|a| a == "-r" || a == "--reset");
        wizard::run_first_launch_wizard(true, reset);
        exit(0);
    }

    // Auto-launch wizard if no config exists and running in an interactive terminal
    let (config, just_initialized) = if !config::config_exists() && io::stdin().is_terminal() {
        if let Some(cfg) = wizard::run_first_launch_wizard(false, false) {
            (cfg, true)
        } else {
            (load_config(), false)
        }
    } else {
        (load_config(), false)
    };

    if just_initialized && args.is_empty() {
        exit(0);
    }

    if args.is_empty() {
        print_help();
        exit(0);
    }

    let cmd = &args[0];
    if cmd == "repos" || cmd == "repo-order" || cmd == "priority" {
        cmd_repos(config);
        exit(0);
    } else if cmd == "check" || cmd == "-Qu" || (cmd.starts_with("-Q") && cmd.contains('u')) {
        let mut check_args = Vec::new();
        if cmd.starts_with("-Q") && cmd.contains('q') {
            check_args.push("-q".to_string());
        }
        check_args.extend_from_slice(&args[1..]);
        cmd_check(&config, &check_args);
    } else if is_upgrade_invocation(&args) {
        let (_targets, extra_flags) = parse_pacman_cli_args(&args[1..]);
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
    } else if cmd == "clean"
        || cmd == "clean-cache"
        || cmd == "-Sc"
        || cmd == "-Scc"
        || (cmd.starts_with("-S")
            && (cmd.contains('c') || args.iter().any(|a| a == "-c" || a == "--clean"))
            && !is_upgrade_invocation(&args)
            && !args.iter().skip(1).any(|a| !a.starts_with('-')))
    {
        let extra_args: Vec<String> = args
            .iter()
            .skip(1)
            .filter(|a| *a != "-c" && *a != "--clean" && !a.starts_with("-Sc"))
            .cloned()
            .collect();
        cmd_clean(&config, &extra_args);
    } else if cmd.starts_with("-Sw")
        || (cmd.starts_with("-S") && args.iter().any(|a| a == "-w" || a == "--downloadonly"))
    {
        match pm::forward_pacman(&args, true) {
            Ok(code) => exit(code),
            Err(e) => {
                eprintln!("{}", format!("Failed to run sudo pacman: {}", e).red());
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
        match pm::forward_pacman(&args, false) {
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
        match pm::forward_pacman(&args, false) {
            Ok(code) => exit(code),
            Err(e) => {
                eprintln!("{}", format!("Failed to run pacman: {}", e).red());
                exit(1);
            }
        }
    } else if cmd.starts_with("-F") {
        let needs_sudo = pacman_files_needs_sudo(&args);
        match pm::forward_pacman(&args, needs_sudo) {
            Ok(code) => exit(code),
            Err(e) => {
                eprintln!("{}", format!("Failed to run pacman: {}", e).red());
                exit(1);
            }
        }
    } else if cmd.starts_with("-U") || cmd.starts_with("-D") {
        match pm::forward_pacman(&args, true) {
            Ok(code) => exit(code),
            Err(e) => {
                eprintln!("{}", format!("Failed to run sudo pacman: {}", e).red());
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
        TransactionJournal::rollback(tx_id, dry_run, allow_partial, noconfirm);
    } else if cmd == "run" {
        let mut opts = sandbox::TryOptions::for_run();
        let mut positional = Vec::new();
        let mut pass_through = Vec::new();
        let mut after_delimiter = false;

        let mut i = 1;
        while i < args.len() {
            let a = &args[i];
            if after_delimiter {
                pass_through.push(a.clone());
                i += 1;
                continue;
            }
            if a == "--" {
                after_delimiter = true;
                i += 1;
                continue;
            }
            if a == "--sandbox" || a == "--sandboxed" {
                opts.no_sandbox = false;
                opts.is_run_mode = false;
            } else if a == "--allow-unverified" {
                opts.allow_unverified = true;
            } else if a == "--noconfirm" || a == "-y" {
                opts.noconfirm = true;
            } else if a == "--bin" {
                if i + 1 < args.len() {
                    opts.bin = Some(args[i + 1].clone());
                    i += 1;
                }
            } else if let Some(stripped) = a.strip_prefix("--bin=") {
                opts.bin = Some(stripped.to_string());
            } else if positional.is_empty() && !a.starts_with('-') {
                positional.push(a.clone());
            } else {
                pass_through.push(a.clone());
            }
            i += 1;
        }

        if positional.is_empty() {
            eprintln!(
                "{}",
                "Error: 'pacpin run' requires a package target."
                    .red()
                    .bold()
            );
            eprintln!("Usage: pacpin run [options] [repo/]package [arguments...]");
            eprintln!("Runs an official, AUR, Flatpak, or Nix package directly on host without permanent installation.");
            eprintln!();
            eprintln!("Options:");
            eprintln!(
                "  --sandbox              Run in Bubblewrap container (unsandboxed by default)"
            );
            eprintln!("  --bin <name>           Specify exact executable binary name if package provides multiple");
            eprintln!("  --allow-unverified     Proceed even if repository metadata lacks a SHA256 checksum");
            eprintln!();
            eprintln!("Examples:");
            eprintln!("  pacpin run fastfetch");
            eprintln!("  pacpin run yt-dlp 'https://youtube.com/watch?v=...'");
            eprintln!("  pacpin run ffmpeg -i screencast.mkv output.mp4");
            eprintln!("  pacpin run jq . package.json");
            exit(1);
        }

        let pkg = &positional[0];
        sandbox::cmd_try(pkg, &pass_through, &opts);
    } else if cmd == "try" {
        let mut opts = sandbox::TryOptions::default();
        let mut positional = Vec::new();
        let mut pass_through = Vec::new();
        let mut after_delimiter = false;

        let mut i = 1;
        while i < args.len() {
            let a = &args[i];
            if after_delimiter {
                pass_through.push(a.clone());
                i += 1;
                continue;
            }
            if a == "--" {
                after_delimiter = true;
                i += 1;
                continue;
            }
            if a == "--no-sandbox" || a == "--bare" || a == "--unsandboxed" {
                opts.no_sandbox = true;
            } else if a == "--net" || a == "--network" || a == "--share-net" {
                opts.share_net = true;
            } else if a == "--rw" || a == "--rw-cwd" || a == "--write-cwd" {
                opts.rw_cwd = true;
            } else if a == "--gui" {
                opts.gui = true;
            } else if a == "--audio" {
                opts.audio = true;
            } else if a == "--allow-unverified" {
                opts.allow_unverified = true;
            } else if a == "--noconfirm" || a == "-y" {
                opts.noconfirm = true;
            } else if a == "--bin" {
                if i + 1 < args.len() {
                    opts.bin = Some(args[i + 1].clone());
                    i += 1;
                }
            } else if let Some(stripped) = a.strip_prefix("--bin=") {
                opts.bin = Some(stripped.to_string());
            } else if positional.is_empty() && !a.starts_with('-') {
                positional.push(a.clone());
            } else {
                pass_through.push(a.clone());
            }
            i += 1;
        }

        if positional.is_empty() {
            eprintln!(
                "{}",
                "Error: 'pacpin try' requires a package target."
                    .red()
                    .bold()
            );
            eprintln!("Usage: pacpin try [options] [repo/]package [arguments...]");
            eprintln!("Runs native Arch packages in a Bubblewrap container; Flatpak uses its own sandbox.");
            eprintln!();
            eprintln!("Options:");
            eprintln!("  --no-sandbox, --bare   Bypass Bubblewrap containerization and run directly on host");
            eprintln!("  --net, --network       Share host network access (unshared by default)");
            eprintln!("  --rw, --write-cwd      Mount current working directory read-write (read-only by default)");
            eprintln!("  --gui                  Grant X11/Wayland and DRI access for graphical applications");
            eprintln!("  --audio                Grant /dev/snd access for audio playback");
            eprintln!("  --bin <name>           Specify exact executable binary name if package provides multiple");
            eprintln!("  --allow-unverified     Proceed even if repository metadata lacks a SHA256 checksum");
            eprintln!();
            eprintln!("Examples:");
            eprintln!("  pacpin try jq . foo.json");
            eprintln!("  pacpin try --gui flatpak/org.gnome.Calculator");
            eprintln!("  pacpin run nix/ripgrep -i 'foo'  # Nix executes on the host");
            exit(1);
        }

        let pkg = &positional[0];
        sandbox::cmd_try(pkg, &pass_through, &opts);
    } else if cmd == "needrestart" || cmd == "restart-check" {
        restart::RestartInspector::print_restart_advisory(&[]);
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
        cmd_delay(config, &args[1], args[2].parse::<u32>().unwrap());
    } else if cmd == "undelay" {
        if args.len() < 2 {
            eprintln!("{}", "Error: 'pacpin undelay' requires <pkg>.".red());
            eprintln!("Usage: pacpin undelay <pkg>");
            exit(1);
        }
        cmd_undelay(config, &args[1]);
    } else if cmd == "list" {
        cmd_list(&config);
    } else if cmd == "orphans" || cmd == "autoremove" {
        let clean = args.iter().any(|a| a == "-c" || a == "--clean") || cmd == "autoremove";
        let noconfirm = args.iter().any(|a| a == "--noconfirm");
        cmd_orphans(clean, noconfirm);
    } else if cmd == "keep" || cmd == "adopt" {
        if args.len() < 2 {
            eprintln!(
                "{}",
                "Error: 'pacpin keep' requires at least one package name.".red()
            );
            eprintln!("Usage: pacpin keep <pkg1> [pkg2...]");
            exit(1);
        }
        let pkgs = &args[1..];
        println!(
            "{} Marking {} package(s) as explicitly installed (sudo pacman -D --asexplicit)...",
            "::".cyan(),
            pkgs.len()
        );
        let status = Command::new("sudo")
            .arg("pacman")
            .arg("-D")
            .arg("--asexplicit")
            .args(pkgs)
            .status();
        match status {
            Ok(s) if s.success() => {
                println!("{}", "✔ Package(s) successfully marked as explicit! They will no longer appear as orphans.".green());
            }
            Ok(s) => exit(s.code().unwrap_or(1)),
            Err(e) => {
                eprintln!("{}", format!("Failed to run pacman: {}", e).red());
                exit(1);
            }
        }
    } else if cmd == "pin" || cmd == "add-pin" || cmd == "add-pins" {
        let mut known_repos = AlpmManager::discover_repos();
        known_repos.push("aur".to_string());

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
        cmd_unpin(config, &args[1]);
    } else if cmd.starts_with('-') {
        let helper = &config.options.helper;
        let status = if is_command_available(helper) {
            Command::new(helper).args(&args).status()
        } else {
            Command::new("pacman").args(&args).status()
        };
        match status {
            Ok(s) => exit(s.code().unwrap_or(0)),
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

fn parse_pin_args(
    pin_args: &[String],
    known_repos: &[String],
) -> Result<(String, Vec<String>), String> {
    let mut file_patterns = Vec::new();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < pin_args.len() {
        let arg = &pin_args[i];
        if arg == "-f" || arg == "--file" {
            if i + 1 >= pin_args.len() {
                return Err("'-f/--file' requires a file path.".to_string());
            }
            let file_path = &pin_args[i + 1];
            match std::fs::read_to_string(file_path) {
                Ok(content) => {
                    for line in content.lines() {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() && !trimmed.starts_with('#') {
                            file_patterns.push(trimmed.to_string());
                        }
                    }
                }
                Err(e) => {
                    return Err(format!(
                        "Error reading package list file '{}': {}",
                        file_path, e
                    ));
                }
            }
            i += 2;
        } else {
            positional.push(arg.clone());
            i += 1;
        }
    }

    if positional.is_empty() {
        Err("'pacpin pin' requires a repository name.".to_string())
    } else if positional.len() == 1 {
        if file_patterns.is_empty() {
            return Err(
                "'pacpin pin' requires both a repository and at least one package.".to_string(),
            );
        }
        Ok((positional[0].clone(), file_patterns))
    } else if positional.len() == 2 && file_patterns.is_empty() {
        if known_repos.iter().any(|r| r == &positional[1])
            && !known_repos.iter().any(|r| r == &positional[0])
        {
            Ok((positional[1].clone(), vec![positional[0].clone()]))
        } else {
            Ok((positional[0].clone(), vec![positional[1].clone()]))
        }
    } else {
        let (r, mut p) = if known_repos.iter().any(|r| r == &positional[0]) {
            (positional[0].clone(), positional[1..].to_vec())
        } else if known_repos.iter().any(|r| r == positional.last().unwrap()) {
            let last_idx = positional.len() - 1;
            (
                positional[last_idx].clone(),
                positional[..last_idx].to_vec(),
            )
        } else {
            (positional[0].clone(), positional[1..].to_vec())
        };
        p.extend(file_patterns);
        Ok((r, p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_command_available() {
        assert!(is_command_available("sh") || is_command_available("bash"));
        assert!(!is_command_available("this_binary_does_not_exist_xyz123"));
    }

    #[test]
    fn test_is_command_available_permissions() {
        let temp_dir = std::env::temp_dir();
        let non_exec = temp_dir.join(format!("pacpin_non_exec_test_{}", std::process::id()));
        std::fs::write(&non_exec, b"#!/bin/sh\necho hi\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&non_exec).unwrap().permissions();
            perms.set_mode(0o644); // No executable bit
            std::fs::set_permissions(&non_exec, perms).unwrap();
        }
        assert!(!is_command_available(&non_exec.to_string_lossy()));
        let _ = std::fs::remove_file(&non_exec);
    }

    #[test]
    fn test_find_matching_pin_exact_and_glob() {
        let mut pins = BTreeMap::new();
        pins.insert("mesa".to_string(), "core".to_string());
        pins.insert("linux-*".to_string(), "extra".to_string());
        pins.insert("linux-firmware*".to_string(), "cachyos".to_string());

        assert_eq!(
            find_matching_pin("mesa", &pins),
            Some(("mesa".to_string(), "core".to_string(), true))
        );
        // Specificity tie-break: "linux-firmware*" must beat "linux-*" despite ASCII key order
        assert_eq!(
            find_matching_pin("linux-firmware-intel", &pins),
            Some(("linux-firmware*".to_string(), "cachyos".to_string(), false))
        );
        assert_eq!(
            find_matching_pin("linux-zen", &pins),
            Some(("linux-*".to_string(), "extra".to_string(), false))
        );
        assert_eq!(find_matching_pin("git", &pins), None);
    }

    #[test]
    fn test_parse_pin_args_legacy_syntax() {
        let known_repos = vec!["core".to_string(), "extra".to_string(), "aur".to_string()];
        let args = vec!["linux-firmware*".to_string(), "core".to_string()];
        let (repo, patterns) = parse_pin_args(&args, &known_repos).unwrap();
        assert_eq!(repo, "core");
        assert_eq!(patterns, vec!["linux-firmware*"]);
    }

    #[test]
    fn test_parse_pin_args_repo_first() {
        let known_repos = vec!["core".to_string(), "extra".to_string(), "aur".to_string()];
        let args = vec!["core".to_string(), "linux-firmware*".to_string()];
        let (repo, patterns) = parse_pin_args(&args, &known_repos).unwrap();
        assert_eq!(repo, "core");
        assert_eq!(patterns, vec!["linux-firmware*"]);
    }

    #[test]
    fn test_parse_pin_args_batch() {
        let known_repos = vec!["core".to_string(), "extra".to_string(), "aur".to_string()];
        let args = vec![
            "core".to_string(),
            "pkg1".to_string(),
            "pkg2".to_string(),
            "pkg3".to_string(),
        ];
        let (repo, patterns) = parse_pin_args(&args, &known_repos).unwrap();
        assert_eq!(repo, "core");
        assert_eq!(patterns, vec!["pkg1", "pkg2", "pkg3"]);
    }

    #[test]
    fn test_parse_pin_args_file() {
        let known_repos = vec!["core".to_string(), "extra".to_string(), "aur".to_string()];
        let tmp_file = std::env::temp_dir().join("pacpin_test_pkgs.txt");
        std::fs::write(&tmp_file, "pkgA\n# comment\npkgB\n\npkgC\n").unwrap();

        let args = vec![
            "core".to_string(),
            "-f".to_string(),
            tmp_file.to_str().unwrap().to_string(),
        ];
        let (repo, patterns) = parse_pin_args(&args, &known_repos).unwrap();
        assert_eq!(repo, "core");
        assert_eq!(patterns, vec!["pkgA", "pkgB", "pkgC"]);

        let _ = std::fs::remove_file(&tmp_file);
    }

    #[test]
    fn test_pure_orphan_filtering() {
        use crate::db::OrphanPackage;
        let orphans = [
            OrphanPackage {
                name: "pure-orphan".to_string(),
                version: "1.0".to_string(),
                desc: "test".to_string(),
                isize: 100,
                optional_for: Vec::new(),
                dropped_by: Vec::new(),
                is_projected: false,
            },
            OrphanPackage {
                name: "opt-plugin".to_string(),
                version: "1.0".to_string(),
                desc: "test plugin".to_string(),
                isize: 200,
                optional_for: vec!["ffmpeg".to_string()],
                dropped_by: Vec::new(),
                is_projected: false,
            },
        ];

        let pure: Vec<String> = orphans
            .iter()
            .filter(|o| o.optional_for.is_empty())
            .map(|o| o.name.clone())
            .collect();

        assert_eq!(pure, vec!["pure-orphan"]);
    }

    #[test]
    fn test_is_upgrade_invocation() {
        let to_vec = |slice: &[&str]| slice.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Valid upgrade forms
        assert!(is_upgrade_invocation(&to_vec(&["upgrade"])));
        assert!(is_upgrade_invocation(&to_vec(&["up"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Syu"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Suy"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Su"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Sy"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Syuu"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-y", "-u"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-u", "-y"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-u"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-y"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-u", "-n"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Syu", "--noconfirm"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "--sysupgrade"])));

        // Non-upgrade forms (should not match upgrade)
        assert!(!is_upgrade_invocation(&to_vec(&["-S"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-S", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Syu", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Ss", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-S", "-s", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Si", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-S", "-i", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Sc"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-S", "-c"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Sw", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Sp", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["check"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Qu"])));

        // Upgrade with option-consuming flags
        assert!(is_upgrade_invocation(&to_vec(&[
            "-Syu", "--ignore", "linux"
        ])));
        assert!(is_upgrade_invocation(&to_vec(&["-Syu", "--ignore=linux"])));
        assert!(is_upgrade_invocation(&to_vec(&[
            "-S",
            "-u",
            "--overwrite",
            "/usr/*"
        ])));
        assert!(is_upgrade_invocation(&to_vec(&[
            "-Syu",
            "--assume-installed",
            "foo:1.0"
        ])));
    }

    #[test]
    fn test_parse_pacman_cli_args() {
        let to_vec = |slice: &[&str]| slice.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Separates option arguments from targets
        let (targets, flags) =
            parse_pacman_cli_args(&to_vec(&["--ignore", "linux", "--noconfirm", "neovim"]));
        assert_eq!(targets, vec!["neovim"]);
        assert_eq!(flags, vec!["--ignore", "linux", "--noconfirm"]);

        // Handles --ignore=linux equals syntax
        let (targets, flags) = parse_pacman_cli_args(&to_vec(&["--ignore=linux", "ripgrep"]));
        assert_eq!(targets, vec!["ripgrep"]);
        assert_eq!(flags, vec!["--ignore=linux"]);

        // Handles double dash -- to treat everything after as targets
        let (targets, flags) = parse_pacman_cli_args(&to_vec(&["-S", "--", "--ignore", "foo"]));
        assert_eq!(flags, vec!["-S"]);
        assert_eq!(targets, vec!["--ignore", "foo"]);

        // Handles config, dbpath, root, overwrite flags consuming arguments
        let (targets, flags) = parse_pacman_cli_args(&to_vec(&[
            "--config",
            "/etc/pacman.conf",
            "-b",
            "/var/lib/pacman",
            "--overwrite",
            "/usr/share/*",
            "git",
        ]));
        assert_eq!(targets, vec!["git"]);
        assert_eq!(
            flags,
            vec![
                "--config",
                "/etc/pacman.conf",
                "-b",
                "/var/lib/pacman",
                "--overwrite",
                "/usr/share/*"
            ]
        );

        let args = to_vec(&[
            "--ignore", "linux", "--overwrite", "*", "--assume-installed", "foo=1.0",
            "--noconfirm", "bar",
        ]);
        let (targets, flags) = parse_pacman_cli_args(&args);
        assert_eq!(targets, vec!["bar"]);
        assert_eq!(flags, args[..7]);
        assert!(!is_upgrade_invocation(&to_vec(&[
            "-Syu", "--ignore", "linux", "--overwrite", "*",
            "--assume-installed", "foo=1.0", "bar",
        ])));
    }

    #[test]
    fn test_pacman_files_needs_sudo() {
        let to_vec = |slice: &[&str]| slice.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Read-only queries should NOT require sudo
        assert!(!pacman_files_needs_sudo(&to_vec(&["-F", "python"])));
        assert!(!pacman_files_needs_sudo(&to_vec(&["-Fl", "ripgrep"])));
        assert!(!pacman_files_needs_sudo(&to_vec(&["-Fs", "libssl.so"])));
        assert!(!pacman_files_needs_sudo(&to_vec(&[
            "-F",
            "-b",
            "/var/lib/pacman",
            "python"
        ])));

        // Sync / refresh actions DO require sudo
        assert!(pacman_files_needs_sudo(&to_vec(&["-Fy"])));
        assert!(pacman_files_needs_sudo(&to_vec(&["-Fyy"])));
        assert!(pacman_files_needs_sudo(&to_vec(&["-F", "-y"])));
        assert!(pacman_files_needs_sudo(&to_vec(&["-F", "--refresh"])));
        assert!(pacman_files_needs_sudo(&to_vec(&["-Fy", "python"])));
    }

    #[test]
    fn test_find_matching_pin_exact_and_wildcard() {
        let mut pins = std::collections::BTreeMap::new();
        pins.insert("linux-cachyos*".to_string(), "cachyos".to_string());
        pins.insert("neovim".to_string(), "extra".to_string());

        let res_exact = find_matching_pin("neovim", &pins);
        assert_eq!(res_exact, Some(("neovim".to_string(), "extra".to_string(), true)));

        let res_wildcard = find_matching_pin("linux-cachyos-headers", &pins);
        assert_eq!(res_wildcard, Some(("linux-cachyos*".to_string(), "cachyos".to_string(), false)));

        let res_none = find_matching_pin("ripgrep", &pins);
        assert_eq!(res_none, None);
    }

    #[test]
    fn test_parse_install_flags() {
        let to_vec = |slice: &[&str]| slice.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Standalone -y (refresh without sysupgrade)
        let opts = parse_install_flags(&to_vec(&["-y"]));
        assert!(opts.refresh);
        assert!(!opts.sysupgrade);
        assert!(!opts.dry_run);
        assert!(opts.forwarded_flags.is_empty());

        // Standalone -u (sysupgrade without refresh)
        let opts = parse_install_flags(&to_vec(&["-u"]));
        assert!(!opts.refresh);
        assert!(opts.sysupgrade);
        assert!(opts.forwarded_flags.is_empty());

        // Combined -yu and -uy (refresh and sysupgrade)
        let opts = parse_install_flags(&to_vec(&["-yu"]));
        assert!(opts.refresh);
        assert!(opts.sysupgrade);
        assert!(opts.forwarded_flags.is_empty());

        let opts = parse_install_flags(&to_vec(&["-uy"]));
        assert!(opts.refresh);
        assert!(opts.sysupgrade);
        assert!(opts.forwarded_flags.is_empty());

        // Long options
        let opts = parse_install_flags(&to_vec(&["--refresh", "--sysupgrade", "--noconfirm"]));
        assert!(opts.refresh);
        assert!(opts.sysupgrade);
        assert!(opts.noconfirm);
        assert!(opts.forwarded_flags.is_empty());

        // Forwarding unrecognized flags while consuming internal ones
        let opts = parse_install_flags(&to_vec(&["-yu", "--overwrite", "*", "--cachedir=/tmp"]));
        assert!(opts.refresh);
        assert!(opts.sysupgrade);
        assert_eq!(opts.forwarded_flags, vec!["--overwrite", "*", "--cachedir=/tmp"]);
    }
}
