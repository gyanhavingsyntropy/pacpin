//! Shared process cancellation and ownership of registered temporary resources.
//! Manager and UI policy remain with their respective adapters.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

mod capture;
mod resources;
pub use resources::remove_owned_tree;
pub(crate) use capture::{capture_output, capture_output_with_limit};
pub(crate) use capture::capture_output_observed;
#[cfg(test)]
mod tests;

/// Optional UI cleanup. Execution can be used without a terminal renderer.
static INTERRUPT_RESTORE: OnceLock<fn()> = OnceLock::new();
pub(crate) fn register_interrupt_restore(restore: fn()) {
    let _ = INTERRUPT_RESTORE.set(restore);
}

static CLEANUP_TARGETS: resources::Registry = resources::Registry::new();
static ACTIVE_CHILDREN: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static ACTIVE_GROUPS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static TERMINATING: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(crate) static CHILD_TEST_LOCK: Mutex<()> = Mutex::new(());

pub fn register_child(pid: u32) {
    if let Ok(mut children) = ACTIVE_CHILDREN.lock() { children.push(pid); }
}

pub fn unregister_child(pid: u32) {
    if let Ok(mut children) = ACTIVE_CHILDREN.lock() { children.retain(|&active| active != pid); }
    if let Ok(mut groups) = ACTIVE_GROUPS.lock() { groups.retain(|&active| active != pid); }
}
pub fn register_child_group(pid: u32) {
    if let Ok(mut groups) = ACTIVE_GROUPS.lock() { groups.push(pid); }
    register_child(pid);
}

pub fn is_terminating() -> bool {
    TERMINATING.load(Ordering::SeqCst)
}

pub fn run_tracked_command(command: &mut Command) -> io::Result<std::process::ExitStatus> {
    if is_terminating() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "pacpin is terminating"));
    }
    let mut child = command.spawn()?;
    let pid = child.id();
    if let Ok(mut lock) = ACTIVE_CHILDREN.lock() {
        lock.push(pid);
    }
    if is_terminating() {
        let _ = child.kill();
    }
    let status = child.wait();
    if let Ok(mut lock) = ACTIVE_CHILDREN.lock() {
        lock.retain(|&active| active != pid);
    }
    if is_terminating() {
        Err(io::Error::new(io::ErrorKind::Interrupted, "pacpin is terminating"))
    } else {
        status
    }
}

fn kill_active_children() {
    let children = ACTIVE_CHILDREN.lock().map(|mut lock| std::mem::take(&mut *lock)).unwrap_or_default();
    let groups = ACTIVE_GROUPS.lock().map(|mut lock| std::mem::take(&mut *lock)).unwrap_or_default();
    terminate_children(children, groups);
}

fn terminate_children(mut children: Vec<u32>, groups: Vec<u32>) {
    // Let sudo relay interruption and let pacman release its database lock.
    // An immediate SIGKILL can leave an interrupted download's lock behind.
    for &pid in &children {
        let target = if groups.contains(&pid) { -(pid as i32) } else { pid as i32 };
        unsafe { libc::kill(target, libc::SIGINT); }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !children.is_empty() && std::time::Instant::now() < deadline {
        children.retain(|&pid| {
            if unsafe { libc::kill(pid as i32, 0) } != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) { return false; }
            !fs::read_to_string(format!("/proc/{pid}/stat")).ok()
                .and_then(|stat| stat.rsplit_once(") ").map(|(_, rest)| rest.starts_with('Z')))
                .unwrap_or(false)
        });
        if !children.is_empty() { std::thread::sleep(std::time::Duration::from_millis(20)); }
    }
    for pid in children {
        unsafe { libc::kill(pid as i32, libc::SIGKILL); }
    }
    for pid in groups { unsafe { libc::kill(-(pid as i32), libc::SIGKILL); } }
}

pub fn register_cleanup_target(path: PathBuf) {
    CLEANUP_TARGETS.register(path);
}

pub fn unregister_cleanup_target(path: &Path) {
    CLEANUP_TARGETS.unregister(path);
}

pub fn cleanup_active_targets() {
    CLEANUP_TARGETS.cleanup();
}

pub fn init_signal_handlers() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = ctrlc::set_handler(|| interrupt());
    });
}

/// Sudo may own the foreground terminal when pacman receives Ctrl-C. Its
/// interruption diagnostic must use the same cleanup path as our own SIGINT.
pub fn interrupt() -> ! {
    if TERMINATING.swap(true, Ordering::SeqCst) {
        // Another thread owns cleanup; let it release native locks and exit.
        loop { std::thread::park_timeout(std::time::Duration::from_millis(20)); }
    }
    kill_active_children();
    cleanup_active_targets();
    if let Some(restore) = INTERRUPT_RESTORE.get() { restore(); }
    std::process::exit(130);
}

pub(crate) fn ensure_not_terminating() -> io::Result<()> {
    // A manager may finish successfully while SIGINT interrupts its hook.
    // The signal handler is still cleaning up; do not race it by accepting
    // that status or starting another operation.
    if crate::execution::is_terminating() {
        crate::execution::interrupt()
    } else { Ok(()) }
}

pub(crate) struct ChildGuard(pub(crate) std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
        crate::execution::unregister_child(self.0.id());
    }
}
