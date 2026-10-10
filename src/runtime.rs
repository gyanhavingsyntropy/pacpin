//! Invocation-only controls; never persisted or forwarded to managers.
use crate::scratch::Policy;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpgradeScope {
    #[default]
    All,
    Sources,
    Aur,
}
impl UpgradeScope {
    pub fn parse(flags: &[String]) -> Result<Self, String> {
        match (
            has_control(flags, "--sources-only"),
            has_control(flags, "--aur-only"),
        ) {
            (true, true) => Err("--sources-only and --aur-only are mutually exclusive".into()),
            (true, false) => Ok(Self::Sources),
            (false, true) => Ok(Self::Aur),
            _ => Ok(Self::All),
        }
    }
    pub fn includes_native(self) -> bool {
        self == Self::All
    }
    pub fn includes_aur(self) -> bool {
        self != Self::Sources
    }
    pub fn includes_runner(self, name: &str) -> bool {
        self == Self::All || self == Self::Sources && name == "Sources"
    }
}
pub fn is_owned_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--keep-going" | "--sources-only" | "--aur-only" | "--use-ram" | "--use-storage"
    )
}
pub fn has_control(flags: &[String], control: &str) -> bool {
    let mut value = false;
    for flag in flags {
        if value {
            value = false;
            continue;
        }
        if flag == control {
            return true;
        }
        value = crate::commands::cli::takes_value(flag);
    }
    false
}
pub fn manager_flags(flags: &[String]) -> Vec<String> {
    let mut value = false;
    flags
        .iter()
        .filter(|flag| {
            if value {
                value = false;
                return true;
            }
            value = crate::commands::cli::takes_value(flag);
            !is_owned_flag(flag)
        })
        .cloned()
        .collect()
}
pub fn parse(args: &mut Vec<String>) -> Result<Policy, String> {
    let upgrade = crate::commands::cli::is_upgrade_invocation(args)
        && !crate::commands::cli::is_refresh_only_invocation(args);
    let install = args.first().is_some_and(|s| {
        s == "install" || s.starts_with("-S") && s[2..].chars().all(|c| matches!(c, 'y' | 'n'))
    });
    let mut policy = Policy::Auto;
    let mut output = Vec::new();
    let mut value = false;
    let mut targets = false;
    let mut controls = Vec::new();
    for arg in args.iter() {
        if targets || value {
            output.push(arg.clone());
            value = false;
            continue;
        }
        if arg == "--" {
            targets = true;
            output.push(arg.clone());
            continue;
        }
        if arg
            .split_once('=')
            .is_some_and(|(key, _)| is_owned_flag(key))
        {
            return Err(format!("{arg}: this flag does not take a value"));
        }
        if is_owned_flag(arg) {
            if !upgrade && (!install || !matches!(arg.as_str(), "--use-ram" | "--use-storage")) {
                return Err(format!(
                    "{arg} is only supported for {}",
                    if arg.starts_with("--use-") {
                        "installation and upgrades"
                    } else {
                        "upgrades"
                    }
                ));
            }
            if matches!(arg.as_str(), "--use-ram" | "--use-storage") {
                let selected = if arg == "--use-ram" {
                    Policy::Ram
                } else {
                    Policy::Storage
                };
                if policy != Policy::Auto && policy != selected {
                    return Err("--use-ram and --use-storage are mutually exclusive".into());
                }
                policy = selected;
                continue;
            }
            controls.push(arg.clone());
        }
        value = crate::commands::cli::takes_value(arg);
        output.push(arg.clone());
    }
    UpgradeScope::parse(&controls)?;
    *args = output;
    Ok(policy)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).into()).collect()
    }
    #[test]
    fn flags_validated_and_scratch_flags_consumed() {
        let mut input = args(&["-Syu", "--aur-only", "--use-ram", "--keep-going"]);
        assert_eq!(parse(&mut input).unwrap(), Policy::Ram);
        assert_eq!(input, args(&["-Syu", "--aur-only", "--keep-going"]));
        for values in [
            vec!["-Syu", "--sources-only", "--aur-only"],
            vec!["-Syu", "--use-ram", "--use-storage"],
            vec!["clean", "--use-ram"],
            vec!["-Sy", "--aur-only"],
            vec!["-S", "app", "--keep-going"],
            vec!["-Syu", "--use-ram=yes"],
        ] {
            assert!(parse(&mut args(&values)).is_err(), "{values:?}");
        }
    }
    #[test]
    fn values_and_explicit_targets_are_not_controls() {
        let mut input = args(&["-Syu", "--ignore", "--use-ram", "--", "--use-storage"]);
        assert_eq!(parse(&mut input).unwrap(), Policy::Auto);
        assert_eq!(input.len(), 5);
        assert_eq!(
            UpgradeScope::parse(&["--ignore".into(), "--sources-only".into()]).unwrap(),
            UpgradeScope::All
        );
        assert!(!has_control(
            &["--ignore".into(), "--keep-going".into()],
            "--keep-going"
        ));
        assert_eq!(
            manager_flags(&[
                "--ignore".into(),
                "--sources-only".into(),
                "--keep-going".into()
            ]),
            vec!["--ignore", "--sources-only"]
        );
    }
    #[test]
    fn scope_membership_is_explicit() {
        assert!(!UpgradeScope::Sources.includes_native());
        assert!(!UpgradeScope::Sources.includes_aur());
        assert!(UpgradeScope::Sources.includes_runner("Sources"));
        assert!(!UpgradeScope::Sources.includes_runner("Flatpak"));
        assert!(UpgradeScope::Aur.includes_aur());
        assert!(!UpgradeScope::Aur.includes_runner("Sources"));
        assert!(UpgradeScope::All.includes_native());
    }
}
