use std::collections::BTreeMap;
use crate::{find_matching_pin, parse_install_flags};

    #[test]
    fn clean_flags_preserve_full_cache_wipe() {
        let args = |parts: &[&str]| parts.iter().map(|part| (*part).to_string()).collect::<Vec<_>>();
        assert_eq!(super::parse_clean_args(&args(&["-Sc"])), (false, vec![]));
        assert_eq!(super::parse_clean_args(&args(&["-Scc", "--noconfirm"])), (true, vec!["--noconfirm".to_string()]));
        assert_eq!(super::parse_clean_args(&args(&["-S", "-c", "-c"])), (true, vec![]));
        assert_eq!(super::parse_clean_args(&args(&["clean-cache"])), (false, vec![]));
    }
    use super::*;

    #[test]
    fn test_is_command_available() {
        assert!(is_command_available("sh") || is_command_available("bash"));
        assert!(!is_command_available("this_binary_does_not_exist_xyz123"));
    }

    #[test]
    fn test_is_command_available_permissions() {
        let temp_dir = std::env::temp_dir();
        let non_exec = temp_dir.join(format!("pacpin_non_exec_test_{}", std::process::id()));
        std::fs::write(&non_exec, b"#!/bin/sh\necho hi\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&non_exec).unwrap().permissions();
            perms.set_mode(0o644); // No executable bit
            std::fs::set_permissions(&non_exec, perms).unwrap();
        }
        assert!(!is_command_available(&non_exec.to_string_lossy()));
        let _ = std::fs::remove_file(&non_exec);
    }

    #[test]
    fn test_find_matching_pin_exact_and_glob() {
        let mut pins = BTreeMap::new();
        pins.insert("mesa".to_string(), "core".to_string());
        pins.insert("linux-*".to_string(), "extra".to_string());
        pins.insert("linux-firmware*".to_string(), "cachyos".to_string());

        assert_eq!(
            find_matching_pin("mesa", &pins),
            Some(("mesa".to_string(), "core".to_string(), true))
        );
        // Specificity tie-break: "linux-firmware*" must beat "linux-*" despite ASCII key order
        assert_eq!(
            find_matching_pin("linux-firmware-intel", &pins),
            Some(("linux-firmware*".to_string(), "cachyos".to_string(), false))
        );
        assert_eq!(
            find_matching_pin("linux-zen", &pins),
            Some(("linux-*".to_string(), "extra".to_string(), false))
        );
        assert_eq!(find_matching_pin("git", &pins), None);
    }

    #[test]
    fn test_parse_pin_args_legacy_syntax() {
        let known_repos = vec!["core".to_string(), "extra".to_string(), "aur".to_string()];
        let args = vec!["linux-firmware*".to_string(), "core".to_string()];
        let (repo, patterns) = parse_pin_args(&args, &known_repos).unwrap();
        assert_eq!(repo, "core");
        assert_eq!(patterns, vec!["linux-firmware*"]);
    }

    #[test]
    fn test_parse_pin_args_repo_first() {
        let known_repos = vec!["core".to_string(), "extra".to_string(), "aur".to_string()];
        let args = vec!["core".to_string(), "linux-firmware*".to_string()];
        let (repo, patterns) = parse_pin_args(&args, &known_repos).unwrap();
        assert_eq!(repo, "core");
        assert_eq!(patterns, vec!["linux-firmware*"]);
    }

    #[test]
    fn test_parse_pin_args_batch() {
        let known_repos = vec!["core".to_string(), "extra".to_string(), "aur".to_string()];
        let args = vec![
            "core".to_string(),
            "pkg1".to_string(),
            "pkg2".to_string(),
            "pkg3".to_string(),
        ];
        let (repo, patterns) = parse_pin_args(&args, &known_repos).unwrap();
        assert_eq!(repo, "core");
        assert_eq!(patterns, vec!["pkg1", "pkg2", "pkg3"]);
    }

    #[test]
    fn test_parse_pin_args_file() {
        let known_repos = vec!["core".to_string(), "extra".to_string(), "aur".to_string()];
        let tmp_file = std::env::temp_dir().join("pacpin_test_pkgs.txt");
        std::fs::write(&tmp_file, "pkgA\n# comment\npkgB\n\npkgC\n").unwrap();

        let args = vec![
            "core".to_string(),
            "-f".to_string(),
            tmp_file.to_str().unwrap().to_string(),
        ];
        let (repo, patterns) = parse_pin_args(&args, &known_repos).unwrap();
        assert_eq!(repo, "core");
        assert_eq!(patterns, vec!["pkgA", "pkgB", "pkgC"]);

        let _ = std::fs::remove_file(&tmp_file);
    }

    #[test]
    fn test_pure_orphan_filtering() {
        use crate::db::OrphanPackage;
        let orphans = [
            OrphanPackage {
                name: "pure-orphan".to_string(),
                version: "1.0".to_string(),
                desc: "test".to_string(),
                isize: 100,
                optional_for: Vec::new(),
                dropped_by: Vec::new(),
                is_projected: false,
            },
            OrphanPackage {
                name: "opt-plugin".to_string(),
                version: "1.0".to_string(),
                desc: "test plugin".to_string(),
                isize: 200,
                optional_for: vec!["ffmpeg".to_string()],
                dropped_by: Vec::new(),
                is_projected: false,
            },
        ];

        let pure: Vec<String> = orphans
            .iter()
            .filter(|o| o.optional_for.is_empty())
            .map(|o| o.name.clone())
            .collect();

        assert_eq!(pure, vec!["pure-orphan"]);
    }

    #[test]
    fn test_is_upgrade_invocation() {
        let to_vec = |slice: &[&str]| slice.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Valid upgrade forms
        assert!(is_upgrade_invocation(&to_vec(&["upgrade"])));
        assert!(is_upgrade_invocation(&to_vec(&["up"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Syu"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Suy"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Su"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Sy"])));
        assert!(is_refresh_only_invocation(&to_vec(&["-Sy", "--noconfirm"])));
        assert!(is_refresh_only_invocation(&to_vec(&["-Syy"])));
        assert!(is_refresh_only_invocation(&to_vec(&["-S", "--refresh"])));
        assert!(!is_refresh_only_invocation(&to_vec(&["-Syu"])));
        assert!(!is_refresh_only_invocation(&to_vec(&["-Sy", "--sysupgrade"])));
        assert!(!is_refresh_only_invocation(&to_vec(&["-Sy", "ollama"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Syuu"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-y", "-u"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-u", "-y"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-u"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-y"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-u", "-n"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Syu", "--noconfirm"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "--sysupgrade"])));

        // Non-upgrade forms (should not match upgrade)
        assert!(!is_upgrade_invocation(&to_vec(&["-S"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-S", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Sy", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Ss", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-S", "-s", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Si", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-S", "-i", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Sc"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-S", "-c"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Sw", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Sp", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["check"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Qu"])));

        // Upgrade with option-consuming flags
        assert!(is_upgrade_invocation(&to_vec(&[
            "-Syu", "--ignore", "linux"
        ])));
        assert!(is_upgrade_invocation(&to_vec(&["-Syu", "--ignore=linux"])));
        assert!(is_upgrade_invocation(&to_vec(&[
            "-S",
            "-u",
            "--overwrite",
            "/usr/*"
        ])));
        assert!(is_upgrade_invocation(&to_vec(&[
            "-Syu",
            "--assume-installed",
            "foo:1.0"
        ])));

        // Targeted upgrades: explicit targets join the upgrade engine.
        assert!(is_upgrade_invocation(&to_vec(&["-Syu", "ripgrep"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Su", "ripgrep"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-u", "ripgrep"])));
        assert!(is_upgrade_invocation(&to_vec(&["upgrade", "ripgrep"])));
        assert!(is_upgrade_invocation(&to_vec(&["up", "extra/ripgrep"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Syu", "extra/ripgrep"])));
        assert!(!is_refresh_only_invocation(&to_vec(&["-Syu", "ripgrep"])));
    }

    #[test]
    fn test_parse_pacman_cli_args() {
        let to_vec = |slice: &[&str]| slice.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Separates option arguments from targets
        let (targets, flags) =
            parse_pacman_cli_args(&to_vec(&["--ignore", "linux", "--noconfirm", "neovim"]));
        assert_eq!(targets, vec!["neovim"]);
        assert_eq!(flags, vec!["--ignore", "linux", "--noconfirm"]);

        // Handles --ignore=linux equals syntax
        let (targets, flags) = parse_pacman_cli_args(&to_vec(&["--ignore=linux", "ripgrep"]));
        assert_eq!(targets, vec!["ripgrep"]);
        assert_eq!(flags, vec!["--ignore=linux"]);

        // Handles double dash -- to treat everything after as targets
        let (targets, flags) = parse_pacman_cli_args(&to_vec(&["-S", "--", "--ignore", "foo"]));
        assert_eq!(flags, vec!["-S"]);
        assert_eq!(targets, vec!["--ignore", "foo"]);

        // Handles config, dbpath, root, overwrite flags consuming arguments
        let (targets, flags) = parse_pacman_cli_args(&to_vec(&[
            "--config",
            "/etc/pacman.conf",
            "-b",
            "/var/lib/pacman",
            "--overwrite",
            "/usr/share/*",
            "git",
        ]));
        assert_eq!(targets, vec!["git"]);
        assert_eq!(
            flags,
            vec![
                "--config",
                "/etc/pacman.conf",
                "-b",
                "/var/lib/pacman",
                "--overwrite",
                "/usr/share/*"
            ]
        );

        let args = to_vec(&[
            "--ignore", "linux", "--overwrite", "*", "--assume-installed", "foo=1.0",
            "--noconfirm", "bar",
        ]);
        let (targets, flags) = parse_pacman_cli_args(&args);
        assert_eq!(targets, vec!["bar"]);
        assert_eq!(flags, args[..7]);
        // A genuine bare target with -u joins the upgrade engine now.
        assert!(is_upgrade_invocation(&to_vec(&[
            "-Syu", "--ignore", "linux", "--overwrite", "*",
            "--assume-installed", "foo=1.0", "bar",
        ])));
    }

    #[test]
    fn test_pacman_files_needs_sudo() {
        let to_vec = |slice: &[&str]| slice.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Read-only queries should NOT require sudo
        assert!(!pacman_files_needs_sudo(&to_vec(&["-F", "python"])));
        assert!(!pacman_files_needs_sudo(&to_vec(&["-Fl", "ripgrep"])));
        assert!(!pacman_files_needs_sudo(&to_vec(&["-Fs", "libssl.so"])));
        assert!(!pacman_files_needs_sudo(&to_vec(&[
            "-F",
            "-b",
            "/var/lib/pacman",
            "python"
        ])));

        // Sync / refresh actions DO require sudo
        assert!(pacman_files_needs_sudo(&to_vec(&["-Fy"])));
        assert!(pacman_files_needs_sudo(&to_vec(&["-Fyy"])));
        assert!(pacman_files_needs_sudo(&to_vec(&["-F", "-y"])));
        assert!(pacman_files_needs_sudo(&to_vec(&["-F", "--refresh"])));
        assert!(pacman_files_needs_sudo(&to_vec(&["-Fy", "python"])));
    }

    #[test]
    fn test_find_matching_pin_exact_and_wildcard() {
        let mut pins = std::collections::BTreeMap::new();
        pins.insert("linux-cachyos*".to_string(), "cachyos".to_string());
        pins.insert("neovim".to_string(), "extra".to_string());

        let res_exact = find_matching_pin("neovim", &pins);
        assert_eq!(res_exact, Some(("neovim".to_string(), "extra".to_string(), true)));

        let res_wildcard = find_matching_pin("linux-cachyos-headers", &pins);
        assert_eq!(res_wildcard, Some(("linux-cachyos*".to_string(), "cachyos".to_string(), false)));

        let res_none = find_matching_pin("ripgrep", &pins);
        assert_eq!(res_none, None);
    }

    #[test]
    fn test_parse_install_flags() {
        let to_vec = |slice: &[&str]| slice.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Standalone -y (refresh without sysupgrade)
        let opts = parse_install_flags(&to_vec(&["-y"]));
        assert!(opts.refresh);
        assert!(!opts.sysupgrade);
        assert!(!opts.dry_run);
        assert!(opts.forwarded_flags.is_empty());

        // Standalone -u (sysupgrade without refresh)
        let opts = parse_install_flags(&to_vec(&["-u"]));
        assert!(!opts.refresh);
        assert!(opts.sysupgrade);
        assert!(opts.forwarded_flags.is_empty());

        // Combined -yu and -uy (refresh and sysupgrade)
        let opts = parse_install_flags(&to_vec(&["-yu"]));
        assert!(opts.refresh);
        assert!(opts.sysupgrade);
        assert!(opts.forwarded_flags.is_empty());

        let opts = parse_install_flags(&to_vec(&["-uy"]));
        assert!(opts.refresh);
        assert!(opts.sysupgrade);
        assert!(opts.forwarded_flags.is_empty());

        // Long options
        let opts = parse_install_flags(&to_vec(&["--refresh", "--sysupgrade", "--noconfirm"]));
        assert!(opts.refresh);
        assert!(opts.sysupgrade);
        assert!(opts.noconfirm);
        assert!(opts.forwarded_flags.is_empty());

        // Forwarding unrecognized flags while consuming internal ones
        let opts = parse_install_flags(&to_vec(&["-yu", "--overwrite", "*", "--cachedir=/tmp", "--no-cache"]));
        assert!(opts.refresh);
        assert!(opts.sysupgrade);
        assert!(opts.no_cache);
        assert_eq!(opts.forwarded_flags, vec!["--overwrite", "*", "--cachedir=/tmp"]);
    }
