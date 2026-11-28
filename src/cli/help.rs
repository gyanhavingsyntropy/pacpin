//! User-facing CLI reference.
use colored::Colorize;
use crate::commands::cli::{is_check_invocation, is_upgrade_invocation, takes_value};
use crate::ui::print_banner;

pub(super) fn write_general_help(out: &mut impl std::io::Write) {
    let _ = writeln!(out, "\nUsage: pacpin <command> [options]");
    let _ = writeln!(out, "  pacpin <command> --help    Show command-specific options (e.g. 'pacpin -S --help', 'pacpin init --help')");
    let _ = writeln!(out, "  config init                Create editable managers.toml / sources.toml / hooks.toml (preserves existing files)");
    let _ = writeln!(out, "  config validate            Validate all pacpin configuration files");
    let _ = writeln!(out, "  manager NAME --FLAG        Preview and run a configured manager action [--dry-run] [--noconfirm]");
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", "Global Invocation Options:".bold());
    let _ = writeln!(out, "  --sudo                     Use sudo for privileged operations in this invocation");
    let _ = writeln!(out, "  --pkexec                   Use pkexec for privileged operations in this invocation");
    let _ = writeln!(out, "  --noconfirm                Bypass interactive confirmation prompts where supported");
    let _ = writeln!(out, "                             (upgrade, -S/install, remove/-R, rebuild, clean, orphans, rollback, run/try, source adopt, manager, reset, keep)");
    let _ = writeln!(out, "  --wait [SECS]              Wait up to SECS (default 600) for an active pacman lock (upgrade, -S/install, remove)");
    let _ = writeln!(out, "  -h, --help                 Show this help message (or <command> --help for subcommand options)");
    let _ = writeln!(out, "  -v, -V, --version          Show pacpin version and license information");
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", "Core Package Operations:".bold());
    let _ = writeln!(out, "  check, -Qu [flags]           Check pending updates & verify pin protections");
    let _ = writeln!(out, "    Flags:");
    let _ = writeln!(out, "      -q, --quiet              Output package names only");
    let _ = writeln!(out, "      -r, --repo               Check official repositories only");
    let _ = writeln!(out, "      -a, --aur, --aur-only    Check AUR packages only");
    let _ = writeln!(out, "      --sources-only           Check configured Sources only");
    let _ = writeln!(out, "      --external-only          Check enabled external managers only");
    let _ = writeln!(out, "      --no-flatpak             Exclude Flatpak from update checks");
    let _ = writeln!(out, "      --keep-going             Report partial results if a provider check fails");
    let _ = writeln!(out, "  upgrade, -Syu [flags] [pkg...] Execute safe upgrade with True Resolver");
    let _ = writeln!(out, "    Requested packages get their own box, and the approval");
    let _ = writeln!(out, "    also covers auto-pulled dependencies (separate box).");
    let _ = writeln!(out, "    Flags:");
    let _ = writeln!(out, "      -n, --dry-run            Show transaction summary without executing");
    let _ = writeln!(out, "      -y, --refresh            Refresh sync databases with the selected privilege tool");
    let _ = writeln!(out, "      -c, --clean              Prompt to clean all orphans during upgrade");
    let _ = writeln!(out, "      --no-cache               Clean caches for successful native/AUR transactions");
    let _ = writeln!(out, "      --cleanup=POLICY         temporary, cache, or temporary,cache; shared by install/upgrade/rebuild/try/run");
    let _ = writeln!(out, "                               temporary keeps AUR/compiled Sources build dependencies private");
    let _ = writeln!(out, "      --sources-only           Upgrade configured Sources only");
    let _ = writeln!(out, "      --aur-only               Upgrade AUR targets only (build dependencies may use pacman)");
    let _ = writeln!(out, "      --external-only          Upgrade enabled external managers only");
    let _ = writeln!(out, "      --MANAGER.FLAG           Apply a custom flag defined in managers.toml");
    let _ = writeln!(out, "      --keep-going             Skip failed checks/unsupported manager options; report incomplete work");
    let _ = writeln!(out, "                               Also try Sources after a verified AUR build failure");
    let _ = writeln!(out, "      --use-ram                Use tmpfs for supported builds and temporary app files");
    let _ = writeln!(out, "      --use-storage            Use disk for supported builds and temporary app files");
    let _ = writeln!(out, "      --noconfirm              Bypass interactive confirmation prompt");
    let _ = writeln!(out, "  -S, install [flags] [repo/]pkg  Install packages (auto-pins repo/pkg with companion cascade)");
    let _ = writeln!(out, "    Flags: -n/--dry-run, -y/--refresh, -u/--sysupgrade, --noconfirm, --needed,");
    let _ = writeln!(out, "           --no-cache, --cleanup=POLICY, --use-ram/--use-storage, --channel <chan>,");
    let _ = writeln!(out, "           --wait [SECS], --sudo/--pkexec (run 'pacpin -S --help' for full details)");
    let _ = writeln!(out, "  -S github:owner/project      Install a reviewed direct upstream release (--channel alpha)");
    let _ = writeln!(out, "  -S source-alias/pkg          Install a configured upstream release (see docs/sources.md)");
    let _ = writeln!(out, "  source list                  List retained directory Sources (install_dir in sources.toml)");
    let _ = writeln!(out, "  source adopt alias/pkg       Reference an existing app: --from PATH --version VERSION [--dry-run] [--noconfirm]");
    let _ = writeln!(out, "  -Sp, --print-uris <pkg...>   Print package and dependency download URIs");
    let _ = writeln!(out, "  -Sl, --list [repo...]        List packages in sync repositories");
    let _ = writeln!(out, "  -Sg, --groups [group...]     List package groups or packages in a group");
    let _ = writeln!(out, "  remove, rm <pkg...>          Remove package(s) and unneeded dependencies (-Rns + smart unpin)");
    let _ = writeln!(out, "  search, -Ss <query...>       Search official repositories and AUR simultaneously");
    let _ = writeln!(out, "  info, -Si <pkg...>           Show detailed package information (-Si / AUR)");
    let _ = writeln!(out, "  clean, -Sc                   Clean pacman and AUR build cache (also -Scc, --repo, --aur, -n, --noconfirm)");
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", "Declarative Rules & Configuration:".bold());
    let _ = writeln!(
        out,
        "  repos                        Interactive BIOS-style repository search priority menu"
    );
    let _ = writeln!(
        out,
        "  pin <repo> <pkg...>          Lock package(s) or wildcards to a designated repository"
    );
    let _ = writeln!(
        out,
        "  pin <repo> -f <file>         Batch pin packages from a text file (one package per line)"
    );
    let _ = writeln!(out, "  unpin <pkg>                  Remove a repository pin rule");
    let _ = writeln!(out, "  delay <pkg> <days>           Set a stability delay buffer on a package");
    let _ = writeln!(out, "  undelay <pkg>                Remove a package stability delay rule");
    let _ = writeln!(
        out,
        "  list                         List active pins, exclusions, delays, and repo priority"
    );
    let _ = writeln!(out, "  reset [-f]                   Reset all configurations, pins, and delays to default (-f force, --noconfirm)");
    let _ = writeln!(out, "  init [--reset]               Guided preferences/managers setup, final review and backup");
    let _ = writeln!(out, "    init --help                Setup behavior, cancellation and supplemental file ownership");
    let _ = writeln!(out, "  capture [-n|--force]         Save explicitly installed packages to [packages]");
    let _ = writeln!(out, "  rebuild [-n|--noconfirm]     Install missing declared packages without removing extras");
    let _ = writeln!(out, "    --enforce-repos            Reinstall present repo-qualified packages from declared repos");
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", "System Maintenance & Hygiene:".bold());
    let _ = writeln!(
        out,
        "  orphans [-c]                 Inspect or remove orphaned dependencies (-c to clean, --noconfirm)"
    );
    let _ = writeln!(out, "  autoremove                   Alias for 'pacpin orphans -c'");
    let _ = writeln!(out, "  keep, adopt <pkg...>         Mark package(s) as explicitly installed (silences orphan warnings)");
    let _ = writeln!(out, "  needrestart                  Inspect processes holding outdated libraries or kernel in RAM");
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", "Power Tools & Ephemeral Execution:".bold());
    let _ = writeln!(out, "  run [options] [backend/]pkg  Prepare a temporary app; normal home/project writes persist");
    let _ = writeln!(out, "  try [options] [backend/]pkg  Prepare a temporary app; isolated runtime by default");
    let _ = writeln!(out, "    Backends: native repos, aur, configured Sources/exports, Flatpak, Nix, Pipx");
    let _ = writeln!(out, "    AUR/Sources build dependencies stay private; backend stores retain their limits.");
    let _ = writeln!(
        out,
        "  history [id]                 View transaction history timeline or inspect a transaction"
    );
    let _ = writeln!(out, "  rollback [id] [-n]           Restore previous package versions from cache [--allow-partial] [--noconfirm]");
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", "Pacman & AUR Drop-in Aliases:".bold());
    let _ = writeln!(out, "  pacpin -Syu [pkg...]           Alias for 'pacpin upgrade -y' (targets join the upgrade)");
    let _ = writeln!(out, "  pacpin -Qu                   Alias for 'pacpin check'");
    let _ = writeln!(out, "  pacpin -Ss <query...>        Search repositories and AUR");
    let _ = writeln!(out, "  pacpin -Si <pkg...>          Show remote/AUR package metadata");
    let _ = writeln!(out, "  pacpin -Qi <pkg...>          Show locally installed package info");
    let _ = writeln!(out, "  pacpin -R / -Rns <pkg...>    Remove packages (with smart pin cleanup)");
    let _ = writeln!(out, "  pacpin -Q / -Qo / -Ql        Pacman query and package inspection");
    let _ = writeln!(out, "  pacpin -U <pkg.tar.zst>      Install local package archive");
    let _ = writeln!(out, "  pacpin -F / -Fy              Pacman files database operations");
    let _ = writeln!(out, "  pacpin -T <deps...>          Check dependency requirements");
    let _ = writeln!(out);
}

