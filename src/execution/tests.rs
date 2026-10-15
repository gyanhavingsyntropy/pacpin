use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn create_secure_temp_dir(prefix: &str) -> Result<PathBuf, String> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

fn wait_for_test_file(path: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !path.exists() {
        assert!(std::time::Instant::now() < deadline, "child fixture did not become ready");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
    #[test]
    fn test_cleanup_registry() {
        let registry = resources::Registry::new();
        let unrelated = resources::Registry::new();
        let temp_dir = create_secure_temp_dir("pacpin-test-cleanup-reg").unwrap();
        let other = create_secure_temp_dir("pacpin-test-cleanup-other").unwrap();
        unrelated.register(other.clone());
        assert!(temp_dir.exists());

        registry.register(temp_dir.clone());
        registry.register(temp_dir.clone());
        registry.cleanup();

        assert!(!temp_dir.exists());
        assert!(other.exists(), "one registry cleaned another owner's workspace");
        unrelated.unregister(&other);
        unrelated.cleanup();
        assert!(other.exists(), "unregistered workspace was deleted");
        fs::remove_dir(other).unwrap();
    }

#[cfg(unix)]
    #[test]
    fn interruption_allows_child_cleanup_before_forceful_termination() {
        let _serial = CHILD_TEST_LOCK.lock().unwrap();
        let fixture = create_secure_temp_dir("pacpin-run-interrupt-test").unwrap();
        let ready = fixture.join("ready");
        let cleaned = fixture.join("cleaned");
        let handler = format!("touch {}; exit 0", crate::utils::shell_quote(&cleaned.to_string_lossy()));
        let script = format!("trap {} INT; printf '%s' \"$$\" > {}; while :; do sleep .02; done", crate::utils::shell_quote(&handler), crate::utils::shell_quote(&ready.to_string_lossy()));
        let worker = std::thread::spawn(move || run_tracked_command(Command::new("bash").args(["-c", &script])).unwrap());
        wait_for_test_file(&ready);
        let pid = fs::read_to_string(&ready).unwrap().parse().unwrap();
        terminate_children(vec![pid], Vec::new());
        assert!(worker.join().unwrap().success());
        assert!(cleaned.exists(), "child did not get a chance to clean up");
        fs::remove_dir_all(fixture).unwrap();
    }

#[cfg(unix)]
    #[test]
    fn test_active_download_children_are_killed_before_cleanup() {
        let _serial = CHILD_TEST_LOCK.lock().unwrap();
        let fixture = create_secure_temp_dir("pacpin-run-kill-test").unwrap();
        let ready = fixture.join("ready");
        let script = format!("printf '%s' \"$$\" > {}; exec sleep 30", crate::utils::shell_quote(&ready.to_string_lossy()));
        let worker = std::thread::spawn(move || {
            run_tracked_command(Command::new("sh").args(["-c", &script])).unwrap()
        });
        // Concurrent ALPM metadata tests also register children. Seeing one
        // arbitrary PID is not proof that this worker has started.
        wait_for_test_file(&ready);
        let pid = fs::read_to_string(&ready).unwrap().parse().unwrap();
        terminate_children(vec![pid], Vec::new());
        assert!(!worker.join().unwrap().success());
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn bounded_capture_drains_both_streams_and_preserves_failure() {
        let _guard = CHILD_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let output = capture_output(Command::new("/bin/sh").args(["-c", "head -c 16384 /dev/zero; head -c 16384 /dev/zero >&2; exit 7"])).unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout.len(), 16384); assert_eq!(output.stderr.len(), 16384);
        assert!(capture_output(Command::new("/bin/sh").args(["-c", "head -c 65536 /dev/zero"])).is_err());
        let start = std::time::Instant::now();
        assert!(capture_output_with_limit(Command::new("/bin/sh").args(["-c", "sleep 30 & wait"]), 1024, std::time::Duration::from_millis(50)).is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }
