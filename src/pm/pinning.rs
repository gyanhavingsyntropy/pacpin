//! Pin intent comes from repository selection, never helper discovery.
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct RepositoryPinPlan {
    requested: BTreeMap<String, String>,
}

impl RepositoryPinPlan {
    pub(super) fn from_requests(targets: &[String]) -> Self {
        let mut plan = Self::default();
        for target in targets {
            if let Some((repo, package)) = target.split_once('/') {
                plan.select_companions(repo, &[package.to_string()]);
            }
        }
        plan
    }

    /// Called only for companions explicitly selected for a qualified target.
    pub(super) fn select_companions(&mut self, repo: &str, packages: &[String]) {
        let repo = if repo.eq_ignore_ascii_case("aur") {
            "aur"
        } else {
            repo
        };
        for package in packages {
            self.requested.insert(package.clone(), repo.to_string());
        }
    }

    pub(super) fn confirmed(&self, installed_targets: &[String]) -> Vec<(String, String)> {
        self.requested
            .iter()
            .filter(|(package, repo)| {
                installed_targets
                    .iter()
                    .any(|target| target == *package || target == &format!("{repo}/{package}"))
            })
            .map(|(package, repo)| (package.clone(), repo.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unqualified_native_and_aur_targets_never_request_pins() {
        let plan = RepositoryPinPlan::from_requests(&["native-app".into(), "aur-app".into()]);
        assert!(
            plan.confirmed(&["native-app".into(), "aur-app".into()])
                .is_empty()
        );
    }

    #[test]
    fn helper_discovered_dependencies_do_not_inherit_the_targets_pin() {
        let plan = RepositoryPinPlan::from_requests(&["aur/app".into()]);
        assert_eq!(
            plan.confirmed(&[
                "app".into(),
                "aur-dependency".into(),
                "native-dependency".into()
            ]),
            vec![("app".into(), "aur".into())]
        );
    }

    #[test]
    fn selected_companions_and_qualified_native_targets_keep_explicit_pins() {
        let mut plan = RepositoryPinPlan::from_requests(&["AUR/app".into(), "extra/native".into()]);
        plan.select_companions("AUR", &["selected-companion".into()]);
        assert_eq!(
            plan.confirmed(&[
                "app".into(),
                "selected-companion".into(),
                "extra/native".into()
            ]),
            vec![
                ("app".into(), "aur".into()),
                ("native".into(), "extra".into()),
                ("selected-companion".into(), "aur".into())
            ]
        );
    }

    #[test]
    fn failed_uninstalled_and_dry_run_targets_do_not_confirm_pins() {
        let plan = RepositoryPinPlan::from_requests(&["aur/app".into(), "extra/native".into()]);
        assert!(plan.confirmed(&[]).is_empty());
        assert_eq!(
            plan.confirmed(&["extra/native".into()]),
            vec![("native".into(), "extra".into())]
        );
        assert!(plan.confirmed(&["other/native".into()]).is_empty());
    }
}
