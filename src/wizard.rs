//! Interactive onboarding coordinates prompts; configuration owns persistence.
use crate::config::{self, Config};
use std::io::{self, IsTerminal};

mod preferences;
mod prompts;
mod review;
#[cfg(test)]
mod tests;

pub(crate) enum Action {
    Help,
    Setup { reset: bool },
}

pub(crate) fn parse_args(args: &[String]) -> Result<Action, String> {
    let mut reset = false;
    let mut help = false;
    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "--reset" | "-r" => reset = true,
            "--help" | "-h" => help = true,
            _ => {
                return Err(format!(
                    "Unsupported init option '{arg}'. Use pacpin init --help."
                ))
            }
        }
    }
    Ok(if help {
        Action::Help
    } else {
        Action::Setup { reset }
    })
}

pub(crate) fn print_help() {
    println!("pacpin init [--reset]  (alias: setup)");
    println!("Interactive setup for privileges, AUR, rules, managers and cache preferences.");
    println!("Requires an input/output terminal. Enter accepts the shown default; q cancels.");
    println!("Review before saving. Existing config.toml is backed up; supplemental files are preserved.");
    println!("--reset starts main preferences/rules/manifest from defaults, keeping supplemental definitions.");
    println!("Use config init to create missing managers.toml, sources.toml and hooks.toml without prompts.");
    println!("Scratch/scope/keep-going flags belong to individual operations, not init.");
}

pub(crate) fn run(reset: bool) -> Result<Option<Config>, String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("Interactive setup requires an input/output terminal. Use pacpin init in a terminal, or config init to create missing supplemental templates.".into());
    }
    if unsafe { libc::geteuid() } == 0 {
        return Err("Run setup as your regular user; it configures that user's files and needs no elevation.".into());
    }
    let path = config::get_config_path();
    let snapshot = config::setup::Snapshot::load(&path, reset)?;
    let fresh = !snapshot.has_main() || reset;
    let mut prompts = prompts::Prompts::new(io::stdin().lock(), io::stdout().lock());
    let result = questionnaire(
        &mut prompts,
        snapshot.config.clone(),
        fresh,
        &path,
        snapshot.has_main(),
        reset,
    );
    drop(prompts);
    let updated = match result {
        Ok(Some(updated)) => updated,
        Ok(None) | Err(prompts::Error::Cancelled) => {
            println!(":: Setup cancelled; no setup files or shell aliases were written.");
            return Ok(None);
        }
        Err(prompts::Error::Io(error)) => {
            return Err(format!("Setup input/output failed: {error}"))
        }
    };
    let backup = snapshot.save(&updated)?;
    println!(":: Configuration saved: {}", path.display());
    if let Some(backup) = backup {
        println!(":: Previous configuration backup: {}", backup.display());
    }
    match crate::manager_config::initialize(path.parent().unwrap_or_else(|| std::path::Path::new("."))) {
        Ok(created) => for file in created { println!(":: Created {file}"); },
        Err(error) => return Err(format!("Main configuration saved, but optional templates are incomplete: {error}. Run pacpin config init to retry.")),
    }
    if updated.features.shell_aliases {
        for file in crate::alias::AliasManager::setup_aliases() {
            println!(":: Added pin alias to {file}");
        }
        crate::alias::AliasManager::mark_notified();
    }
    let mut prompts = prompts::Prompts::new(io::empty(), io::stdout().lock());
    review::next_steps(&mut prompts)
        .map_err(|error| format!("Cannot print setup guidance: {error:?}"))?;
    Ok(Some(updated))
}

fn questionnaire<R: io::BufRead, W: io::Write>(
    prompts: &mut prompts::Prompts<R, W>,
    config: Config,
    fresh: bool,
    path: &std::path::Path,
    has_main: bool,
    reset: bool,
) -> prompts::Result<Option<Config>> {
    prompts.section(&format!("pacpin {} - setup", env!("CARGO_PKG_VERSION")))?;
    prompts.say("Enter accepts the current value. Type q at any prompt to cancel. Changes are saved only after the final review.")?;
    prompts.say(&format!("Configuration: {}", path.display()))?;
    if reset && has_main {
        prompts.say("Reset replaces main preferences, rules and package declarations. The old main file will be backed up; supplemental definitions stay in their files.")?;
    } else if has_main {
        prompts.say("Existing rules, package declarations, recipes, hooks and manager overrides are retained. Only the preferences you answer are changed.")?;
    }
    let updated = preferences::configure(prompts, config, fresh)?;
    review::show(prompts, &updated, path, has_main)?;
    if prompts.yes_no(
        "Save this configuration and create missing templates?",
        false,
    )? {
        Ok(Some(updated))
    } else {
        Ok(None)
    }
}
