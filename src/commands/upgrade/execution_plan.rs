//! A review of staged work, never a purported shell script for the transaction.
use crate::integrations::{ExternalUpdate, IntegrationProvider, UpgradeAction};
use std::sync::Arc;

pub(super) enum OrphanReview {
    None,
    Interactive(usize),
    Automatic(usize),
}
pub(super) struct Input<'a> {
    pub native: Option<(String, usize)>,
    pub aur: Option<(String, usize)>,
    pub providers: &'a [Arc<dyn IntegrationProvider>],
    pub updates: &'a [ExternalUpdate],
    pub orphans: OrphanReview,
    pub native_during_review: bool,
    pub hooks: bool,
    pub aur_hook: bool,
    pub no_cache: bool,
}
struct Stage {
    name: String,
    count: usize,
    preparation: String,
    actions: Vec<UpgradeAction>,
}
pub(super) struct Plan {
    stages: Vec<Stage>,
    orphans: OrphanReview,
    native_during_review: bool,
    hooks: bool,
    caches: Vec<String>,
}
impl Plan {
    pub(super) fn new(input: Input<'_>) -> Result<Self, String> {
        let mut stages = Vec::new();
        let mut caches = Vec::new();
        for (name, command, preparation) in [
            ("Pacman", input.native, "Pacman downloads, verifies and installs repository packages."),
            ("AUR", input.aur, "The unprivileged helper owns fetching, building and installation; no separate AUR download-only job."),
        ] {
            if let Some((command, count)) = command {
                stages.push(Stage { name: name.into(), count, preparation: preparation.into(), actions: vec![UpgradeAction::Command(command)] });
                if input.no_cache { caches.push(name.into()); }
            }
        }
        if input.no_cache && input.aur_hook && !caches.iter().any(|name| name == "AUR") {
            caches.push("AUR (if a planned rebuild hook runs successfully)".into());
        }
        for provider in input.providers {
            let updates: Vec<_> = input
                .updates
                .iter()
                .filter(|update| update.runner == provider.name())
                .cloned()
                .collect();
            if updates.is_empty() {
                continue;
            }
            let actions = provider
                .upgrade_actions(&updates)
                .map_err(|error| format!("{} plan: {error}", provider.name()))?;
            if actions.is_empty() {
                return Err(format!(
                    "{} returned no actions for approved updates",
                    provider.name()
                ));
            }
            stages.push(Stage {
                name: provider.name().into(),
                count: updates.len(),
                preparation: provider.preparation_policy().description().into(),
                actions,
            });
            if input.no_cache && provider.cache_command_str().is_some() {
                caches.push(provider.name().into());
            }
        }
        Ok(Self {
            stages,
            orphans: input.orphans,
            native_during_review: input.native_during_review,
            hooks: input.hooks,
            caches,
        })
    }

    pub(super) fn lines(&self) -> Vec<String> {
        let mut labels: Vec<_> = self.stages.iter().map(|stage| stage.name.clone()).collect();
        if self.hooks { labels.push("Hooks (if triggered)".into()); }
        match self.orphans {
            OrphanReview::Interactive(_) => labels.push("Orphan review".into()),
            OrphanReview::Automatic(count) => labels.push(format!("Orphan cleanup ({count}, on success)")),
            OrphanReview::None => {},
        }
        if self.has_cache_cleanup() { labels.push("Cache cleanup (on success)".into()); }
        vec![format!("Actions: {}", labels.join(" · "))]
    }

    pub(super) fn has_cache_cleanup(&self) -> bool { !self.caches.is_empty() }

    fn dry_run_lines(&self) -> Vec<String> {
        let mut lines = self.lines();
        match self.orphans {
            OrphanReview::Interactive(count) => lines.push(format!("Orphans: review {count} candidates after approval; remove selected, still-unneeded packages only after whole-run success.")),
            OrphanReview::Automatic(count) => lines.push(format!("Orphans: --autoremove selects {count} candidates, revalidated after whole-run success.")),
            OrphanReview::None => {},
        }
        if self.native_during_review {
            lines.push("Repository downloads may overlap orphan review.".into());
        }
        for stage in &self.stages {
            lines.push(format!(
                "{} ({} approved {})",
                stage.name,
                stage.count,
                if stage.count == 1 {
                    "target"
                } else {
                    "targets"
                }
            ));
            lines.push(format!("Preparation: {}", stage.preparation));
            for action in &stage.actions {
                lines.push(match action {
                    UpgradeAction::Command(command) => format!("Base command: {command}"),
                    UpgradeAction::Internal(action) => format!("Internal action: {action}"),
                });
            }
        }
        if !self.caches.is_empty() {
            lines.push(format!("Cache cleanup (successful managers): {}", self.caches.join(", ")));
        }
        lines
    }

    pub(super) fn render(&self, dry_run: bool) {
        let lines = if dry_run { self.dry_run_lines() } else { self.lines() };
        for (index, text) in lines.iter().enumerate() {
            let prefix = if index == 0 { if dry_run { "Dry-Run: " } else { ":: " } } else { "  " };
            for line in crate::ui::wrap_field(text, prefix, "    ", crate::ui::get_terminal_width())
            {
                println!("{line}");
            }
        }
        println!();
    }