pub(super) fn print_help() {
    print_banner();
    let mut out = std::io::stdout().lock();
    write_general_help(&mut out);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HelpTopic {
    General,
    Install,
    Upgrade,
    Check,
    Remove,
    Clean,
    Search,
    Info,
    PrintUris,
    SyncList,
    SyncGroups,
    Run,
    Try,
    Rebuild,
    Orphans,
    Keep,
    Rollback,
    History,
    Capture,
    Reset,
    Pin,
    Unpin,
    Delay,
    Undelay,
    List,
    Repos,
    Source,
    Manager,
    Config,
    Needrestart,
}

fn has_help_flag(args: &[String], pacman_style: bool, value_flags: &[&str]) -> bool {
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            return false;
        }
        if arg == "--help" || arg == "-h" {
            return true;
        }
        if pacman_style && arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains('h') {
            return true;
        }
        if value_flags.contains(&arg) || (pacman_style && takes_value(arg)) {
            i += 2;
            continue;
        }
        if pacman_style && arg == "--wait" && i + 1 < args.len() && args[i + 1].parse::<u64>().is_ok() {
            i += 2;
            continue;
        }
        i += 1;
    }
    false
}

fn ephemeral_has_help(args: &[String]) -> bool {
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            return false;
        }
        if arg == "--help" || arg == "-h" {
            return true;
        }
        if arg == "--bin" {
            i += 2;
            continue;
        }
        // Only flags before the target belong to pacpin; trailing `--help`/`-h`
        // after a package name targets the executed application itself.
        if !arg.starts_with('-') {
            return false;
        }
        i += 1;
    }
    false
}

pub(super) fn detect_help_topic(args: &[String]) -> Option<HelpTopic> {
    let first = args.first()?.as_str();
    if matches!(first, "-h" | "--help") {
        return Some(HelpTopic::General);
    }
    if first == "help" {
        if args.len() == 1 {
            return Some(HelpTopic::General);
        }
        let mut synthetic = Vec::with_capacity(args.len());
        synthetic.extend(args[1..].iter().cloned());
        synthetic.push("--help".to_string());
        return detect_help_topic(&synthetic).or(Some(HelpTopic::General));
    }

    let rest = &args[1..];
    if first == "run" {
        return ephemeral_has_help(rest).then_some(HelpTopic::Run);
    }
    if first == "try" {
        return ephemeral_has_help(rest).then_some(HelpTopic::Try);
    }
    if is_check_invocation(args) {
        let short_h = first.starts_with("-Q") && !first.starts_with("--") && first[1..].contains('h');
        return (short_h || has_help_flag(rest, false, &[])).then_some(HelpTopic::Check);
    }
    if super::is_clean_invocation(args) {
        let short_h = first.starts_with("-S") && !first.starts_with("--") && first[1..].contains('h');
        return (short_h || has_help_flag(rest, true, &[])).then_some(HelpTopic::Clean);
    }
    if first == "search" || first == "-Ss" || (first.starts_with("-S") && (first.contains('s') || crate::commands::cli::iter_flags(rest).any(|a| a == "-s" || a == "--search"))) {
        let short_h = first.starts_with("-S") && !first.starts_with("--") && first[1..].contains('h');
        return (short_h || has_help_flag(rest, true, &[])).then_some(HelpTopic::Search);
    }
    if first == "info" || first == "-Si" || (first.starts_with("-S") && (first.contains('i') || crate::commands::cli::iter_flags(rest).any(|a| a == "-i" || a == "--info"))) {
        let short_h = first.starts_with("-S") && !first.starts_with("--") && first[1..].contains('h');
        return (short_h || has_help_flag(rest, true, &[])).then_some(HelpTopic::Info);
    }
    if first == "-Sp" || (first.starts_with("-S") && (first.contains('p') || crate::commands::cli::iter_flags(rest).any(|a| a == "-p" || a == "--print" || a == "--print-uris"))) {
        let short_h = !first.starts_with("--") && first[1..].contains('h');
        return (short_h || has_help_flag(rest, true, &[])).then_some(HelpTopic::PrintUris);
    }
    if first == "-Sl" || (first.starts_with("-S") && (first.contains('l') || crate::commands::cli::iter_flags(rest).any(|a| a == "-l" || a == "--list"))) {
        let short_h = !first.starts_with("--") && first[1..].contains('h');
        return (short_h || has_help_flag(rest, true, &[])).then_some(HelpTopic::SyncList);
    }
    if first == "-Sg" || (first.starts_with("-S") && (first.contains('g') || crate::commands::cli::iter_flags(rest).any(|a| a == "-g" || a == "--groups"))) {
        let short_h = !first.starts_with("--") && first[1..].contains('h');
        return (short_h || has_help_flag(rest, true, &[])).then_some(HelpTopic::SyncGroups);
    }
    if is_upgrade_invocation(args) {
        let short_h = first.starts_with("-S") && !first.starts_with("--") && first[1..].contains('h');
        return (short_h || has_help_flag(rest, true, &[])).then_some(HelpTopic::Upgrade);
    }
    if first == "install" || first == "-S" || (first.starts_with("-S") && !first.starts_with("--")) {
        let short_h = first.starts_with("-S") && !first.starts_with("--") && first[1..].contains('h');
        return (short_h || has_help_flag(rest, true, &[])).then_some(HelpTopic::Install);
    }
    if first == "remove" || first == "rm" || first.starts_with("-R") {
        let short_h = first.starts_with("-R") && !first.starts_with("--") && first[1..].contains('h');
        return (short_h || has_help_flag(rest, true, &[])).then_some(HelpTopic::Remove);
    }

    let topic = match first {
        "rebuild" => HelpTopic::Rebuild,
        "orphans" | "autoremove" => HelpTopic::Orphans,
        "keep" | "adopt" => HelpTopic::Keep,
        "rollback" => HelpTopic::Rollback,
        "history" => HelpTopic::History,
        "capture" => HelpTopic::Capture,
        "reset" | "config-reset" => HelpTopic::Reset,
        "pin" | "add-pin" | "add-pins" => HelpTopic::Pin,
        "unpin" => HelpTopic::Unpin,
        "delay" => HelpTopic::Delay,
        "undelay" => HelpTopic::Undelay,
        "list" => HelpTopic::List,
        "repos" | "repo-order" | "priority" => HelpTopic::Repos,
        "source" => HelpTopic::Source,
        "manager" => HelpTopic::Manager,
        "config" => HelpTopic::Config,
        "needrestart" | "restart-check" => HelpTopic::Needrestart,
        _ => return None,
    };

    let value_flags: &[&str] = match topic {
        HelpTopic::Pin => &["-f", "--file"],
        HelpTopic::Source => &["--from", "--version"],
        _ => &[],
    };
    has_help_flag(rest, matches!(topic, HelpTopic::Rollback | HelpTopic::Orphans | HelpTopic::Reset), value_flags).then_some(topic)
}

