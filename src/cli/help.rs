//! User-facing CLI reference.
use colored::Colorize;
use crate::ui::print_banner;

pub(super) fn print_help() {
    print_banner();
    println!("\nUsage: pacpin <command> [options]");
    println!("  config init                Create editable managers.toml / sources.toml / hooks.toml (preserves existing files)");
    println!("  config validate            Validate all pacpin configuration files");
    println!("  manager NAME --FLAG        Preview and run a configured manager action");
    println!();
    println!("{}", "Core Package Operations:".bold());
    println!("  check, -Qu                   Check pending updates & verify pin protections");
    println!("  upgrade, -Syu [flags] [pkg...] Execute safe upgrade with True Resolver");
    println!("    Requested packages get their own box, and the approval");
    println!("    also covers auto-pulled dependencies (separate box).");
    println!("    Flags:");
    println!("      -n, --dry-run            Show transaction summary without executing");
    println!("      -y, --refresh            Refresh sync databases with the selected privilege tool");
    println!("      -c, --clean              Prompt to clean all orphans during upgrade");
    println!("      --no-cache               Clean caches for successful native/AUR transactions");
    println!("      --sources-only           Upgrade configured Sources only");
    println!("      --aur-only               Upgrade AUR targets only (build dependencies may use pacman)");
    println!("      --external-only          Upgrade enabled external managers only");
    println!("      --MANAGER.FLAG           Apply a custom flag defined in managers.toml");
    println!("      --keep-going             Skip failed checks/unsupported manager options; report incomplete work");
    println!("                               Also try Sources after a verified AUR build failure");
    println!("      --use-ram                Use tmpfs for Sources / Paru builds and native run/try files");
    println!("      --use-storage            Use disk for Sources / Paru builds and native run/try files");
    println!("      --noconfirm              Bypass interactive confirmation prompt");
    println!("  -S [flags] [repo/]pkg        Install packages (auto-pins repo/pkg with companion cascade)");
    println!("  -S github:owner/project      Install a reviewed direct upstream release (--channel alpha)");
    println!("  -S source-alias/pkg          Install a configured upstream release (see docs/sources.md)");
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
    println!("  init [--reset]               Guided preferences/managers setup, final review and backup");
    println!("    init --help                 Setup behavior, cancellation and supplemental file ownership");
    println!("  capture [-n|--force]          Save explicitly installed packages to [packages]");
    println!("  rebuild [-n|--noconfirm]      Install missing declared packages without removing extras");
    println!("    --enforce-repos             Reinstall present repo-qualified packages from declared repos");
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
    println!("  pacpin -Syu [pkg...]           Alias for 'pacpin upgrade -y' (targets join the upgrade)");
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

pub(super) fn print_version() {
    println!("pacpin {}", env!("CARGO_PKG_VERSION"));
    println!("Copyright (C) 2026 Gyan <330976822+gyanhavingsyntropy@users.noreply.github.com>");
    println!("License GPLv3+: GNU GPL version 3 or later <https://gnu.org/licenses/gpl.html>");
    println!("This is free software: you are free to change and redistribute it.");
    println!("There is NO WARRANTY, to the extent permitted by law.");
}
