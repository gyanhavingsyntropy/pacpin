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

//! Ephemeral application routing. Shared process ownership lives in execution.
use colored::Colorize;
use std::process::exit;

mod entrypoints;
mod mounts;
mod native;
mod providers;
mod workspace;
#[cfg(test)]
mod tests;

pub use workspace::clean_stale_sandboxes;
use native::try_pacman;
use providers::{try_flatpak, try_nix, try_pipx};

pub fn parse_target(target: &str) -> (Option<&str>, &str) {
    if let Some((prefix, rest)) = target.split_once('/') {
        (Some(prefix), rest)
    } else if let Some((prefix, rest)) = target.split_once(':') {
        (Some(prefix), rest)
    } else if let Some(rest) = target.strip_prefix("nixpkgs#") {
        (Some("nix"), rest)
    } else {
        (None, target)
    }
}

#[derive(Debug, Clone, Default)]
pub struct TryOptions {
    pub no_sandbox: bool,
    pub share_net: bool,
    pub rw_cwd: bool,
    pub gui: bool,
    pub audio: bool,
    pub bin: Option<String>,
    pub allow_unverified: bool,
    pub is_run_mode: bool,
    pub noconfirm: bool,
    pub installed: bool,
    pub no_cache: bool,
    pub dry_run: bool,
}

impl TryOptions {
    pub fn for_run() -> Self {
        Self {
            no_sandbox: true,
            share_net: true,
            rw_cwd: true,
            gui: false,
            audio: false,
            bin: None,
            allow_unverified: false,
            is_run_mode: true,
            noconfirm: false,
            installed: false,
            no_cache: false,
            dry_run: false,
        }
    }
}

pub fn cmd_try(target: &str, args: &[String], options: &TryOptions) {
    let (prefix, pkg) = parse_target(target);
    if options.no_cache && matches!(prefix, Some("flatpak" | "nix" | "nixpkgs")) {
        eprintln!("Error: --no-cache is unsupported for this run/try backend. Its installed runtime/store is not a disposable cache.");
        exit(2);
    }
    if crate::scratch::policy() != crate::scratch::Policy::Auto
        && matches!(prefix, Some("flatpak" | "nix" | "nixpkgs" | "pipx"))
    {
        eprintln!("Error: --use-ram/--use-storage is unsupported for {prefix:?} run/try: this backend owns its store or environment and cannot guarantee relocation. Native repository packages support these flags.");
        exit(2);
    }
    if options.installed && prefix.is_some() {
        eprintln!("Error: --installed requires an unqualified native package name; omit the repository/provider prefix.");
        exit(2);
    }
    if options.dry_run && matches!(prefix, Some("flatpak" | "nix" | "nixpkgs" | "pipx")) {
        if !crate::journal::TransactionJournal::is_safe_pkg_name(pkg) {
            eprintln!("Error: invalid package identity"); exit(2);
        }
        if host_only_provider(prefix) && !options.no_sandbox {
            eprintln!("Error: Nix and Pipx require run or --no-sandbox"); exit(2);
        }
        println!(":: Dry-run: backend {:?} would run {pkg:?} with arguments {args:?}; no command executed.{}", prefix, if options.no_cache { " Pipx environment reuse disabled." } else { "" });
        exit(0);
    }

    let res = match prefix {
        Some("flatpak") => try_flatpak(pkg, args),
        provider if host_only_provider(provider) && !options.no_sandbox => Err(
            "Nix and Pipx run their programs on the host. Use 'pin run' or pass --no-sandbox to opt in to host execution."
                .to_string(),
        ),
        Some("nix") | Some("nixpkgs") => try_nix(pkg, args),
        Some("pipx") => try_pipx(pkg, args, options.no_cache),
        Some(repo) => try_pacman(Some(repo), pkg, args, options),
        None => try_pacman(None, pkg, args, options),
    };

    match res {
        Ok(code) => exit(code),
        Err(err) => {
            eprintln!("{}", format!("Error: {}", err).red().bold());
            exit(1);
        }
    }
}

fn host_only_provider(prefix: Option<&str>) -> bool {
    matches!(prefix, Some("nix" | "nixpkgs" | "pipx"))
}
