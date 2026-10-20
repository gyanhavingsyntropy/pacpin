
    #[test]
    fn footer_cleanup_keeps_the_previous_region_visible_across_resize() {
        assert_eq!(super::footer_cleanup_row(30, 18, 3), 16);
        assert_eq!(super::footer_cleanup_row(18, 30, 3), 16);
        assert_eq!(super::footer_cleanup_row(30, 30, 3), 28);
        assert_eq!(super::footer_cleanup_row(18, 18, 2), 17);
    }

    use super::*;
    #[test]
    fn build_logs_cannot_impersonate_authentication_verification_or_installation() {
        for manager in [Manager::Aur, Manager::Sources] {
            for line in ["checking for a working compiler... yes", "checking password support... no", "installing headers in pkgdir", "optional feature test failed", "gcc -Dfilename=authentication.c -c source.c"] {
                assert_eq!(phase_text(manager, line, "Building packages"), "Building packages", "{line}");
            }
            assert_eq!(phase_text(manager, "==> Starting package()...", "Building packages"), "Packaging source");
            assert_eq!(phase_text(manager, "installing staged binaries", "Packaging source"), "Packaging source");
            assert_eq!(phase_text(manager, "==> Finished making: package 1-1", "Packaging source"), "Build complete; awaiting installation");
            assert_eq!(phase_text(manager, "(1/1) upgrading package [####] 100%", "Packaging source"), if manager == Manager::Aur { "Installing packages (100%, 1/1)" } else { "Installing packages" });
            assert_eq!(phase_text(manager, "==> ERROR: A failure occurred in build().", "Building packages"), "Operation failed");
        }
    }
    #[test]
    fn makepkg_charset_resets_do_not_hide_preparation_windows() {
        let mut observer = Observer::default();
        let mut lines = Vec::new();
        for chunk in [b"\x1b[1m==>\x1b(".as_slice(), b"B\x1b[m Starting build()...\x1b(B\x1b[m\r\n"] {
            observer.feed_observed(chunk, &mut || {}, &mut |line| lines.push(line.to_owned()));
        }
        assert_eq!(lines, ["==> Starting build()..."]);
        assert_eq!(transfers::window(Manager::Aur, &lines[0]), Some(transfers::Window::Computing));
        assert_eq!(phase_text(Manager::Aur, &lines[0], "Downloading"), "Building packages");
    }
    #[test]
    fn aur_transfer_scope_survives_narrow_footer_and_resets_at_build() {
        let phase = phase_text(Manager::Aur, "12 100M 25 25M 0 0 1M 0 0:00:40 0:00:10 0:00:30 1M", "Preparing");
        assert_eq!(phase, "Downloading (file) (25%)");
        for width in [12, 18, 25, 35] {
            let line = native_footer(&phase, width);
            assert!(line.contains("25%"), "{width}: {line}");
            assert!(crate::ui::str_width(&line) <= width);
        }
        assert_eq!(phase_text(Manager::Aur, "==> Starting build()...", &phase), "Building packages");
    }
    #[test]
    fn native_totals_weight_all_archives_and_ignore_individual_bars() {
        native_downloads(vec![("small-1-x86_64".into(), 10), ("large-1-x86_64".into(), 90)], || {
            let mut downloads = NativeDownloads::current();
            downloads.observe("small-1-x86_64 1 KiB [###] 100%");
            assert_eq!(downloads.phase(Manager::Native, "large-1-x86_64 1 KiB [###] 50%", "Downloading packages"), "Downloading packages (55%)");
            assert_eq!(downloads.phase(Manager::Native, "large-1... [###] 50%", "Downloading packages"), "Downloading packages (55%)");
            assert_eq!(downloads.phase(Manager::Native, "large-1-x86_64-other [###] 100%", "Downloading packages"), "Downloading packages");
            assert_eq!(downloads.phase(Manager::Native, "Total (2/2) 1 KiB [###] 63%", "Downloading packages"), "Downloading packages (63%)");
            assert_eq!(downloads.phase(Manager::Native, "(1/2) upgrading small [###] 100%", "Downloading packages"), "Installing packages (50%, 1/2)");
        });
        assert!(NativeDownloads::current().packages.is_empty());
        assert_eq!(phase_text(Manager::Native, "(1/2) installing small 0%", "Starting"), "Installing packages (0%, 1/2)");
        assert_eq!(phase_text(Manager::Native, "(0/0) installing small 100%", "Starting"), "Installing packages");
    }

    #[test]
    fn native_totals_handle_parallel_lines_and_large_sizes() {
        let mut downloads = NativeDownloads { packages: vec![("one".into(), u64::MAX, 0), ("two".into(), u64::MAX, 0)], ..Default::default() };
        let mut observer = Observer::default();
        observer.feed_observed(b"one 100%\r\x1b[1Atwo 50%\r", &mut || {}, &mut |line| downloads.observe(line));
        assert_eq!(downloads.phase(Manager::Native, "two 50%", "Downloading packages"), "Downloading packages (75%)");
    }
    #[test]
    fn ambiguous_truncated_names_do_not_invent_download_totals() {
        let mut downloads = NativeDownloads { packages: vec![("same-one".into(), 10, 0), ("same-two".into(), 90, 0)], ..Default::default() };
        assert_eq!(downloads.phase(Manager::Native, "same... 100%", "Downloading packages"), "Downloading packages");
        assert_eq!(downloads.phase(Manager::Native, "Total (2/2) 60%", "Downloading packages"), "Downloading packages (60%)");
        assert_eq!(downloads.phase(Manager::Native, "same... 100%", "Downloading packages"), "Downloading packages (60%)");
        for width in [18, 24, 32] {
            let text = native_footer("Installing packages (59%, 29/49)", width);
            assert!(text.contains("59%"));
            assert!(crate::ui::str_width(&text) <= width);
        }
    }
    #[test]
    fn short_footer_keeps_active_steps_and_recent_failures() {
        let history = vec!["Pacman: ✔ Upgrade complete".into(), "Sources: ✔ Upgrade complete".into(), "AUR: ✗ Operation failed".into()];
        let active = vec!["Review: Choosing orphan cleanup".into(), "Pacman: Downloading packages".into()];
        let lines = fit_rows(history, active, 3);
        assert_eq!(lines, ["AUR: ✗ Operation failed", "Review: Choosing orphan cleanup", "Pacman: Downloading packages"]);
        assert_eq!(Manager::Refresh.label(), "Repositories");
        assert_eq!(Manager::Restart.label(), "Restart");
        assert_eq!(phase_text(Manager::Record, "arbitrary output", "Recording dependency reasons"), "Recording dependency reasons");
    }

    #[test]
    fn many_pending_managers_do_not_consume_the_main_output_area() {
        let active = vec!["Pacman: Downloading packages (50%)".into(), "AUR: Pending".into(), "Flatpak: Pending".into(), "Nix: Pending".into()];
        assert_eq!(fit_rows(Vec::new(), active, 3), ["Pacman: Downloading packages (50%)", "AUR: Pending", "… 2 more stages"]);
    }

    #[test]
    fn native_interruption_marker_is_exact_and_survives_fragmented_output() {
        let mut observer = Observer::default();
        observer.feed(b"hook says: Interrupt signal received\n", &mut || {});
        assert!(!observer.interrupted);
        observer.feed(b"\x1b[31mInterrupt signal ", &mut || {});
        observer.feed(b"received\x1b[0m\r\n", &mut || {});
        assert!(observer.interrupted);
    }
    #[test]
    fn housekeeping_phases_and_completion_describe_actual_operations() {
        assert_eq!(phase_text(Manager::Orphans, "checking dependencies...", "Starting"), "Checking dependencies");
        assert_eq!(phase_text(Manager::Orphans, "(2/6) removing ninja [###] 100%", "Starting"), "Removing packages (33%, 2/6)");
        assert_eq!(phase_text(Manager::Orphans, "[sudo] password for user:", "Starting"), "Awaiting authorization");
        assert_eq!(phase_text(Manager::Cache, "Do you want to remove ALL files from cache? [y/N]", "Starting"), "Awaiting confirmation");
        assert_eq!(phase_text(Manager::Cache, "removing all files from cache...", "Starting"), "Cleaning package cache");
        assert_eq!(phase_text(Manager::Cache, "error: could not remove directory", "Starting"), "Operation failed");
        assert_eq!(Manager::Orphans.completion(), "Removal complete");
        assert_eq!(Manager::Cache.completion(), "Cleanup complete");
        assert_eq!(scoped_phase(Manager::Cache, "Cleaning native package cache", "Awaiting confirmation"), "Native: Awaiting confirmation");
        assert_eq!(scoped_phase(Manager::Cache, "Cleaning AUR build cache", "Cleaning package cache"), "AUR: Cleaning package cache");
    }

    #[test]
    fn prepared_cache_input_survives_shared_execution_and_real_failure_status() {
        let _guard = crate::execution::CHILD_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use std::io::Write;
        let (input, mut output) = UnixStream::pair().unwrap();
        output.write_all(b"y\nn\n").unwrap();
        output.shutdown(std::net::Shutdown::Write).unwrap();
        let status = run_with_stdin(Command::new("/bin/sh").args(["-c", "read first; read second; test \"$first:$second\" = y:n || exit 99; exit 7"]), Manager::Cache, "Cleaning package cache", Stdio::from(std::os::fd::OwnedFd::from(input))).unwrap();
        assert_eq!(status.code(), Some(7));
    }
    #[test]
    fn observed_lines_preserve_split_colored_failure_markers() {
        let mut observer = Observer::default();
        let mut lines = Vec::new();
        for fragment in [b"\x1b[31m==> ER".as_slice(), b"ROR:\x1b[0m A failure occurred in build().\r\n:: Processing package changes...\n"] {
            observer.feed_observed(fragment, &mut || {}, &mut |line| lines.push(line.to_owned()));
        }
        assert_eq!(lines, ["==> ERROR: A failure occurred in build().", ":: Processing package changes..."]);
    }
    #[test]
    fn footer_describes_phases_without_echoing_prompts_or_hook_logs() {
        assert_eq!(phase_text(Manager::Native, "(1/2) checking package integrity [###] 73%", "Installing packages"), "Verifying packages (73%, 1/2)");
        assert_eq!(phase_text(Manager::Native, "(1/2) loading package files [###] 73%", "Verifying packages"), "Verifying packages (73%, 1/2)");
        let mut observer = Observer::default();
        let mut downloads = NativeDownloads::default();
        let mut phase = "Installing packages".to_string();
        observer.feed_observed(b":: Running post-transaction hooks...\n(1/1) pacpin UI test hook\nPACPIN_NATIVE_HOOK_1\n", &mut || {}, &mut |line| { phase = downloads.phase(Manager::Native, line, &phase); });
        assert_eq!(phase, "Running post-install hooks");
        assert_eq!(phase_text(Manager::Native, "[sudo] password for user:", "Starting"), "Awaiting authorization");
        assert_eq!(phase_text(Manager::Native, "Proceed with installation? [Y/n]", "Starting"), "Awaiting confirmation");
        assert_eq!(phase_text(Manager::Native, "PACPIN_NATIVE_HISTORY_01", "Running hooks"), "Running hooks");
        assert_eq!(phase_text(Manager::Sources, ":: Packaging verified source", "Preparing"), "Packaging source");
        assert_eq!(phase_text(Manager::Native, "(29/49) upgrading webkitgtk-6.0 [####] 100%", "Preparing"), "Installing packages (59%, 29/49)");
        assert_eq!(phase_text(Manager::Native, "package-very-long-name [####] 80%", "Downloading packages (75%)"), "Downloading packages");
        assert_eq!(phase_text(Manager::Native, "( 7/10) Install DKMS modules", "Preparing"), "Building kernel modules (70%, 7/10)");
    }
    #[test]
    fn download_progress_handles_unknown_zero_and_overflowing_totals() {
        assert!(download_text("app", 5_242_880, Some(10_485_760)).contains("50%"));
        assert!(!download_text("app", 1, Some(0)).contains('%'));
        assert!(download_text("app", 20, Some(10)).contains("100%"));
    }

    #[test]
    fn long_source_filenames_keep_counts_and_percent_visible() {
        let text = footer_download(
            "codex-package-x86_64-unknown-linux-musl.tar.gz",
            125 * 1_048_576,
            Some(154 * 1_048_576),
            70,
        );
        assert!(text.contains("125.0/154.0 MiB (81%)"));
        assert!(crate::ui::str_width(&text) <= 70);
        for width in [20, 22, 30] {
            let text = footer_download("codex-package-x86_64-unknown-linux-musl.tar.gz", 125 * 1_048_576, Some(154 * 1_048_576), width);
            assert!(text.contains("125/154MiB 81%"));
            assert!(crate::ui::str_width(&text) <= width);
        }
    }

    #[test]
    fn native_footer_keeps_download_metrics_visible() {
        let text = native_footer("long-package-name-with-version-and-architecture  125.0 MiB  10 MiB/s  00:12 [########-----] 80%", 67);
        assert!(text.contains("125.0 MiB 10 MiB/s 00:12 80%"));
        assert!(crate::ui::str_width(&text) <= 67);
        assert_eq!(
            native_footer("(7/10) Install DKMS modules", 67),
            "(7/10) Install DKMS modules"
        );
    }
    #[test]
    fn prefetch_starts_once_after_downloads_and_handles_split_ansi() {
        let mut observer = Observer::default();
        let mut starts = 0;
        observer.feed(b":: Retrieving packages...\rapp 20%\x1b[", &mut || {
            starts += 1
        });
        assert_eq!(starts, 0);
        observer.feed(b"32m\r(1/2) checking keys in key", &mut || starts += 1);
        assert_eq!(starts, 0);
        observer.feed(b"ring\x1b[0m\nchecking package integrity\n", &mut || {
            starts += 1
        });
        assert_eq!(starts, 1);
    }
    #[test]
    fn failed_download_does_not_start_prefetch() {
        let mut observer = Observer::default();
        observer.feed(
            b"failed retrieving file\nerror: failed to commit transaction\n",
            &mut || panic!("prefetch started"),
        );
        assert!(!observer.ready);
    }
    #[test]
    fn terminal_text_strips_control_sequences() {
        assert_eq!(clean_text("\x1b[32mverified\x1b[0m"), "verified");
        assert_eq!(clean_text("\x1b]0;title\x07hello"), "hello");
    }
    #[test]
    fn relay_preserves_exit_status_and_detects_phase_without_host_changes() {
        let _serial = crate::execution::CHILD_TEST_LOCK.lock().unwrap();
        let mut starts = 0;
        let status = run(
            Command::new("sh").args([
                "-c",
                "printf 'checking keys in keyring\\n'; printf 'stderr retained\\n' >&2; exit 7",
            ]),
            Manager::Native,
            "Test",
            || starts += 1,
        )
        .unwrap();
        assert_eq!(status.code(), Some(7));
        assert_eq!(starts, 1);
    }

    #[test]
    fn background_wait_preserves_failure_and_clears_download_state() {
        let _serial = crate::execution::CHILD_TEST_LOCK.lock().unwrap();
        let child = start_background(Command::new("sh").args([
            "-c", "printf 'Downloading fixture 25%%\\r'; sleep 0.2; exit 7",
        ])).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(7));
        assert!(NATIVE_BACKGROUND.lock().unwrap().is_none());
        assert!(TERMINAL.lock().unwrap().is_none());
    }

    #[test]
    fn background_download_has_own_group_and_preserves_invoking_session() {
        let _serial = crate::execution::CHILD_TEST_LOCK.lock().unwrap();
        let child = start_background(Command::new("sh").args(["-c", "sleep 0.2"])).unwrap();
        let pid = child.child.0.id() as libc::pid_t;
        // A privilege helper may open /dev/tty despite redirected stdin.
        // It must see a background group, but retain the session/tty ticket.
        assert_eq!(unsafe { libc::getsid(pid) }, unsafe { libc::getsid(0) });
        assert_eq!(unsafe { libc::getpgid(pid) }, pid);
        assert_ne!(unsafe { libc::getpgid(pid) }, unsafe { libc::getpgrp() });
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn pty_preserves_prompt_output_footer_and_terminal_settings() {
        const CHILD: &str = "PACPIN_PROGRESS_PTY_TEST";
        if std::env::var_os(CHILD).is_some() {
            let mut before: libc::termios = unsafe { std::mem::zeroed() };
            assert_eq!(unsafe { libc::tcgetattr(0, &mut before) }, 0);
            let session = unsafe { libc::getsid(0) }.to_string();
            let tty = std::fs::read_link("/proc/self/fd/0").unwrap();
            begin_transaction();
            completed(Manager::Native);
            failed(Manager::Aur);
            let view = session_view();
            for _ in 0..5 { println!("PHASE_ANCHOR_PADDING"); }
            println!("PHASE_PADDING_START");
            for (manager, label) in [
                (Manager::Checks, "Checking update candidates"),
                (Manager::Review, "Awaiting plan approval"),
                (Manager::Record, "Saving upgrade history"),
                (Manager::Restart, "Checking restart requirements"),
            ] {
                operation(manager, label, || {
                    operation(Manager::Checks, "Nested check", || {});
                    std::thread::sleep(std::time::Duration::from_millis(110));
                });
                assert!(TERMINAL.lock().unwrap().is_none());
            }
            println!("PHASE_PADDING_END");
            drop(view);
            assert!(VIEW.lock().unwrap().is_none());
            context(Manager::Hooks, "Rebuilding triggered packages", || {
                operation(Manager::Aur, "Building triggered packages", || {
                    std::thread::sleep(std::time::Duration::from_millis(110));
                });
            });
            assert!(CONTEXT.lock().unwrap().is_none());
            let status = run(Command::new("sh").args(["-c",
                "stty -echo; printf 'AUTH_WAIT_PROMPT: ' >/dev/tty; read -r answer; test \"$answer\" = fixture-secret || exit 8; printf '\\nresolving dependencies...\\n'; sleep .3; stty echo; printf 'AUTH_WAIT_COMPLETE\\n'",
            ]), Manager::Refresh, "Awaiting authorization", || {}).unwrap();
            assert!(status.success());
            begin_transaction();
            queued_sources();
            let status = run(Command::new("sh").args(["-c",
                "test -t 0 && test -t 1 || exit 2; test \"$(ps -o sid= -p $$ | tr -d ' ')\" = \"$PACPIN_TEST_SESSION\" || exit 4; test \"$(readlink /proc/$$/fd/0)\" = \"$PACPIN_TEST_TTY\" || exit 5; printf 'checking keys in keyring\\n'; printf 'Answer: '; read -r answer; test \"$answer\" = yes || exit 3; printf 'native output retained\\n'; printf 'stderr retained\\n' >&2"])
                .env("PACPIN_TEST_SESSION", &session).env("PACPIN_TEST_TTY", &tty),
                Manager::Native, "Test", || background(|| source_download("app.tar.gz", 5_242_880, Some(10_485_760)))).unwrap();
            assert!(status.success());
            // sudo can disable output processing on the inherited tty while
            // relaying its own child. Output must still start each new line at
            // column one rather than producing a staircase of indented lines.
            let mut raw_output = before;
            raw_output.c_oflag &= !libc::OPOST;
            assert_eq!(unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw_output) }, 0);
            let status = run(Command::new("sh").args(["-c",
                "test \"$(ps -o sid= -p $$ | tr -d ' ')\" = \"$PACPIN_TEST_SESSION\" || exit 4; test \"$(readlink /proc/$$/fd/0)\" = \"$PACPIN_TEST_TTY\" || exit 5; printf 'source terminal retained\\nraw output line one\\nraw output line two\\n'"])
                .env("PACPIN_TEST_SESSION", &session).env("PACPIN_TEST_TTY", &tty),
                Manager::Sources, "Second operation", || {}).unwrap();
            assert!(status.success());
            assert_eq!(unsafe { libc::tcsetattr(0, libc::TCSANOW, &before) }, 0);
            clear_sources();
            queued_sources();
            let download = start_background(Command::new("sh").args(["-c",
                "test -t 1 || exit 9; printf 'Downloading fixture.pkg.tar.zst 25%%\\r'; sleep 1; printf 'Downloading fixture.pkg.tar.zst 75%%\\r'; sleep 1; exit 7",
            ])).unwrap();
            let selected = selection_operation(|| crate::ui::prompt_orphan_selection(&[
                crate::db::OrphanPackage {
                    name: "fixture-orphan".into(), version: "1-1".into(), isize: 1,
                    desc: "No host package operations".into(), is_projected: false,
                    dropped_by: Vec::new(), optional_for: Vec::new(),
                },
            ], true));
            assert_eq!(selected, ["fixture-orphan"]);
            assert_eq!(download.wait().unwrap().code(), Some(7));
            assert!(NATIVE_BACKGROUND.lock().unwrap().is_none());
            println!("BACKGROUND_WAIT_FINISHED");
            println!("FINISHED_PREFETCH_BEFORE");
            let mut finished = start_background(Command::new("sh").args(["-c", "exit 0"])).unwrap();
            while finished.child.0.try_wait().unwrap().is_none() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(finished.wait().unwrap().success());
            println!("FINISHED_PREFETCH_AFTER");
            let mut after: libc::termios = unsafe { std::mem::zeroed() };
            assert_eq!(unsafe { libc::tcgetattr(0, &mut after) }, 0);
            assert_eq!(before.c_lflag, after.c_lflag);
            assert_eq!(before.c_oflag, after.c_oflag);
            return;
        }
        let (mut master, mut slave) = (-1, -1);
        let size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &size,
                )
            },
            0
        );
        let mut master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "progress::tests::pty_preserves_prompt_output_footer_and_terminal_settings",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("TERM", "xterm")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = ChildGuard(command.spawn().unwrap());
        command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut output = Vec::new();
        let mut answered = false;
        let mut selection_confirmed = false;
        let mut auth_started = None;
        let mut auth_answered = false;
        loop {
            if !auth_answered && auth_started.is_some_and(|started: std::time::Instant| started.elapsed() >= std::time::Duration::from_millis(750)) {
                let text = String::from_utf8_lossy(&output);
                let waiting = text.split("AUTH_WAIT_PROMPT: ").nth(1).unwrap();
                assert!(!waiting.contains("\x1b7"), "footer moved the authentication cursor: {waiting}");
                master.write_all(b"fixture-secret\n").unwrap();
                auth_answered = true;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "PTY relay hung: {}",
                String::from_utf8_lossy(&output)
            );
            let mut fd = libc::pollfd {
                fd: master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut fd, 1, 100) } <= 0 {
                continue;
            }
            let mut bytes = [0u8; 4096];
            match master.read(&mut bytes) {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&bytes[..n]),
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) => panic!("PTY read failed: {error}"),
            }
            if !answered && String::from_utf8_lossy(&output).contains("Answer: ") {
                master.write_all(b"yes\n").unwrap();
                answered = true;
                let resized = libc::winsize {
                    ws_row: 30,
                    ws_col: 100,
                    ..size
                };
                assert_eq!(
                    unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &resized) },
                    0
                );
            }
            if auth_started.is_none() && String::from_utf8_lossy(&output).contains("AUTH_WAIT_PROMPT: ") {
                auth_started = Some(std::time::Instant::now());
                let resized = libc::winsize { ws_row: 18, ws_col: 40, ..size };
                assert_eq!(unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &resized) }, 0);
            }
            if !selection_confirmed && String::from_utf8_lossy(&output).contains("Enter confirms") {
                master.write_all(b"\r").unwrap();
                selection_confirmed = true;
            }
        }
        assert!(
            child.0.wait().unwrap().success(),
            "{}",
            String::from_utf8_lossy(&output)
        );
        let output = String::from_utf8_lossy(&output);
        assert!(auth_answered && output.contains("AUTH_WAIT_COMPLETE"));
        assert!(!output.contains("fixture-secret"), "authentication input was echoed");
        let phases = output.split("PHASE_PADDING_START\r\n").nth(1).unwrap().split("PHASE_PADDING_END").next().unwrap();
        assert!(!phases.contains('\n'), "footer phases inserted blank lines: {phases}");
        let finished_wait = output.split("FINISHED_PREFETCH_BEFORE").nth(1).unwrap().split("FINISHED_PREFETCH_AFTER").next().unwrap();
        assert!(!finished_wait.contains("Waiting for repository downloads"), "already-finished download displayed a wait: {output}");
        assert!(!finished_wait.contains("Pacman:"), "already-finished download displayed an active footer: {output}");
        assert!(output.contains("No host package operations\r\n\r\nPure orphans are pre-selected ([x]). Optional plugins are unchecked ([ ]).\r\n\r\n"), "selector sections have no blank line: {output}");
        // A footer refresh may occur between a line and its newline. Remove
        // only the DEC save/restore display updates before checking CRLF.
        let mut normal_output = output.to_string();
        while let Some(start) = normal_output.find("\x1b7") {
            let Some(end) = normal_output[start..].find("\x1b8") else { break; };
            normal_output.replace_range(start..start + end + 2, "");
        }
        assert!(normal_output.contains("raw output line one\r\nraw output line two\r\n"), "newline processing lost: {output}");
        for text in [
            "Checks: Checking update candidates",
            "Review: Awaiting plan approval",
            "Records: Saving upgrade history",
            "Restart: Checking restart requirements",
            "AUR: ✗ Operation failed",
            "Pacman: ✔ Upgrade complete",
            "Review: Choosing orphan cleanup",
            "Hooks: Rebuilding triggered packages",
            "Pacman:",
            "Sources:",
            "50%",
            "native output retained",
            "stderr retained",
            "source terminal retained",
            "Orphan review confirmed: 1 package selected",
            "Waiting for repository downloads to finish...",
            "Downloading packages",
            "BACKGROUND_WAIT_FINISHED",
        ] {
            assert!(output.contains(text), "missing {text}: {output}");
        }
        assert!(
            !output.contains(":: Downloading app.tar.gz"),
            "worker printed into native output"
        );
    }
