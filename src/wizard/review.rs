//! Setup review and reference use the same wrapping as its prompts.
use super::prompts::{Prompts, Result};
use crate::config::Config;
use std::io::{BufRead, Write};
use std::path::Path;

pub(super) fn show<R: BufRead, W: Write>(
    prompts: &mut Prompts<R, W>,
    config: &Config,
    path: &Path,
    backup: bool,
) -> Result<()> {
    prompts.section("Review setup")?;
    prompts.say(&format!("Main configuration: {}", path.display()))?;
    prompts.say(&format!(
        "Privilege tool: {} | AUR helper: {}",
        config.options.privilege_tool, config.options.helper
    ))?;
    for (label, enabled) in [
        ("Repository pinning", config.features.pinning),
        ("Vendor stickiness", config.features.vendor_stickiness),
        ("Stability delays", config.features.stability_delays),
        ("Smart orphan review", config.features.smart_orphans),
        ("No-cache upgrade default", config.options.no_cache),
        ("Shell alias maintenance", config.features.shell_aliases),
    ] {
        prompts.say(&format!(
            "{label}: {}",
            if enabled { "enabled" } else { "disabled" }
        ))?;
    }
    for (name, legacy) in [
        ("flatpak", config.integrations.flatpak),
        ("nix", config.integrations.nix),
        ("pipx", config.integrations.pipx),
    ] {
        let enabled = config
            .manager_file
            .managers
            .get(name)
            .and_then(|d| d.enabled)
            .unwrap_or(config.features.integrations && legacy);
        prompts.say(&format!(
            "{name} updates: {}",
            if enabled { "enabled" } else { "disabled" }
        ))?;
    }
    for (name, definition) in &config.manager_file.managers {
        if definition.adapter == "command" {
            prompts.say(&format!(
                "{name} updates: {} (custom)",
                if definition.enabled == Some(true) {
                    "enabled"
                } else {
                    "disabled"
                }
            ))?;
        }
    }
    let package_count = config.packages.pacman.len()
        + config.packages.aur.len()
        + config.packages.sources.len()
        + config.packages.flatpak_user.len()
        + config.packages.flatpak_system.len();
    prompts.say(&format!("Rules: {} pins, {} exclusion groups, {} delays, {} priorities. Definitions: {} Sources, {} hooks, {} package declarations.", config.pins.len(), config.exclude.len(), config.delay.len(), config.repo_order.len(), config.sources.len(), config.hooks.len(), package_count))?;
    prompts.say("Missing managers.toml / sources.toml / hooks.toml templates will be created beside config.toml. Existing supplemental files are kept byte for byte.")?;
    if backup {
        prompts.say("The previous config.toml will be backed up beside it before replacement.")?;
    }
    if config.features.shell_aliases {
        prompts.say("After saving, existing ~/.bashrc and ~/.zshrc may receive the pin alias if it is absent.")?;
    }
    Ok(())
}

pub(super) fn next_steps<R: BufRead, W: Write>(prompts: &mut Prompts<R, W>) -> Result<()> {
    prompts.section("Next steps")?;
    for line in [
        "pacpin config validate  - validate all four configuration files",
        "pacpin check  - check configured managers without upgrading",
        "pacpin -Syu --dry-run  - review the eligible upgrade plan",
        "pacpin -Syu --sources-only  - upgrade configured Sources only",
        "pacpin -Syu --aur-only  - upgrade AUR packages only",
        "pacpin -Syu --external-only  - upgrade enabled external managers only",
        "pacpin run --use-ram --bin java jdk-openjdk -- --version  - temporary native app workspace",
        "pacpin capture --dry-run / pacpin rebuild --dry-run  - preview declarative inventory changes",
        "pacpin --help  - commands and flags; README.md and docs/sources.md have full recipes",
    ] { prompts.say(line)?; }
    Ok(())
}
