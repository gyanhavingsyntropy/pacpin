use super::*;

#[test]
fn setup_arguments_are_checked_before_runtime_flags_can_disappear() {
    for command in ["init", "setup"] {
        for suffix in [vec![], vec!["--reset"], vec!["-r"]] {
            let args: Vec<_> = std::iter::once(command)
                .chain(suffix)
                .map(str::to_string)
                .collect();
            assert!(matches!(parse_args(&args), Ok(Action::Setup { .. })));
        }
        for option in [
            "--pkexec",
            "--sudo",
            "--use-ram",
            "--keep-going",
            "--noconfirm",
            "--unknown",
        ] {
            assert!(parse_args(&[command.into(), option.into()]).is_err());
        }
    }
    assert!(matches!(
        parse_args(&["init".into(), "--help".into()]),
        Ok(Action::Help)
    ));
}

#[test]
fn reconfiguration_defaults_preserve_rules_options_manifest_and_overrides() {
    let mut original = Config::default();
    original.options.privilege_tool = "pkexec".into();
    original.options.helper = "yay".into();
    original.options.no_cache = true;
    original.features.shell_aliases = false;
    original.features.stability_delays = true;
    original.features.vendor_stickiness = true;
    original.features.integrations = true;
    original.integrations.nix = true;
    original.pins.insert("example".into(), "aur".into());
    original.delay.insert("linux".into(), 9);
    original
        .exclude
        .insert("extra".into(), vec!["example".into()]);
    original.repo_order.push("extra".into());
    original.packages.aur.push("example".into());
    original.manager_file = toml::from_str("[managers.custom]\nadapter='command'\nenabled=true\ncheck=['/bin/true']\nverify=['/bin/true','{id}']\nupgrade=['/bin/true','{id}']\n").unwrap();
    let answers = "\n".repeat(11);
    let mut output = Vec::new();
    let mut prompts = prompts::Prompts::new(answers.as_bytes(), &mut output);
    let updated = preferences::configure(&mut prompts, original.clone(), false).unwrap();
    assert_eq!(updated, original);
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("custom: custom adapter, enabled"));
    assert!(output.contains("sources.toml") && output.contains("hooks.toml"));
}

#[test]
fn manager_owned_settings_are_not_presented_as_editable_main_settings() {
    let mut original = Config::default();
    original.features.shell_aliases = false;
    original.features.integrations = true;
    original.options.privilege_tool = "pkexec".into();
    original.manager_file = toml::from_str("[managers.pacman]\nadapter='pacman'\nprivilege_tool='pkexec'\n[managers.aur]\nadapter='aur'\nhelper='paru'\n[managers.flatpak]\nadapter='flatpak'\nenabled=true\n").unwrap();
    let answers = "\n".repeat(8);
    let mut output = Vec::new();
    let mut prompts = prompts::Prompts::new(answers.as_bytes(), &mut output);
    let updated = preferences::configure(&mut prompts, original.clone(), false).unwrap();
    assert_eq!(updated, original);
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("flatpak: enabled (owned by managers.toml)"));
    assert!(!output.contains("Include flatpak updates?"));
    assert!(!output.contains("Privilege tool: sudo, pkexec or doas"));
}

#[test]
fn questionnaire_requires_final_explicit_acceptance_and_cancels_at_any_prompt() {
    let path = std::path::Path::new("/tmp/example/pacpin/config.toml");
    for answers in [format!("{}y\n", "\n".repeat(11)), "\n".repeat(12)] {
        let saved = answers.ends_with("y\n");
        let mut prompts = prompts::Prompts::new(answers.as_bytes(), Vec::new());
        let result =
            questionnaire(&mut prompts, Config::default(), true, path, false, false).unwrap();
        assert_eq!(result.is_some(), saved);
        if let Some(config) = result {
            assert!(!config.features.shell_aliases);
        }
    }
    for count in 0..12 {
        for suffix in ["", "q\n"] {
            let answers = format!("{}{suffix}", "\n".repeat(count));
            let mut prompts = prompts::Prompts::new(answers.as_bytes(), Vec::new());
            assert!(matches!(
                questionnaire(&mut prompts, Config::default(), true, path, false, false),
                Err(prompts::Error::Cancelled)
            ));
        }
    }
}

#[test]
fn review_reports_effective_external_selection_and_no_invocation_flags_as_defaults() {
    let config = Config {
        manager_file: toml::from_str("[managers.flatpak]\nadapter='flatpak'\nenabled=true\n")
            .unwrap(),
        ..Config::default()
    };
    let mut output = Vec::new();
    let mut prompts = prompts::Prompts::new(io::empty(), &mut output);
    review::show(
        &mut prompts,
        &config,
        std::path::Path::new("config.toml"),
        true,
    )
    .unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("flatpak updates: enabled"));
    assert!(output.contains("nix updates: disabled"));
    assert!(output.contains("backed up"));
    assert!(!output.contains("--use-ram: enabled"));
}

#[test]
fn selecting_a_legacy_manager_enables_checks_without_enabling_others() {
    for (index, name) in [(8, "flatpak"), (9, "nix"), (10, "pipx")] {
        let mut answers = [""; 11];
        answers[index] = "y";
        let answers = format!("{}\n", answers.join("\n"));
        let mut prompts = prompts::Prompts::new(answers.as_bytes(), Vec::new());
        let updated = preferences::configure(&mut prompts, Config::default(), true).unwrap();
        assert!(updated.features.integrations);
        assert_eq!(updated.integrations.flatpak, name == "flatpak");
        assert_eq!(updated.integrations.nix, name == "nix");
        assert_eq!(updated.integrations.pipx, name == "pipx");
    }
}
