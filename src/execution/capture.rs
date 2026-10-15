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
    use std::os::unix::process::CommandExt;
    ensure_not_terminating()?;
    fn read_bounded(mut stream: impl Read, limit: usize, overflow: std::sync::Arc<std::sync::atomic::AtomicBool>) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let count = stream.read(&mut buffer)?;
            if count == 0 { return Ok(bytes); }
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
    let output = std::thread::spawn(move || read_bounded(stdout, limit, out_overflow));
    let errors = std::thread::spawn(move || read_bounded(stderr, limit, err_overflow));
    let start = std::time::Instant::now();
    let status = loop {
        if overflow.load(std::sync::atomic::Ordering::SeqCst) {
            break Err(io::Error::other("Command output exceeds metadata limit"));
        }
        if start.elapsed() >= timeout {
            break Err(io::Error::new(io::ErrorKind::TimedOut, "Metadata command timed out"));
        }
        if is_terminating() {
            break Err(io::Error::new(io::ErrorKind::Interrupted, "pacpin is terminating"));
        }
        match child.0.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(1)),
            Err(error) => break Err(error),
        }
    };
    if status.is_err() { let _ = child.0.kill(); let _ = child.0.wait(); }
    // Metadata scripts must not leave descendants holding capture pipes open.
    unsafe { libc::kill(-(child.0.id() as i32), libc::SIGKILL); }
    let stdout = output.join().map_err(|_| io::Error::other("Output reader failed"))??;
    let stderr = errors.join().map_err(|_| io::Error::other("Error reader failed"))??;
    if overflow.load(std::sync::atomic::Ordering::SeqCst) { return Err(io::Error::other("Command output exceeds metadata limit")); }
    ensure_not_terminating()?;
    Ok(std::process::Output { status: status?, stdout, stderr })
}
