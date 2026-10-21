//! Explicit, once-per-upgrade actions triggered by verified package changes.
use crate::{config::Config, db::AlpmManager};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Hook {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch_all: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unless_updated: Vec<String>,
    #[serde(flatten)]
    pub action: Action,
}

impl<'de> Deserialize<'de> for Hook {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // An explicit wire enum rejects unknown fields; serde flatten together
        // with deny_unknown_fields cannot deserialize tagged actions reliably.
        #[derive(Deserialize)]
        #[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
        enum Wire {
            Script {
                #[serde(default)]
                watch: Vec<String>,
                #[serde(default)]
                watch_all: Vec<String>,
                #[serde(default)]
                unless_updated: Vec<String>,
                command: Vec<String>,
            },
            RebuildAur {
                #[serde(default)]
                watch: Vec<String>,
                #[serde(default)]
                watch_all: Vec<String>,
                #[serde(default)]
                unless_updated: Vec<String>,
                packages: Vec<String>,
            },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Script {
                watch,
                watch_all,
                unless_updated,
                command,
            } => Hook {
                watch,
                watch_all,
                unless_updated,
                action: Action::Script { command },
            },
            Wire::RebuildAur {
                watch,
                watch_all,
                unless_updated,
                packages,
            } => Hook {
                watch,
                watch_all,
                unless_updated,
                action: Action::RebuildAur { packages },
            },
        })
    }
}

impl Hook {
    fn positive_names(&self) -> impl Iterator<Item = &String> {
        self.watch.iter().chain(&self.watch_all)
    }
    fn matches(&self, verified: &BTreeSet<String>, changed: &BTreeSet<String>) -> bool {
        (self.watch.is_empty() || self.watch.iter().any(|p| verified.contains(p)))
            && self.watch_all.iter().all(|p| verified.contains(p))
            && !self.unless_updated.iter().any(|p| changed.contains(p))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    Script { command: Vec<String> },
    RebuildAur { packages: Vec<String> },
}

pub fn validate(config: &Config) -> Result<(), String> {
    for (name, hook) in &config.hooks {
        if !crate::utils::is_safe_pkg_name(name)
            || (hook.watch.is_empty() && hook.watch_all.is_empty())
            || hook
                .positive_names()
                .chain(&hook.unless_updated)
                .any(|p| !crate::utils::is_safe_pkg_name(p))
        {
            return Err(format!(
                "Hook '{name}' needs a safe name and nonempty package watch list"
            ));
        }
        match &hook.action {
            Action::Script { command } => {
                if command.is_empty()
                    || !Path::new(&command[0]).is_absolute()
                    || command.iter().any(|arg| arg.contains('\0'))
                {
                    return Err(format!(
                        "Hook '{name}' needs an absolute executable and valid arguments"
                    ));
                }
            }
            Action::RebuildAur { packages } => {
                if packages.is_empty()
                    || packages.iter().any(|p| !crate::utils::is_safe_pkg_name(p))
                {
                    return Err(format!("Hook '{name}' needs a nonempty AUR package list"));
                }
            }
        }
    }
    Ok(())
}

pub fn versions(manager: &AlpmManager) -> BTreeMap<String, String> {
    manager
        .handle()
        .localdb()
        .pkgs()
        .into_iter()
        .map(|p| (p.name().to_owned(), p.version().to_string()))
        .collect()
}

#[derive(Default)]
pub struct Plan {
    entries: Vec<(String, Hook, Vec<String>)>,
    before: BTreeMap<String, String>,
    expected: BTreeMap<String, String>,
}

impl Plan {
    pub fn new(
        config: &Config,
        before: BTreeMap<String, String>,
        expected: BTreeMap<String, String>,
        scope: crate::runtime::UpgradeScope,
        aur_available: bool,
    ) -> Result<Self, String> {
        validate(config)?;
        if std::env::var_os("PACPIN_HOOK").is_some() {
            if !config.hooks.is_empty() {
                println!(":: Post-update hooks skipped in a hook subprocess.");
            }
            return Ok(Self::default());
        }
        let mut entries = Vec::new();
        let projected: BTreeSet<_> = expected
            .iter()
            .filter(|(p, v)| before.contains_key(*p) && before.get(*p) != Some(*v))
            .map(|(p, _)| p.clone())
            .collect();
        for (name, hook) in &config.hooks {
            let matched: Vec<_> = hook
                .positive_names()
                .filter(|p| projected.contains(*p))
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            if matched.is_empty() {
                continue;
            }
            if !hook.matches(&projected, &projected) {
                println!(":: Hook {name} skipped: update conditions are not met by this plan.");
                continue;
            }
            if matches!(hook.action, Action::RebuildAur { .. })
                && (scope == crate::runtime::UpgradeScope::Sources || !aur_available)
            {
                println!(":: Hook {name} skipped: AUR rebuilding is outside scope or AUR is ineligible for this run.");
                continue;
            }
            match &hook.action {
                Action::Script { command } => {
                    if !crate::utils::is_executable_file(Path::new(&command[0])) {
                        return Err(format!(
                            "Hook '{name}' executable is unavailable: {}",
                            command[0]
                        ));
                    }
                }
                Action::RebuildAur { packages } => {
                    crate::pm::validate_rebuild(config, packages)?;
                }
            }
            entries.push((name.clone(), hook.clone(), matched));
        }
        Ok(Self {
            entries,
            before,
            expected,
        })
    }

