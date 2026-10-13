//! Invocation-only controls; never persisted or forwarded to managers.
use crate::scratch::Policy;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpgradeScope {
    #[default]
    All,
    Sources,
    Aur,
    External,
}
impl UpgradeScope {
    pub fn parse(flags: &[String]) -> Result<Self, String> {
        let selected: Vec<_> = [("--sources-only", Self::Sources), ("--aur-only", Self::Aur), ("--external-only", Self::External)]
            .into_iter().filter(|(flag, _)| has_control(flags, flag)).collect();
        if selected.len() > 1 { return Err("--sources-only, --aur-only and --external-only are mutually exclusive".into()); }
        Ok(selected.first().map(|(_, scope)| *scope).unwrap_or(Self::All))
    }
    pub fn includes_native(self) -> bool {
        self == Self::All
    }
    pub fn includes_aur(self) -> bool {
        matches!(self, Self::All | Self::Aur)
    }
    pub fn includes_runner(self, name: &str) -> bool {
        self == Self::All || self == Self::Sources && name == "Sources" || self == Self::External && name != "Sources"
    }
}
pub fn is_owned_flag(flag: &str) -> bool {
    is_custom_flag(flag) || matches!(
        flag,
        "--keep-going" | "--sources-only" | "--aur-only" | "--external-only" | "--use-ram" | "--use-storage"
    )
}
pub fn is_custom_flag(flag: &str) -> bool {
    flag.starts_with("--") && flag.trim_start_matches("--").contains('.')
}
pub fn custom_flags(flags: &[String]) -> Vec<&str> {
    let mut value = false;
    flags.iter().filter_map(|arg| {
        if value { value = false; return None; }
        value = crate::commands::cli::takes_value(arg);
        is_custom_flag(arg).then_some(arg.as_str())
    }).collect()
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
        matches!(s.as_str(), "install" | "rebuild") || s.starts_with("-S") && s[2..].chars().all(|c| matches!(c, 'y' | 'n'))
    });
    let manager_action = args.first().is_some_and(|s| s == "manager");
    let ephemeral = args.first().is_some_and(|s| matches!(s.as_str(), "run" | "try"));
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
        if matches!(arg.as_str(), "--no-cache" | "--clean-cache") && !(upgrade || install || ephemeral) {
            return Err(format!("{arg} is supported for installation, rebuild, upgrades, run and try; use clean for explicit cache maintenance"));
        }
        if arg
            .split_once('=')
            .is_some_and(|(key, _)| is_owned_flag(key))
        {
            return Err(format!("{arg}: this flag does not take a value"));
        }
        if is_owned_flag(arg) {
            if !(upgrade || (install || ephemeral) && matches!(arg.as_str(), "--use-ram" | "--use-storage")
                || manager_action && is_custom_flag(arg)) {
                return Err(format!(
                    "{arg} is only supported for {}",
                    if arg.starts_with("--use-") {
                        "installation, upgrades, run and try"
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
        value = crate::commands::cli::takes_value(arg) || ephemeral && arg == "--bin";
        output.push(arg.clone());
    }
    UpgradeScope::parse(&controls)?;
    *args = output;
    Ok(policy)
}
/// Privilege selectors belong to pacpin only before the application delimiter.
pub fn extract_privilege_override(args: &mut Vec<String>) -> Result<Option<&'static str>, String> {
    let ephemeral = args.first().is_some_and(|arg| matches!(arg.as_str(), "run" | "try"));
    let mut selected = None;
    let mut output = Vec::new();
    let mut value = false;
    let mut targets = false;
    for arg in args.iter() {
        if targets || value {
            output.push(arg.clone());
            value = false;
            continue;
        }
        if arg == "--" { targets = true; }
        if matches!(arg.as_str(), "--sudo" | "--pkexec") {
            let tool = if arg == "--sudo" { "sudo" } else { "pkexec" };
            if selected.is_some_and(|previous| previous != tool) {
                return Err("--sudo and --pkexec are mutually exclusive".into());
            }
            selected = Some(tool);
        } else {
            value = crate::commands::cli::takes_value(arg) || ephemeral && arg == "--bin";
            output.push(arg.clone());
        }
    }
    *args = output;
    Ok(selected)
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
        let custom = args(&["--ignore", "--nix.example", "--other.actual"]);
        assert_eq!(custom_flags(&custom), ["--other.actual"]);
        assert_eq!(manager_flags(&custom), args(&["--ignore", "--nix.example"]));
        assert_eq!(UpgradeScope::parse(&args(&["--external-only"])).unwrap(), UpgradeScope::External);
        assert!(!UpgradeScope::External.includes_native()); assert!(!UpgradeScope::External.includes_aur());
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
    fn ephemeral_scratch_flags_compose_and_respect_program_arguments() {
        for mode in ["run", "try"] {
            let mut input = args(&[mode, "--use-ram", "--installed", "app", "--", "--use-storage"]);
            assert_eq!(parse(&mut input).unwrap(), Policy::Ram);
            assert_eq!(input, args(&[mode, "--installed", "app", "--", "--use-storage"]));
            let mut input = args(&[mode, "--bin", "--use-ram", "app"]);
            assert_eq!(parse(&mut input).unwrap(), Policy::Auto);
            assert!(parse(&mut args(&[mode, "--use-ram", "--use-storage", "app"])).is_err());
            assert!(parse(&mut args(&[mode, "--keep-going", "app"])).is_err());
        }
    }
    #[test]
    fn privilege_selectors_preserve_option_values_and_application_arguments() {
        let mut input = args(&["run", "--pkexec", "app", "--", "--sudo"]);
        assert_eq!(extract_privilege_override(&mut input).unwrap(), Some("pkexec"));
        assert_eq!(input, args(&["run", "app", "--", "--sudo"]));
        let mut input = args(&["run", "--bin", "--sudo", "app"]);
        assert_eq!(extract_privilege_override(&mut input).unwrap(), None);
        assert!(extract_privilege_override(&mut args(&["-Syu", "--sudo", "--pkexec"])).is_err());
        let mut input = args(&["rebuild", "--dry-run", "--use-storage"]);
        assert_eq!(parse(&mut input).unwrap(), Policy::Storage);
        for mode in ["check", "search", "info", "history", "capture", "clean", "remove", "orphans", "keep", "rollback", "config", "delay", "list", "manager"] {
            assert!(parse(&mut args(&[mode, "--use-ram"])).is_err(), "{mode}");
        }
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
