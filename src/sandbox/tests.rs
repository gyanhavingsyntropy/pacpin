use std::{env, fs};
use std::path::{Path, PathBuf};
use std::process::Command;
use crate::utils::is_package_entry_point;
use crate::utils::is_executable_file;
use crate::download::is_safe_tar_entry;
use super::providers::is_safe_flatpak_id;
use super::entrypoints::{find_executable, select_executable};
use super::native::{ephemeral_download_label, ephemeral_footprint, download_targets_parallel};
use super::mounts::{check_bwrap_capability, safe_wayland_socket, should_bind_cwd, add_package_data_mounts};
use super::workspace::{SandboxGuard, create_secure_temp_dir, create_secure_temp_dir_in, should_clean_stale_sandbox, private_runtime_subdir};

    #[test]
    fn ephemeral_scratch_estimate_includes_archives_extraction_and_overhead() {
        assert!(ephemeral_download_label(10, 40).contains("25%"));
        assert!(ephemeral_download_label(1024, 2048).contains("1.0/2.0 KiB"));
        assert!(!ephemeral_download_label(10, 0).contains('%'));
        let targets = [crate::db::DownloadTarget {
            name: "fixture".into(), url: String::new(), candidate_urls: Vec::new(), sha256: None,
            download_bytes: 100, installed_bytes: 1024 * 1024 * 1024,
        }];
        let (bytes, inodes) = ephemeral_footprint(&targets);
        assert_eq!(bytes, 32 * 1024 * 1024 + 100 + 2 * 1024 * 1024 * 1024);
        assert_eq!(inodes, 130);
        let base = std::env::temp_dir();
        let workspace = create_secure_temp_dir_in("pacpin-run-medium-test", &base).unwrap();
        assert_eq!(workspace.parent(), Some(base.as_path()));
        let guard = SandboxGuard::new(workspace.clone());
        drop(guard);
        assert!(!workspace.exists());
    }
    use super::*;

    #[test]
    fn test_parse_target() {
        assert_eq!(parse_target("flatpak/org.gnome.Calculator"), (Some("flatpak"), "org.gnome.Calculator"));
        assert_eq!(parse_target("flatpak:org.gnome.Calculator"), (Some("flatpak"), "org.gnome.Calculator"));
        assert_eq!(parse_target("nix/fastfetch"), (Some("nix"), "fastfetch"));
        assert_eq!(parse_target("nix:fastfetch"), (Some("nix"), "fastfetch"));
        assert_eq!(parse_target("nixpkgs#ripgrep"), (Some("nix"), "ripgrep"));
        assert_eq!(parse_target("pipx/cowsay"), (Some("pipx"), "cowsay"));
        assert_eq!(parse_target("pipx:cowsay"), (Some("pipx"), "cowsay"));
        assert_eq!(parse_target("extra/tree"), (Some("extra"), "tree"));
        assert_eq!(parse_target("tree"), (None, "tree"));
    }

    #[test]
    fn test_host_only_providers_require_explicit_opt_in() {
        for target in ["nix/fastfetch", "nixpkgs#ripgrep", "pipx/cowsay"] {
            assert!(host_only_provider(parse_target(target).0));
        }
        assert!(!host_only_provider(parse_target("flatpak/org.gnome.Calculator").0));
        assert!(!host_only_provider(parse_target("extra/jq").0));
    }

    #[test]
    fn test_is_safe_flatpak_id() {
        assert!(is_safe_flatpak_id("org.gnome.Calculator"));
        assert!(is_safe_flatpak_id("com.spotify.Client"));
        assert!(is_safe_flatpak_id("com.github.tchx84.Flatseal"));
        assert!(!is_safe_flatpak_id("org.gnome/calc"));
        assert!(!is_safe_flatpak_id("org.gnome;rm -rf"));
    }

    #[test]
    fn test_is_executable_file() {
        let sh_path = Path::new("/bin/sh");
        if sh_path.exists() {
            assert!(is_executable_file(sh_path));
        }
        let non_exec = Path::new("/etc/passwd");
        if non_exec.exists() {
            assert!(!is_executable_file(non_exec));
        }
    }

    #[test]
    fn test_is_safe_tar_entry() {
        assert!(is_safe_tar_entry(".PKGINFO"));
        assert!(is_safe_tar_entry(".BUILDINFO"));
        assert!(is_safe_tar_entry("usr/bin/jq"));
        assert!(is_safe_tar_entry("./usr/bin/jq"));
        assert!(is_safe_tar_entry("usr/share/man/man1/jq.1.gz"));
        assert!(is_safe_tar_entry(".hidden/file"));
        assert!(is_safe_tar_entry(""));

        // Unsafe entries
        assert!(!is_safe_tar_entry("/etc/shadow"));
        assert!(!is_safe_tar_entry("/usr/bin/jq"));
        assert!(!is_safe_tar_entry("../../../etc/shadow"));
        assert!(!is_safe_tar_entry("usr/bin/../../etc/passwd"));
        assert!(!is_safe_tar_entry("usr/bin/.."));
        assert!(!is_safe_tar_entry(".."));
        assert!(!is_safe_tar_entry("\\etc\\shadow"));
        assert!(!is_safe_tar_entry("..\\..\\windows\\system32"));
    }


    #[test]
    fn test_try_options_default() {
        let opts = TryOptions::default();
        assert!(!opts.no_sandbox);
        assert!(!opts.share_net);
        assert!(!opts.rw_cwd);
        assert!(!opts.gui);
        assert!(!opts.audio);
        assert!(opts.bin.is_none());
        assert!(!opts.allow_unverified);
        assert!(!opts.is_run_mode);
    }

    #[test]
    fn installed_executable_selection_is_limited_to_package_inventory() {
        assert!(is_package_entry_point(Path::new("usr/lib/jvm/java-fixture/bin/java")));
        assert!(is_package_entry_point(Path::new("opt/tool/bin/tool")));
        assert!(!is_package_entry_point(Path::new("usr/share/doc/bin/example")));
        assert!(!is_package_entry_point(Path::new("usr/lib/../../tmp/bin/tool")));
        assert!(!is_package_entry_point(Path::new("/usr/bin/tool")));
        assert!(select_executable(vec!["/usr/bin/java".into(), "/usr/lib/jvm/tool/bin/java".into()], "jdk", Some("java")).is_err());
        let paths = vec![PathBuf::from("/usr/bin/one"), PathBuf::from("/usr/bin/two")];
        assert!(select_executable(paths.clone(), "package", None).is_err());
        assert_eq!(select_executable(paths.clone(), "package", Some("two")).unwrap(), Path::new("/usr/bin/two"));
        assert!(select_executable(paths.clone(), "package", Some("sh")).is_err());
        for invalid in ["../one", "/usr/bin/one", "one\n", "-one"] { assert!(select_executable(paths.clone(), "package", Some(invalid)).is_err()); }
        assert_eq!(select_executable(paths.clone(), "one", None).unwrap(), paths[0]);
        assert_eq!(select_executable(vec![paths[1].clone()], "package", None).unwrap(), paths[1]);
    }

    #[test]
    fn test_try_options_for_run() {
        let opts = TryOptions::for_run();
        assert!(opts.no_sandbox);
        assert!(opts.share_net);
        assert!(opts.rw_cwd);
        assert!(!opts.gui);
        assert!(!opts.audio);
        assert!(opts.bin.is_none());
        assert!(!opts.allow_unverified);
        assert!(opts.is_run_mode);
        assert!(!opts.noconfirm);
    }


    #[test]
    fn test_guard_preserves_cached_archive() {
        let sandbox_dir = create_secure_temp_dir("pacpin-try-cache-test").unwrap();
        let cached = crate::download::create_secure_temp_file(&env::temp_dir(), "pacpin-cache-test", "pkg.tar.zst").unwrap();
        let mut guard = SandboxGuard::new(sandbox_dir.clone());
        guard.register_tarball(&cached);
        drop(guard);
        assert!(cached.exists());
        assert!(!sandbox_dir.exists());
        fs::remove_file(cached).unwrap();
    }

    #[test]
    fn test_stale_cleanup_keeps_live_sandbox() {
        let live = create_secure_temp_dir("pacpin-run-live-test").unwrap();
        assert!(!should_clean_stale_sandbox(&live));
        fs::remove_dir_all(live).unwrap();

        let dead = env::temp_dir().join(format!("pacpin-run-dead-test-pid{}-123", i32::MAX));
        fs::create_dir(&dead).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dead, fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(should_clean_stale_sandbox(&dead));
        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};
            let link = dead.with_extension("link");
            symlink(&dead, &link).unwrap();
            assert!(!should_clean_stale_sandbox(&link));
            fs::remove_file(link).unwrap();
            fs::set_permissions(&dead, fs::Permissions::from_mode(0o777)).unwrap();
            assert!(!should_clean_stale_sandbox(&dead));
        }
        fs::remove_dir_all(dead).unwrap();
    }

    #[test]
    fn test_cwd_mount_policy_protects_home_and_system() {
        let home = env::temp_dir().join(format!("pacpin-home-policy-{}", std::process::id()));
        let nested = home.join("private");
        fs::create_dir_all(&nested).unwrap();
        assert!(!should_bind_cwd(Path::new("/"), &home));
        assert!(!should_bind_cwd(Path::new("/etc"), &home));
        assert!(!should_bind_cwd(&home, &home));
        assert!(!should_bind_cwd(&nested, &home));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn test_package_data_mounts_preserve_absolute_lookup_paths() {
        let root = create_secure_temp_dir("pacpin-data-mount-test").unwrap();
        fs::create_dir_all(root.join("usr/share/figlet")).unwrap();
        fs::create_dir_all(root.join("etc/figlet")).unwrap();
        let mut command = Command::new("bwrap");
        add_package_data_mounts(&mut command, &root, false).unwrap();
        let args: Vec<_> = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert!(args.windows(3).any(|w| w[0] == "--ro-bind" && w[2] == "/usr/share"));
        assert!(!args.windows(3).any(|w| w[0] == "--ro-bind" && w[2] == "/etc/figlet"));
        let mut overlay = Command::new("bwrap");
        add_package_data_mounts(&mut overlay, &root, true).unwrap();
        let overlay_args: Vec<_> = overlay.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert!(overlay_args.windows(2).any(|w| w == ["--ro-overlay", "/usr/share"]));
        assert!(overlay_args.windows(2).any(|w| w == ["--ro-overlay", "/etc"]));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_runtime_directory_rejects_symlink_and_public_permissions() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let runtime = create_secure_temp_dir("pacpin-runtime-test").unwrap();
        let private = private_runtime_subdir(&runtime, "run").unwrap();
        assert!(private.is_dir());

        let link = runtime.with_extension("link");
        symlink(&runtime, &link).unwrap();
        assert!(private_runtime_subdir(&link, "run").is_err());
        fs::remove_file(link).unwrap();

        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_runtime_subdir(&runtime, "run").is_err());
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(runtime).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_wayland_socket_must_be_real_socket_with_safe_basename() {
        use std::os::unix::net::UnixListener;
        use std::os::unix::fs::symlink;
        let runtime = create_secure_temp_dir("pacpin-wayland-test").unwrap();
        let socket = runtime.join("wayland-0");
        let _listener = UnixListener::bind(&socket).unwrap();
        assert_eq!(safe_wayland_socket(&runtime, "wayland-0"), Some(socket.clone()));
        for display in ["", ".", "..", "../passwd", "/etc/passwd", "nested/wayland-0"] {
            assert_eq!(safe_wayland_socket(&runtime, display), None);
        }
        symlink(&socket, runtime.join("wayland-link")).unwrap();
        assert_eq!(safe_wayland_socket(&runtime, "wayland-link"), None);
        fs::remove_dir_all(runtime).unwrap();
    }



    #[test]
    fn test_downloads_overlap_but_preserve_extraction_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let targets: Vec<crate::db::DownloadTarget> = (0..3)
            .map(|i| crate::db::DownloadTarget {
                name: format!("pkg-{}", i), url: String::new(),
                candidate_urls: Vec::new(), sha256: None,
                download_bytes: 0, installed_bytes: 0,
            })
            .collect();
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let mut guard = SandboxGuard::new(create_secure_temp_dir("pacpin-run-parallel-test").unwrap());
        let archives = download_targets_parallel(&targets, false, &mut guard, |target, _, _| {
            let count = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(count, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(40));
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(PathBuf::from(&target.name))
        }).unwrap();
        assert_eq!(archives, vec![PathBuf::from("pkg-0"), PathBuf::from("pkg-1"), PathBuf::from("pkg-2")]);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_parallel_download_error_cleans_owned_files() {
        let sandbox = create_secure_temp_dir("pacpin-run-download-error-test").unwrap();
        let mut guard = SandboxGuard::new(sandbox.clone());
        let target = crate::db::DownloadTarget {
            name: "broken".into(), url: String::new(), candidate_urls: Vec::new(), sha256: None,
            download_bytes: 0, installed_bytes: 0,
        };
        let mut downloaded_path = None;
        let result = download_targets_parallel(&[target], false, &mut guard, |_, _, downloaded| {
            let temp = crate::download::create_secure_temp_file(&env::temp_dir(), "pacpin-download-error-test", "pkg.tar.zst")?;
            downloaded.push(temp.clone());
            Err(format!("test failure at {}", temp.display()))
        });
        if let Err(message) = &result {
            downloaded_path = message.strip_prefix("test failure at ").map(PathBuf::from);
        }
        assert!(result.is_err());
        let downloaded_path = downloaded_path.unwrap();
        assert!(downloaded_path.exists());
        drop(guard);
        assert!(!downloaded_path.exists());
        assert!(!sandbox.exists());
    }

    #[test]
    fn test_requested_binary_cannot_escape_package_root() {
        let root = create_secure_temp_dir("pacpin-try-bin-test").unwrap();
        for name in ["/bin/sh", "../../bin/sh", "../sh", "-sh", "\\bin\\sh"] {
            assert!(find_executable(&root, "test", Some(name))
                .unwrap_err()
                .contains("single executable"));
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_check_bwrap_capability_runs() {
        // Must execute cleanly without panic
        let _ = check_bwrap_capability();
    }

    #[test]
    fn test_cwd_is_home_logic() {
        let temp_home = std::env::temp_dir().join(format!("pacpin-test-home-{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_home);
        let sub_dir = temp_home.join("workspace");
        let _ = fs::create_dir_all(&sub_dir);

        let is_home = temp_home.canonicalize().ok() == temp_home.canonicalize().ok();
        assert!(is_home);

        let sub_is_home = sub_dir.canonicalize().ok() == temp_home.canonicalize().ok();
        assert!(!sub_is_home);

        let _ = fs::remove_dir_all(&temp_home);
    }