#[test]
fn skipped_footer_preserves_the_typed_cause() {
    assert_eq!(SkipReason::UpdateCheck.footer(Manager::Sources), "Sources: ⚠ Skipped (update check failed)");
    assert_eq!(SkipReason::MetadataRefresh.footer(Manager::Flatpak), "Flatpak: ⚠ Skipped (metadata refresh failed)");
    assert_eq!(SkipReason::UnsupportedOptions.footer(Manager::Nix), "Nix: ⚠ Skipped (unsupported options)");
}
#[test]
fn partial_source_checks_preserve_live_and_completed_source_rows() {
    let mut history = vec!["Pacman: ✔ Upgrade complete".into()];
    let mut active = vec!["Sources: Downloading 40%".into()];
    super::annotate_source_checks(&mut history, &mut active, 1);
    assert_eq!(active, ["Sources: Downloading 40% · ⚠ 1 check(s) skipped"]);
    assert_eq!(history.len(), 1);
    let mut history = vec!["Sources: ✔ Upgrade complete".into()];
    super::annotate_source_checks(&mut history, &mut [], 2);
    assert_eq!(history, ["Sources: ✔ Upgrade complete · ⚠ 2 check(s) skipped"]);
    let mut history = Vec::new();
    super::annotate_source_checks(&mut history, &mut [], 1);
    assert_eq!(history, ["Sources: ⚠ 1 check(s) skipped"]);
    let previous = history.clone();
    super::annotate_source_checks(&mut history, &mut [], 0);
    assert_eq!(history, previous);
}