    pub fn preview(&self) {
        if self.entries.is_empty() {
            return;
        }
        println!("\nPost-update hooks (after verified updates):");
        for (name, hook, matched) in &self.entries {
            let action = match &hook.action {
                Action::Script { command } => format!(
                    "run {}",
                    command
                        .iter()
                        .map(|s| crate::utils::shell_quote(s))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                Action::RebuildAur { packages } => format!("rebuild AUR: {}", packages.join(", ")),
            };
            println!("  • {name}: {action} (watching {})", matched.join(", "));
            if !hook.watch_all.is_empty() {
                println!("    Requires all updated: {}", hook.watch_all.join(", "));
            }
            if !hook.unless_updated.is_empty() {
                println!("    Skip if updated: {}", hook.unless_updated.join(", "));
            }
        }
    }

    pub fn has_aur(&self) -> bool {
        self.entries
            .iter()
            .any(|(_, hook, _)| matches!(hook.action, Action::RebuildAur { .. }))
    }

    pub fn is_empty(&self) -> bool { self.entries.is_empty() }

    pub fn run(&self, config: &Config, successful: bool, noconfirm: bool) -> Outcome {
        let mut outcome = Outcome::default();
        if self.entries.is_empty() {
            return outcome;
        }
        if !successful {
            println!(":: Post-update hooks skipped: an earlier transaction failed.");
            return outcome;
        }
        if unsafe { libc::geteuid() } == 0 {
            eprintln!(":: Post-update hooks refused: start pacpin as a regular user.");
            outcome.success = false;
            return outcome;
        }
        let after = match crate::progress::operation(crate::progress::Manager::Checks, "Verifying hook conditions", || AlpmManager::with_repo_order(&config.repo_order)) {
            Ok(manager) => versions(&manager),
            Err(error) => {
                eprintln!(":: Cannot verify updates for hooks: {error}");
                outcome.success = false;
                return outcome;
            }
        };
        let verified: BTreeSet<_> = verified_changes(
            &self.before,
            &self.expected,
            &after,
            &self.expected.keys().cloned().collect::<Vec<_>>(),
        )
        .into_iter()
        .collect();
        let actual: BTreeSet<_> = after
            .iter()
            .filter(|(p, v)| self.before.contains_key(*p) && self.before.get(*p) != Some(*v))
            .map(|(p, _)| p.clone())
            .collect();
        let mut ran = false;
        for (name, hook, matched) in &self.entries {
            let changed = verified_changes(&self.before, &self.expected, &after, matched);
            if changed.is_empty() || !hook.matches(&verified, &actual) {
                println!(":: Hook {name} skipped: verified update conditions are not met.");
                continue;
            }
            println!(
                "\n:: Running post-update hook: {name} ({})",
                changed.join(", ")
            );
            ran = true;
            let result = match &hook.action {
                Action::Script { command } => {
                    let mut child = Command::new(&command[0]);
                    child
                        .args(&command[1..])
                        .env("PACPIN_UPDATED_PACKAGES", changed.join(" "))
                        .env("PACPIN_HOOK", name)
                        .current_dir("/");
                    crate::progress::run(
                        &mut child,
                        crate::progress::Manager::Hooks,
                        "Running user script",
                        || {},
                    )
                    .map(|s| {
                        if !s.success() {
                            eprintln!(":: Hook {name}: script exited with {s}");
                        }
                        s.success()
                    })
                    .map_err(|e| e.to_string())
                }
                Action::RebuildAur { packages } => {
                    let report = crate::progress::context(crate::progress::Manager::Hooks, "Rebuilding triggered packages", || crate::pm::rebuild_aur(config, packages, noconfirm));
                    if let Some(success) = report.cache {
                        outcome.aur = Some(outcome.aur.unwrap_or(true) && success);
                    }
                    report.result
                }
            };
            let success = result.as_ref().is_ok_and(|ok| *ok);
            if let Err(error) = result {
                eprintln!(":: Hook {name}: {error}");
            }
            println!(
                ":: Hook {name}: {}",
                if success { "completed" } else { "failed" }
            );
            outcome.success &= success;
            if let Err(error) = crate::journal::TransactionJournal::record_transaction(
                "post-update-hook",
                vec![],
                &format!("{name}: {}", if success { "completed" } else { "failed" }),
            ) {
                eprintln!(":: Could not record hook outcome: {error}");
            }
            if !success {
                println!(":: Remaining post-update hooks skipped after hook failure.");
                break;
            }
        }
        if ran {
            if outcome.success { crate::progress::completed(crate::progress::Manager::Hooks); }
            else { crate::progress::failed(crate::progress::Manager::Hooks); }
        }
        outcome
    }
}

pub struct Outcome {
    pub success: bool,
    pub aur: Option<bool>,
}
impl Default for Outcome {
    fn default() -> Self {
        Self {
            success: true,
            aur: None,
        }
    }
}

fn verified_changes(
    before: &BTreeMap<String, String>,
    expected: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
    matched: &[String],
) -> Vec<String> {
    matched
        .iter()
        .filter(|p| {
            before.contains_key(*p)
                && after.get(*p) == expected.get(*p)
                && after.get(*p) != before.get(*p)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hooks_round_trip_and_reject_invalid_actions() {
        let text = "[hooks.blur]\nwatch = ['mutter', 'gnome-shell']\naction = 'rebuild-aur'\npackages = ['gnome-rounded-blur']\n[hooks.user]\nwatch = ['mutter']\naction = 'script'\ncommand = ['/bin/true', 'literal;argument']\n";
        let config: Config = toml::from_str(text).unwrap();
        validate(&config).unwrap();
        assert_eq!(
            toml::from_str::<Config>(&toml::to_string(&config).unwrap()).unwrap(),
            config
        );
        assert!(toml::from_str::<Config>(&text.replace("rebuild-aur", "shell")).is_err());
        assert!(toml::from_str::<Config>(&text.replace("packages =", "typo =")).is_err());
        let bad: Config = toml::from_str(&text.replace("/bin/true", "true")).unwrap();
        assert!(validate(&bad).is_err());
    }
    #[test]
    fn only_verified_version_changes_trigger_once_per_hook() {
        let before = BTreeMap::from([
            ("mutter".into(), "50-1".into()),
            ("gnome-shell".into(), "50-1".into()),
        ]);
        let expected = BTreeMap::from([
            ("mutter".into(), "51-1".into()),
            ("gnome-shell".into(), "51-1".into()),
        ]);
        let watched = vec!["mutter".into(), "gnome-shell".into()];
        assert!(verified_changes(&before, &expected, &before, &watched).is_empty());
        assert_eq!(
            verified_changes(&before, &expected, &expected, &watched),
            watched
        );
        let mismatch = BTreeMap::from([("mutter".into(), "52-1".into())]);
        assert!(verified_changes(&before, &expected, &mismatch, &watched).is_empty());
    }
    #[test]
    fn multiple_watched_updates_schedule_one_action_and_failed_runs_skip() {
        let config: Config = toml::from_str("[hooks.example]\nwatch=['mutter','gnome-shell']\naction='script'\ncommand=['/bin/true']").unwrap();
        let before = BTreeMap::from([
            ("mutter".into(), "50-1".into()),
            ("gnome-shell".into(), "50-1".into()),
        ]);
        let expected = BTreeMap::from([
            ("mutter".into(), "51-1".into()),
            ("gnome-shell".into(), "51-1".into()),
        ]);
        let plan = Plan::new(
            &config,
            before.clone(),
            expected,
            crate::runtime::UpgradeScope::All,
            true,
        )
        .unwrap();
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].2.len(), 2);
        assert!(!plan.has_aur());
        assert!(plan.run(&config, false, true).success);
        assert!(
            Plan::new(
                &config,
                before.clone(),
                before,
                crate::runtime::UpgradeScope::All,
                true,
            )
            .unwrap()
            .entries
            .is_empty()
        );
    }
    #[test]
    fn ineligible_aur_blocks_rebuilds_but_preserves_verified_script_actions() {
        let config: Config = toml::from_str("[hooks.script]\nwatch=['app']\naction='script'\ncommand=['/bin/true']\n[hooks.rebuild]\nwatch=['app']\naction='rebuild-aur'\npackages=['uninstalled-aur-package']").unwrap();
        let plan = Plan::new(&config, BTreeMap::from([("app".into(), "1-1".into())]),
            BTreeMap::from([("app".into(), "2-1".into())]), crate::runtime::UpgradeScope::All, false).unwrap();
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].0, "script");
        assert!(!plan.has_aur());
    }

    #[test]
    fn any_all_and_unless_conditions_combine() {
        let config: Config = toml::from_str("[hooks.example]\nwatch=['a','b']\nwatch_all=['c','d']\nunless_updated=['e']\naction='script'\ncommand=['/bin/true']").unwrap();
        validate(&config).unwrap();
        let hook = &config.hooks["example"];
        let set = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        let valid = set(&["a", "c", "d"]);
        assert!(hook.matches(&valid, &valid));
        assert!(!hook.matches(&set(&["c", "d"]), &valid));
        assert!(!hook.matches(&set(&["a", "c"]), &valid));
        assert!(!hook.matches(&valid, &set(&["a", "c", "d", "e"])));
        // Even an unplanned version change can veto, but cannot satisfy a positive condition.
        assert!(!hook.matches(&set(&["a"]), &valid));
        assert_eq!(
            toml::from_str::<Config>(&toml::to_string(&config).unwrap()).unwrap(),
            config
        );
    }
    #[test]
    fn all_only_is_supported_and_negative_only_is_rejected() {
        let config: Config = toml::from_str(
            "[hooks.example]\nwatch_all=['a','b']\naction='script'\ncommand=['/bin/true']",
        )
        .unwrap();
        validate(&config).unwrap();
        let only_negative: Config = toml::from_str(
            "[hooks.example]\nunless_updated=['a']\naction='script'\ncommand=['/bin/true']",
        )
        .unwrap();
        assert!(validate(&only_negative).is_err());
    }
}
