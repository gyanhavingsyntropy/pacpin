//! Edit supported preferences without reconstructing or flattening Config.
use super::prompts::{Prompts, Result};
use crate::config::Config;
use std::io::{BufRead, Write};

pub(super) fn configure<R: BufRead, W: Write>(
    prompts: &mut Prompts<R, W>,
    mut config: Config,
    fresh: bool,
) -> Result<Config> {
    tools(prompts, &mut config, fresh)?;
    rules(prompts, &mut config)?;
    lifecycle(prompts, &mut config, fresh)?;
    managers(prompts, &mut config)?;
    prompts.section("[5/6] Sources, rebuild hooks and package declarations")?;
    prompts.say(&format!(
        "Retaining {} source definition(s) and {} hook(s).",
        config.sources.len(),
        config.hooks.len()
    ))?;
    prompts.say("sources.toml: GitHub/GitLab releases, catalogs and explicit source-build recipes. Builds inherit makepkg settings; CPU optimizations and LTO are opt-in per recipe.")?;
    prompts.say("hooks.toml: actions after verified package or repository changes; watch (any), watch_all (all) and unless_updated (none) select when they run. Only add commands you trust.")?;
    prompts.say("config.toml [packages]: declarative native/AUR/Sources/Flatpak inventory. capture records installed choices; rebuild previews or installs missing declarations. Setup never installs packages or runs hooks.")?;
    prompts.section("[6/6] Temporary apps and per-command controls")?;
    prompts.say("run preserves normal application writes. try uses a private home/tmp; eligible current projects are read-only unless --rw is used.")?;
    prompts.say("--use-ram / --use-storage choose temporary package/build workspaces for supported commands; they do not relocate installed apps, app documents, or external stores. Automatic placement checks live capacity and keeps a RAM reserve.")?;
    prompts.say("--sources-only / --aur-only / --external-only scope upgrades. --keep-going can skip supported independent failures, reports incomplete results, and does not make damaged native installations safe to continue.")?;
    prompts.say("External managers need explicit mappings for forced scratch and cache options. Edit managers.toml for capabilities and --MANAGER.FLAG actions; unsupported options are reported before execution.")?;
    Ok(config)
}

fn tools<R: BufRead, W: Write>(
    prompts: &mut Prompts<R, W>,
    config: &mut Config,
    fresh: bool,
) -> Result<()> {
    prompts.section("[1/6] Privilege tool and AUR helper")?;
    prompts.say("pacpin starts as your regular user. Only native package operations elevate; AUR helpers and makepkg stay unprivileged. Setup does not request a password.")?;
    let tool_override = config
        .manager_file
        .managers
        .get("pacman")
        .is_some_and(|d| d.privilege_tool.is_some());
    if tool_override {
        prompts.say(&format!(
            "Privilege tool: {} (owned by managers.toml; edit that file to change it).",
            config.options.privilege_tool
        ))?;
    } else {
        config.options.privilege_tool = prompts.value(
            "Privilege tool: sudo, pkexec or doas",
            &config.options.privilege_tool,
            |value| crate::config::validate_privilege_tool(value).map(str::to_string),
        )?;
    }
    unavailable(prompts, &config.options.privilege_tool)?;
    let helper_override = config
        .manager_file
        .managers
        .get("aur")
        .is_some_and(|d| d.helper.is_some());
    if helper_override {
        prompts.say(&format!(
            "AUR helper: {} (owned by managers.toml; edit that file to change it).",
            config.options.helper
        ))?;
    } else {
        let helper = if fresh {
            crate::config::SAFE_AUR_HELPERS
                .iter()
                .find(|name| crate::is_command_available(name))
                .map(|s| s.to_string())
                .unwrap_or_else(|| config.options.helper.clone())
        } else {
            config.options.helper.clone()
        };
        config.options.helper = prompts.value(
            "AUR helper name or trusted absolute executable path",
            &helper,
            |value| {
                let helper = crate::config::validate_helper(value)?;
                if std::path::Path::new(&helper).is_absolute()
                    && !crate::is_command_available(&helper)
                {
                    return Err("The helper path must be executable.".into());
                }
                Ok(helper)
            },
        )?;
    }
    unavailable(prompts, &config.options.helper)
}

