//! Bounded metadata capture; drains both output streams and reaps descendants.
use std::io::{self, Read};
use std::process::{Command, Stdio};
use super::{ChildGuard, ensure_not_terminating, register_child_group, is_terminating};

/// Bounded read-only tool capture with the same child tracking as foreground
/// operations. Drain both pipes concurrently even after the capture limit.
pub fn capture_output(command: &mut Command) -> io::Result<std::process::Output> {
    capture_output_with_limit(command, 32_768, std::time::Duration::from_secs(600))
}

/// Metadata must be complete: an oversized body is an error, never a silently
/// truncated successful check. Readers drain both pipes and the child is reaped
/// on timeout, overflow, cancellation, and ordinary failure.
pub fn capture_output_with_limit(command: &mut Command, limit: usize, timeout: std::time::Duration) -> io::Result<std::process::Output> {
    capture_output_observed(command, limit, timeout, std::sync::Arc::new(|_, _| {}))
}

pub(crate) type OutputObserver = std::sync::Arc<dyn Fn(bool, &[u8]) + Send + Sync>;
pub(crate) fn capture_output_observed(command: &mut Command, limit: usize, timeout: std::time::Duration, observe: OutputObserver) -> io::Result<std::process::Output> {
    use std::os::unix::process::CommandExt;
    ensure_not_terminating()?;
    fn read_bounded(mut stream: impl Read, limit: usize, overflow: std::sync::Arc<std::sync::atomic::AtomicBool>, stderr: bool, observe: OutputObserver) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let count = stream.read(&mut buffer)?;
            if count == 0 { return Ok(bytes); }
            observe(stderr, &buffer[..count]);
            let keep = count.min(limit.saturating_sub(bytes.len()));
            if keep != count { overflow.store(true, std::sync::atomic::Ordering::SeqCst); }
            bytes.extend_from_slice(&buffer[..keep]);
        }
    }
    command.process_group(0).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = ChildGuard(command.spawn()?);
    register_child_group(child.0.id());
    let stdout = child.0.stdout.take().ok_or_else(|| io::Error::other("Missing stdout pipe"))?;
    let stderr = child.0.stderr.take().ok_or_else(|| io::Error::other("Missing stderr pipe"))?;
    let overflow = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let out_overflow = std::sync::Arc::clone(&overflow);
    let err_overflow = std::sync::Arc::clone(&overflow);
    let out_observe = observe.clone();
    let output = std::thread::spawn(move || read_bounded(stdout, limit, out_overflow, false, out_observe));
    let errors = std::thread::spawn(move || read_bounded(stderr, limit, err_overflow, true, observe));
    let start = std::time::Instant::now();
    let pid = child.0.id();
    let mut backoff_ms = 1u64;
    let status = loop {
        if overflow.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = super::signal_process_group(pid, libc::SIGKILL);
            let _ = child.0.kill();
            let _ = child.0.wait();
            break Err(io::Error::other("Command output exceeds metadata limit"));
        }
        if start.elapsed() >= timeout {
            let _ = super::signal_process_group(pid, libc::SIGKILL);
            let _ = child.0.kill();
            let _ = child.0.wait();
            break Err(io::Error::new(io::ErrorKind::TimedOut, "Metadata command timed out"));
        }
        if is_terminating() {
            let _ = super::signal_process_group(pid, libc::SIGKILL);
            let _ = child.0.kill();
            let _ = child.0.wait();
            break Err(io::Error::new(io::ErrorKind::Interrupted, "pacpin is terminating"));
        }
        #[cfg(unix)]
        {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let (exited, _error) = unsafe {
                let res = libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                );
                (res == 0 && info.si_pid() != 0, res != 0)
            };
            if exited {
                // Signal process group before reaping leader to avoid PID-reuse hazard
                let _ = super::signal_process_group(pid, libc::SIGKILL);
                break child.0.wait();
            }
        }
        #[cfg(not(unix))]
        {
            if let Ok(Some(status)) = child.0.try_wait() {
                break Ok(status);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
        backoff_ms = (backoff_ms.saturating_mul(2)).min(20);
    };
    if status.is_err() {
        let _ = super::signal_process_group(pid, libc::SIGKILL);
        let _ = child.0.kill();
        let _ = child.0.wait();
    }
    let stdout = output.join().map_err(|_| io::Error::other("Output reader failed"))??;
    let stderr = errors.join().map_err(|_| io::Error::other("Error reader failed"))??;
    if overflow.load(std::sync::atomic::Ordering::SeqCst) { return Err(io::Error::other("Command output exceeds metadata limit")); }
    ensure_not_terminating()?;
    Ok(std::process::Output { status: status?, stdout, stderr })
}