pub(super) fn render_topic_help(topic: HelpTopic) -> String {
    match topic {
        HelpTopic::General => String::new(),
        HelpTopic::Run | HelpTopic::Try => String::new(),
        HelpTopic::Install => [
            "Usage: pacpin -S [flags] [repo/]pkg ...",
            "       pacpin install [flags] [repo/]pkg ...",
            "       pacpin -S github:owner/project [--channel alpha]",
            "       pacpin -S source-alias/pkg",
            "",
            "Install native repository packages (auto-pinned with companion cascade),",
            "AUR packages, direct GitHub/GitLab upstream releases, or configured Sources.",
            "",
            "Options:",
            "  -n, --dry-run            Preview transaction plan and commands without executing",
            "  -y, --refresh            Refresh sync databases before installing (-yy forces)",
            "  -u, --sysupgrade         Also upgrade outdated packages (-uu allows downgrades)",
            "  --noconfirm              Bypass interactive confirmation prompts",
            "  --needed                 Do not reinstall up-to-date targets (default for -S)",
            "  --channel <CHANNEL>      Release channel for github:/gitlab: (stable, alpha, beta, prerelease)",
            "  --no-cache, --clean-cache Clean caches for successful native/AUR transaction",
            "  --cleanup=POLICY         temporary, cache, or temporary,cache",
            "  --use-ram                Use tmpfs for supported builds and temporary files",
            "  --use-storage            Use disk for supported builds and temporary files",
            "  --wait [SECS]            Wait up to SECS (default 600) for an active pacman lock",
            "  --sudo | --pkexec        Override configured privilege escalation tool for this run",
            "  --                       Treat remaining arguments as literal package targets",
            "",
            "Pacman & Sync Operation Flags:",
            "  --ignore, --ignoregroup, --overwrite, --assume-installed, --asdeps, --asexplicit,",
            "  --dbpath (-b), --root (-r), --config, --cachedir, --logfile, --gpgdir, --hookdir,",
            "  --arch, --color          Forwarded safely to pacman/helper",
            "",
            "Related Sync Operations:",
            "  pacpin -Sy               Refresh package databases only",
            "  pacpin -Syu / -Su        Run declarative upgrade engine (see 'pacpin upgrade --help')",
            "  pacpin -Ss / -Si / -Sp / -Sl / -Sg / -Sc / -Sw",
        ]
        .join("\n"),
        HelpTopic::Upgrade => [
            "Usage: pacpin upgrade [flags] [pkg...]",
            "       pacpin -Syu [flags] [pkg...]",
            "       pacpin -Su [flags] [pkg...]",
            "",
            "Execute safe declarative system upgrade across repositories, AUR, Sources,",
            "and enabled external managers, optionally joining explicit targets.",
            "",
            "Options:",
            "  -n, --dry-run            Show transaction summary without executing",
            "  -y, --refresh            Refresh sync databases and external metadata",
            "  -c, --clean, --autoremove Prompt/remove pure orphans after successful upgrade",
            "  --noconfirm              Bypass interactive confirmation prompt",
            "  --no-cache, --clean-cache Clean caches for successful native/AUR transactions",
            "  --cleanup=POLICY         temporary, cache, or temporary,cache",
            "  --sources-only           Upgrade configured Sources only",
            "  --aur-only               Upgrade AUR packages only",
            "  --external-only          Upgrade enabled external managers only",
            "  --MANAGER.FLAG           Apply a custom flag defined in managers.toml",
            "  --keep-going             Skip failed checks or unsupported manager options",
            "  --use-ram / --use-storage Use tmpfs or disk for supported builds",
            "  --wait [SECS]            Wait up to SECS (default 600) for an active pacman lock",
            "  --sudo | --pkexec        Override configured privilege escalation tool for this run",
            "  pacpin -Sy               Refresh native repository databases only",
        ]
        .join("\n"),
        HelpTopic::Check => [
            "Usage: pacpin check [flags]",
            "       pacpin -Qu [flags]",
            "",
            "Check pending updates across repositories, AUR, configured Sources,",
            "and external managers while verifying pin and delay protections.",
            "",
            "Options:",
            "  -q, --quiet              Output updatable package names only",
            "  -r, --repo               Check official repositories only",
            "  -a, --aur, --aur-only    Check AUR packages only",
            "  --sources-only           Check configured Sources only",
            "  --external-only          Check enabled external managers only",
            "  --no-flatpak             Exclude Flatpak from update checks",
            "  --keep-going             Report partial results with exit code 2 if a provider fails",
        ]
        .join("\n"),
        HelpTopic::Remove => [
            "Usage: pacpin remove [flags] <pkg...>",
            "       pacpin rm [flags] <pkg...>",
            "       pacpin -R[ns...] [flags] <pkg...>",
            "",
            "Remove packages with pacman and clean up matching declarative pin rules.",
            "'remove' / 'rm' defaults to '-Rns'; raw '-R*' keeps your exact pacman flags.",
            "",
            "Options:",
            "  -n, --dry-run            Preview synthesized removal command without executing",
            "  --noconfirm              Bypass pacman and smart-unpin confirmation prompts",
            "  --wait [SECS]            Wait up to SECS (default 600) for an active pacman lock",
            "  --sudo | --pkexec        Override configured privilege escalation tool for this run",
            "  -s, -c, -u, --cascade, ... Forwarded to pacman (note: standalone -n means --dry-run;",
            "                             bundle e.g. '-Rns' or '-Rn' for pacman --nosave)",
        ]
        .join("\n"),
        HelpTopic::Clean => [
            "Usage: pacpin clean [-c|--clean] [options]",
            "       pacpin -Sc | -Scc [options]",
            "",
            "Clean native pacman caches, AUR helper caches, and enabled integration caches.",
            "",
            "Options:",
            "  -c, --clean              Pass twice (-Scc or -c -c) to remove ALL cached packages",
            "  --repo                   Clean native pacman cache only",
            "  --aur                    Clean AUR helper cache only",
            "  -n, --dry-run            Preview cleanup commands without executing",
            "  --noconfirm              Bypass confirmation prompt",
            "  --sudo | --pkexec        Override configured privilege escalation tool for this run",
        ]
        .join("\n"),
        HelpTopic::Search => [
            "Usage: pacpin search <query...>",
            "       pacpin -Ss <query...>",
            "",
            "Search official repositories and AUR simultaneously (via configured helper or pacman).",
        ]
        .join("\n"),
        HelpTopic::Info => [
            "Usage: pacpin info <pkg...>",
            "       pacpin -Si <pkg...>",
            "",
            "Show remote repository or AUR package metadata (via configured helper or pacman).",
        ]
        .join("\n"),
        HelpTopic::PrintUris => [
            "Usage: pacpin -Sp <pkg...>",
            "       pacpin --print-uris <pkg...>",
            "",
            "Print download URIs for package(s) and their unresolved dependencies.",
        ]
        .join("\n"),
        HelpTopic::SyncList => [
            "Usage: pacpin -Sl [repo...]",
            "       pacpin --list [repo...]",
            "",
            "List all packages in all sync repositories or the specified repository(s).",
        ]
        .join("\n"),
        HelpTopic::SyncGroups => [
            "Usage: pacpin -Sg [group...]",
            "       pacpin --groups [group...]",
            "",
            "List package groups or member packages belonging to the specified group(s).",
        ]
        .join("\n"),
        HelpTopic::Rebuild => [
            "Usage: pacpin rebuild [options]",
            "",
            "Install declared packages missing from the system without removing extra packages.",
            "",
            "Options:",
            "  -n, --dry-run            Preview planned installations without executing",
            "  --noconfirm              Bypass interactive confirmation prompts",
            "  --enforce-repos          Reinstall present repo-qualified packages from declared repos",
            "  --no-cache, --clean-cache Clean caches for successful native/AUR installations",
            "  --cleanup=POLICY         temporary, cache, or temporary,cache",
            "  --use-ram / --use-storage Use tmpfs or disk for supported builds",
            "  --sudo | --pkexec        Override configured privilege escalation tool for this run",
        ]
        .join("\n"),
        HelpTopic::Orphans => [
            "Usage: pacpin orphans [-c] [--noconfirm] [--sudo | --pkexec]",
            "       pacpin autoremove [--noconfirm] [--sudo | --pkexec]",
            "",
            "Inspect unneeded orphaned dependencies, or remove them interactively with '-c'.",
        ]
        .join("\n"),
        HelpTopic::Keep => [
            "Usage: pacpin keep <pkg1> [pkg2...] [--sudo | --pkexec]",
            "       pacpin adopt <pkg1> [pkg2...] [--sudo | --pkexec]",
            "",
            "Mark package(s) as explicitly installed (`pacman -D --asexplicit`) to silence orphan prompts.",
            "Also accepts '--noconfirm' as a compatibility no-op.",
        ]
        .join("\n"),
        HelpTopic::Rollback => [
            "Usage: pacpin rollback [tx_id] [options]",
            "",
            "Restore previous package versions from cached archives for the latest or given transaction.",
            "",
            "Options:",
            "  -n, --dry-run            Preview rollback commands without executing",
            "  --allow-partial, -f      Proceed with available cached archives if some are missing",
            "  --noconfirm              Bypass interactive confirmation prompts",
            "  --sudo | --pkexec        Override configured privilege escalation tool for this run",
        ]
        .join("\n"),
        HelpTopic::History => "Usage: pacpin history [tx_id]\n\nView transaction history timeline or inspect details of a specific transaction ID.".into(),
        HelpTopic::Capture => "Usage: pacpin capture [-n|--dry-run] [-f|--force]\n\nSave explicitly installed packages into [packages] in pacpin config.".into(),
        HelpTopic::Reset => "Usage: pacpin reset [-f|--force] [--noconfirm]\n\nReset all pacpin configurations, pins, and delays to defaults.".into(),
        HelpTopic::Pin => "Usage: pacpin pin <repo> <pkg1> [pkg2...]\n       pacpin pin <repo> -f <packages.txt>\n       pacpin pin <pkg> <repo>\n\nLock package(s) or wildcards to a specific repository, 'aur', or configured source alias.".into(),
        HelpTopic::Unpin => "Usage: pacpin unpin <pattern>\n\nRemove a repository pin rule matching <pattern>.".into(),
        HelpTopic::Delay => "Usage: pacpin delay <pkg> <days>\n\nSet an update stability delay buffer (in days) on <pkg>.".into(),
        HelpTopic::Undelay => "Usage: pacpin undelay <pkg>\n\nRemove an update stability delay rule for <pkg>.".into(),
        HelpTopic::List => "Usage: pacpin list\n\nList active pins, exclusions, delays, and repository search priority.".into(),
        HelpTopic::Repos => "Usage: pacpin repos\n\nOpen interactive menu to inspect and reorder repository search priority.".into(),
        HelpTopic::Source => "Usage: pacpin source list\n       pacpin source adopt <alias/pkg> --from <dir> --version <ver> [-n|--dry-run] [--noconfirm]\n\nInspect or adopt retained directory Sources installs.".into(),
        HelpTopic::Manager => "Usage: pacpin manager <NAME> <--FLAG> [--dry-run|-n] [--noconfirm] [--sudo|--pkexec]\n\nPreview and run a custom action defined in managers.toml.".into(),
        HelpTopic::Config => "Usage: pacpin config init\n       pacpin config validate\n       pacpin config reset [-f|--force] [--noconfirm]\n\nInitialize, validate, or reset pacpin configuration files.".into(),
        HelpTopic::Needrestart => "Usage: pacpin needrestart [--sudo | --pkexec]\n\nInspect running processes holding outdated libraries or booted kernel updates in RAM.".into(),
    }
}

pub(super) fn maybe_handle_help(args: &[String]) -> bool {
    let Some(topic) = detect_help_topic(args) else {
        return false;
    };
    match topic {
        HelpTopic::General => print_help(),
        HelpTopic::Run => crate::commands::ephemeral::print_mode_help(true),
        HelpTopic::Try => crate::commands::ephemeral::print_mode_help(false),
        other => {
            print_banner();
            println!("\n{}", render_topic_help(other));
        }
    }
    true
}

pub(super) fn print_version() {
    println!("pacpin {}", env!("CARGO_PKG_VERSION"));
    println!("Copyright (C) 2026 Gyan <330976822+gyanhavingsyntropy@users.noreply.github.com>");
    println!("License GPLv3+: GNU GPL version 3 or later <https://gnu.org/licenses/gpl.html>");
    println!("This is free software: you are free to change and redistribute it.");
    println!("There is NO WARRANTY, to the extent permitted by law.");
}