fn unavailable<R: BufRead, W: Write>(prompts: &mut Prompts<R, W>, command: &str) -> Result<()> {
    if !crate::is_command_available(command) {
        prompts.say(&format!("'{command}' is not executable on PATH. The preference can be saved, but install it separately before using that operation."))?;
    }
    Ok(())
}

fn rules<R: BufRead, W: Write>(prompts: &mut Prompts<R, W>, config: &mut Config) -> Result<()> {
    prompts.section("[2/6] Repository rules and stability delays")?;
    prompts.say(&format!("Retaining {} pins, {} exclusion groups, {} delay rules and {} custom repository priorities. Use pin/unpin, delay/undelay and repos to edit individual rules.", config.pins.len(), config.exclude.len(), config.delay.len(), config.repo_order.len()))?;
    config.features.pinning = prompts.yes_no(
        "Enable configured repository pinning?",
        config.features.pinning,
    )?;
    config.features.vendor_stickiness = prompts.yes_no(
        "Prefer each installed package's originating repository?",
        config.features.vendor_stickiness,
    )?;
    config.features.stability_delays = prompts.yes_no(
        "Enable configured stability delays?",
        config.features.stability_delays,
    )?;
    if config.features.stability_delays
        && config.delay.is_empty()
        && prompts.yes_no("Add delay rules for known kernel packages?", false)?
    {
        let days = prompts.value("Kernel delay in days", "3", |value| {
            value
                .parse::<u32>()
                .map_err(|_| "Enter a nonnegative whole number of days.".into())
        })?;
        for package in crate::utils::KERNEL_PACKAGES {
            config.delay.entry(package.to_string()).or_insert(days);
        }
    }
    Ok(())
}

fn lifecycle<R: BufRead, W: Write>(
    prompts: &mut Prompts<R, W>,
    config: &mut Config,
    fresh: bool,
) -> Result<()> {
    prompts.section("[3/6] Orphans, caches and shell convenience")?;
    prompts.say("Smart orphan review distinguishes unneeded dependencies from optional plugins. Selected removal follows a successful upgrade; failed cleanup is offered again later.")?;
    config.features.smart_orphans = prompts.yes_no(
        "Review orphaned packages during full upgrades?",
        config.features.smart_orphans,
    )?;
    prompts.say("Keeping archives helps rollback and avoids repeat downloads. No-cache cleanup is scoped to successful manager work and requires that manager's cache support; it does not prune installed Flatpak runtimes.")?;
    config.options.no_cache = prompts.yes_no(
        "Use no-cache by default for upgrades?",
        config.options.no_cache,
    )?;
    config.features.shell_aliases = prompts.yes_no(
        "Maintain a pin=pacpin alias in existing Bash/Zsh shell files after saving?",
        !fresh && config.features.shell_aliases,
    )?;
    Ok(())
}

fn managers<R: BufRead, W: Write>(prompts: &mut Prompts<R, W>, config: &mut Config) -> Result<()> {
    prompts.section("[4/6] External managers")?;
    prompts.say("Select which managers participate in update checks. No manager is installed by setup. Explicit enabled settings and custom commands in managers.toml take precedence and are preserved.")?;
    let mut enable_legacy = false;
    for (name, selected) in [
        ("flatpak", &mut config.integrations.flatpak),
        ("nix", &mut config.integrations.nix),
        ("pipx", &mut config.integrations.pipx),
    ] {
        if let Some(enabled) = config
            .manager_file
            .managers
            .get(name)
            .and_then(|d| d.enabled)
        {
            prompts.say(&format!(
                "{name}: {} (owned by managers.toml).",
                if enabled { "enabled" } else { "disabled" }
            ))?;
            continue;
        }
        let available = crate::is_command_available(name);
        *selected = prompts.yes_no(
            &format!(
                "Include {name} updates? ({})",
                if available {
                    "executable found"
                } else {
                    "not installed; enabling will report unavailable"
                }
            ),
            config.features.integrations && *selected,
        )?;
        enable_legacy |= *selected;
    }
    // Per-manager toggles disable legacy choices independently. Preserve the
    // global preference when managers.toml owns all effective selections.
    config.features.integrations |= enable_legacy;
    for (name, definition) in &config.manager_file.managers {
        if definition.adapter == "command" {
            prompts.say(&format!(
                "{name}: custom adapter, {} in managers.toml.",
                if definition.enabled == Some(true) {
                    "enabled"
                } else {
                    "disabled"
                }
            ))?;
        }
    }
    Ok(())
}