    pub(super) fn summary(&self) -> String {
        self.stages
            .iter()
            .flat_map(|stage| {
                stage
                    .actions
                    .iter()
                    .map(|action| format!("{}: {}", stage.name, action.description()))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> Input<'static> {
        Input {
            native: None,
            aur: None,
            providers: &[],
            updates: &[],
            orphans: OrphanReview::None,
            native_during_review: false,
            hooks: false,
            aur_hook: false,
            no_cache: false,
        }
    }
    #[test]
    fn native_preview_shows_later_orphan_decision_and_conditional_cleanup() {
        let plan = Plan::new(Input {
            native: Some(("sudo pacman -S -- core/app".into(), 1)),
            orphans: OrphanReview::Interactive(7),
            native_during_review: true,
            no_cache: true,
            hooks: true,
            ..input()
        })
        .unwrap();
        assert_eq!(plan.lines(), ["Actions: Pacman · Hooks (if triggered) · Orphan review · Cache cleanup (on success)"]);
        let text = plan.dry_run_lines().join("\n");
        assert!(text.contains("review 7 candidates after approval"));
        assert!(text.contains("whole-run success"));
        assert!(text.contains("Cache cleanup (successful managers): Pacman"));
        assert!(!text.contains("Restart advisory") && !text.contains(" && "));
    }
    #[test]
    fn aur_only_and_no_review_do_not_promise_native_downloads_or_orphan_removal() {
        let plan = Plan::new(Input {
            aur: Some(("paru -S --aur -- app".into(), 1)),
            ..input()
        })
        .unwrap();
        let text = plan.lines().join("\n");
        assert_eq!(text, "Actions: AUR");
        assert!(
            !text.contains("Pacman (")
                && !text.contains("Orphan cleanup:")
                && !text.contains("Cache cleanup")
        );
    }
    #[test]
    fn automatic_orphan_selection_still_requires_revalidation_and_success() {
        let plan = Plan::new(Input {
            native: Some(("pacman -S -- app".into(), 1)),
            orphans: OrphanReview::Automatic(3),
            ..input()
        })
        .unwrap();
        assert_eq!(plan.lines(), ["Actions: Pacman · Orphan cleanup (3, on success)"]);
        let text = plan.dry_run_lines().join("\n");
        assert!(text.contains("--autoremove selects 3"));
        assert!(text.contains("revalidated after whole-run success"));
    }

    struct Provider {
        name: &'static str,
        policy: crate::integrations::preparation::Policy,
        actions: Option<Vec<UpgradeAction>>,
    }
    impl IntegrationProvider for Provider {
        fn cache_usage(&self) -> Option<crate::pm::CacheUsage> { Some(Default::default()) }
        fn name(&self) -> &str {
            self.name
        }
        fn is_available(&self) -> bool {
            true
        }
        fn refresh_metadata(&self) -> std::io::Result<()> {
            unreachable!()
        }
        fn check_updates(&self) -> crate::discovery::Report<ExternalUpdate> {
            unreachable!()
        }
        fn upgrade_actions(&self, _: &[ExternalUpdate]) -> std::io::Result<Vec<UpgradeAction>> {
            self.actions
                .clone()
                .ok_or_else(|| std::io::Error::other("invalid approved plan"))
        }
        fn execute_upgrade(&self, _: &[ExternalUpdate]) -> std::io::Result<bool> {
            panic!("preview must not execute")
        }
        fn check_options(
            &self,
            _: crate::scratch::Policy,
            _: bool,
        ) -> Result<(), crate::runtime::OptionError> {
            unreachable!()
        }
        fn clean_command_str(&self) -> String {
            unreachable!()
        }
        fn execute_clean(&self) -> std::io::Result<bool> {
            panic!("preview must not clean")
        }
        fn preparation_policy(&self) -> crate::integrations::preparation::Policy {
            self.policy
        }
    }
    #[test]
    fn internal_sources_and_each_external_manager_have_separate_typed_stages() {
        use crate::integrations::preparation::Policy;
        let providers: Vec<Arc<dyn IntegrationProvider>> = [
            Provider {
                name: "Sources",
                policy: Policy::Background,
                actions: Some(vec![UpgradeAction::Internal(
                    "approved/release -> 2; package and install locally".into(),
                )]),
            },
            Provider {
                name: "Flatpak",
                policy: Policy::BeforeStage,
                actions: Some(vec![UpgradeAction::Command(
                    "flatpak update --commit=approved -- app/ref".into(),
                )]),
            },
            Provider {
                name: "Nix",
                policy: Policy::Unsupported("fetch during upgrade"),
                actions: Some(vec![UpgradeAction::Command(
                    "nix profile upgrade -- app".into(),
                )]),
            },
            Provider {
                name: "unused",
                policy: Policy::Unsupported("unused"),
                actions: None,
            },
        ]
        .into_iter()
        .map(|p| Arc::new(p) as Arc<dyn IntegrationProvider>)
        .collect();
        let updates: Vec<_> = ["Sources", "Flatpak", "Nix"]
            .into_iter()
            .map(|name| ExternalUpdate {
                runner: name.into(),
                ..Default::default()
            })
            .collect();
        let plan = Plan::new(Input {
            providers: &providers,
            updates: &updates,
            no_cache: true,
            ..input()
        })
        .unwrap();
        assert_eq!(plan.lines(), ["Actions: Sources · Flatpak · Nix"]);
        let text = plan.dry_run_lines().join("\n");
        for label in [
            "Sources (1 approved target)",
            "Flatpak (1 approved target)",
            "Nix (1 approved target)",
            "Internal action:",
            "Base command: flatpak",
            "immediately before this manager",
        ] {
            assert!(text.contains(label), "{label}: {text}");
        }
        for absent in [
            "unused (",
            "Pacman (",
            "AUR (",
            "Cache cleanup",
            "Orphan cleanup",
            "Restart advisory",
            "pacpin -S",
            " && ",
        ] {
            assert!(!text.contains(absent), "{absent}: {text}");
        }
        assert!(plan.summary().contains("Sources: Internal:"));
        for text in plan.lines() {
            for line in crate::ui::wrap_field(&text, "  ", "    ", 32) {
                assert!(crate::ui::str_width(&line) <= 32);
            }
        }
    }
    #[test]
    fn invalid_or_empty_provider_actions_refuse_the_preview() {
        for actions in [None, Some(Vec::new())] {
            let providers: Vec<Arc<dyn IntegrationProvider>> = vec![Arc::new(Provider {
                name: "broken",
                policy: crate::integrations::preparation::Policy::BeforeStage,
                actions,
            })];
            let updates = vec![ExternalUpdate {
                runner: "broken".into(),
                ..Default::default()
            }];
            assert!(Plan::new(Input {
                providers: &providers,
                updates: &updates,
                ..input()
            })
            .is_err());
        }
    }
}
