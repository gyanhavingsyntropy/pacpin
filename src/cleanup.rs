//! Invocation cleanup intent. Managers own resources and declare capabilities.
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Policy {
    pub temporary: bool,
    pub cache: bool,
}
static POLICY: OnceLock<Policy> = OnceLock::new();
pub(crate) fn initialize(policy: Policy) {
    let _ = POLICY.set(policy);
}
pub(crate) fn temporary() -> bool {
    POLICY.get().is_some_and(|p| p.temporary)
}
pub(crate) fn cache() -> bool {
    POLICY.get().is_some_and(|p| p.cache)
}

/// Consume only pacpin options, preserving option values and application argv.
/// Cache intent is normalized to the existing scoped --no-cache lifecycle.
pub(crate) fn parse(args: &mut Vec<String>) -> Result<Policy, String> {
    let supported = crate::commands::cli::supports_preparation(args);
    let mut policy = Policy::default();
    let mut output = Vec::new();
    let mut legacy_cache = false;
    let mut selected = false;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            output.extend_from_slice(&args[index..]);
            break;
        }
        let selection = if let Some(value) = arg.strip_prefix("--cleanup=") {
            Some(value)
        } else if arg == "--cleanup" {
            index += 1;
            Some(
                args.get(index)
                    .ok_or("--cleanup requires temporary, cache, or temporary,cache")?
                    .as_str(),
            )
        } else {
            None
        };
        if let Some(selection) = selection {
            selected = true;
            if !supported {
                return Err(
                    "--cleanup is supported for installs, upgrades, rebuilds, try and run".into(),
                );
            }
            for value in selection.split(',') {
                match value {
                    "temporary" => policy.temporary = true,
                    "cache" => policy.cache = true,
                    _ => {
                        return Err(format!(
                            "Unknown cleanup policy '{value}'; choose temporary or cache"
                        ))
                    }
                }
            }
        } else {
            if matches!(arg.as_str(), "--no-cache" | "--clean-cache") {
                policy.cache = true;
                legacy_cache = true;
            }
            output.push(arg.clone());
            if crate::commands::cli::takes_value(arg) || arg == "--bin" {
                index += 1;
                if let Some(value) = args.get(index) {
                    output.push(value.clone());
                }
            }
        }
        index += 1;
    }
    if selected && !crate::commands::cli::supports_preparation(&output) {
        return Err("--cleanup requires an installation, upgrade, rebuild, try or run operation".into());
    }
    if policy.cache && !legacy_cache {
        let index = output
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(output.len());
        output.insert(index, "--no-cache".into());
    }
    *args = output;
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).into()).collect()
    }
    #[test]
    fn composes_and_preserves_values_and_application_arguments() {
        let mut input = args(&[
            "run",
            "--cleanup",
            "temporary,cache",
            "app",
            "--",
            "--cleanup=cache",
        ]);
        assert_eq!(
            parse(&mut input).unwrap(),
            Policy {
                temporary: true,
                cache: true
            }
        );
        assert_eq!(
            input,
            args(&["run", "app", "--no-cache", "--", "--cleanup=cache"])
        );
        let mut input = args(&["-S", "--ignore", "--cleanup=temporary", "app"]);
        assert_eq!(parse(&mut input).unwrap(), Policy::default());
        assert_eq!(input[2], "--cleanup=temporary");
        let mut input = args(&["try", "--bin", "--cleanup=temporary", "app"]);
        assert_eq!(parse(&mut input).unwrap(), Policy::default());
    }
    #[test]
    fn rejects_unknown_empty_and_irrelevant_policies() {
        for input in [
            vec!["-S", "app", "--cleanup=all"],
            vec!["run", "app", "--cleanup="],
            vec!["-S", "--cleanup"],
            vec!["-Sy", "--cleanup=cache"],
            vec!["-Sy", "--cleanup", "temporary"],
            vec!["-S", "--downloadonly", "app", "--cleanup", "temporary"],
            vec!["-Syu", "--downloadonly", "--cleanup=temporary"],
            vec!["clean", "--cleanup=cache"],
            vec!["-Ss", "--cleanup=temporary"],
        ] {
            let mut input = args(&input);
            let result =
                parse(&mut input).and_then(|_| crate::runtime::parse(&mut input).map(|_| ()));
            assert!(result.is_err());
        }
    }
}
