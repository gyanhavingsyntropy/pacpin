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
mod export;
mod mounts;
mod native;
mod prepared;
mod routing;
mod launch;
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
    pub(crate) fn preparation(&self) -> crate::preparation::Options {
        crate::preparation::Options {
            no_cache: self.no_cache, allow_unverified: self.allow_unverified,
            noconfirm: self.noconfirm,
            prefix: if self.is_run_mode { "pacpin-run" } else { "pacpin-try" },
        }
    }
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
    if !crate::utils::is_safe_pkg_name(pkg) {
        eprintln!("Error: invalid package identity"); exit(2);
    }
    if options.installed && prefix.is_some() {
        eprintln!("Error: --installed requires an unqualified native package name; omit the repository/provider prefix.");
        exit(2);
    }
    let config = crate::config::load_config();
    let repositories = if prefix.is_some() { crate::db::AlpmManager::discover_repos() } else { Vec::new() };
    let backend = match routing::resolve(prefix, &config, &repositories) {
        Ok(backend) => backend,
        Err(error) => { eprintln!("Error: {error}"); exit(2); }
    };
    if let Err(error) = backend.check_options(options, crate::scratch::policy()) {
        eprintln!("Error: {error}"); exit(2);
    }
    use routing::Backend;
    if options.dry_run && matches!(backend, Backend::Flatpak | Backend::Nix | Backend::Pipx) {
        println!(":: Dry-run: backend {:?} would run {pkg:?} with arguments {args:?}; no command executed.{}", prefix, if options.no_cache { " Pipx environment reuse disabled." } else { "" });
        exit(0);
    }

    let res = match backend {
        Backend::Aur => prepared::try_aur(&config, pkg, args, options),
        Backend::Flatpak => try_flatpak(pkg, args),
        Backend::Nix => try_nix(pkg, args),
        Backend::Pipx => try_pipx(pkg, args, options.no_cache),
        Backend::Source(alias) => prepared::try_source(&config, alias, pkg, args, options),
        Backend::Export(manager) => export::try_export(&config, manager, pkg, args, options),
        Backend::Native(repo) => try_pacman(&config, repo, pkg, args, options),
    };

    match res {
        Ok(code) => exit(code),
        Err(err) => {
            eprintln!("{}", format!("Error: {}", err).red().bold());
            exit(1);
        }
    }
}
