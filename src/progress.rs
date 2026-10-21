//! Own the terminal while a package manager runs, keeping worker output out of
//! its cursor-based progress display. A PTY preserves prompts and native bars.
mod metrics;
mod observer;
mod stages;
mod overview;
pub(crate) mod transfers;
pub use stages::{activity, pending, stop_pending};
#[cfg(test)]
mod tests;
use observer::{Observer, clean_text};
use metrics::{NativeDownloads, download_text, footer_download, native_footer, phase_text, scoped_phase};

impl NativeDownloads {
    fn current() -> Self {
        NATIVE_DOWNLOADS.with(|targets| Self::new(&targets.borrow()))
    }
}

use std::cell::{Cell, RefCell};
use crate::execution::{ChildGuard, ensure_not_terminating};
use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Mutex;

#[derive(Default)]
struct SourceState {
    text: String,
    downloaded: u64,
    total: Option<u64>,
}
static SOURCE: Mutex<SourceState> = Mutex::new(SourceState {
    text: String::new(),
    downloaded: 0,
    total: None,
});
thread_local! { static BACKGROUND: Cell<bool> = const { Cell::new(false) }; }
thread_local! { static SOURCE_DISPLAY: RefCell<Option<Footer>> = const { RefCell::new(None) }; }
static TERMINAL: Mutex<Option<(libc::termios, u16)>> = Mutex::new(None);
static VIEW: Mutex<Option<(u16, u16, u16)>> = Mutex::new(None);
static SESSION_VIEW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Keep one output region across an invocation's successive footer phases.
pub struct SessionView;
pub fn session_view() -> SessionView {
    crate::execution::register_interrupt_restore(restore_terminal);
    SESSION_VIEW.store(true, std::sync::atomic::Ordering::Relaxed);
    extern "C" fn restore_at_exit() { restore_terminal(); }
    // process::exit bypasses Rust destructors in several command handlers.
    unsafe { libc::atexit(restore_at_exit); }
    SessionView
}
impl Drop for SessionView {
    fn drop(&mut self) {
        SESSION_VIEW.store(false, std::sync::atomic::Ordering::Relaxed);
        restore_terminal();
    }
}
static COMPLETED: Mutex<Vec<Manager>> = Mutex::new(Vec::new());
static FAILED: Mutex<Vec<Manager>> = Mutex::new(Vec::new());
static SKIPPED: Mutex<Vec<(Manager, SkipReason)>> = Mutex::new(Vec::new());
static SOURCE_CHECK_FAILURES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub(crate) fn source_checks_failed(count: usize) {
    SOURCE_CHECK_FAILURES.store(count, std::sync::atomic::Ordering::Relaxed);
}

#[derive(Clone, Copy)]
pub(crate) enum SkipReason {
    UnsupportedOptions,
    UpdateCheck,
    MetadataRefresh,
}

impl SkipReason {
    fn label(self) -> &'static str {
        match self {
            Self::UnsupportedOptions => "unsupported options",
            Self::UpdateCheck => "update check failed",
            Self::MetadataRefresh => "metadata refresh failed",
        }
    }
    fn footer(self, manager: Manager) -> String {
        format!("{}: ⚠ Skipped ({})", manager.label(), self.label())
    }
}
static NATIVE_BACKGROUND: Mutex<Option<String>> = Mutex::new(None);
static CONTEXT: Mutex<Option<(Manager, String)>> = Mutex::new(None);
static PREPARING: Mutex<Vec<(Manager, String)>> = Mutex::new(Vec::new());

/// Download-only children use bounded tracked capture and never own the tty.
pub(crate) fn capture_preparation(command: &mut Command, manager: Manager) -> io::Result<()> {
    command.env("LC_ALL", "C");
    struct Reset(Manager);
    impl Drop for Reset { fn drop(&mut self) { PREPARING.lock().unwrap_or_else(|e| e.into_inner()).retain(|(m, _)| *m != self.0); } }
    PREPARING.lock().unwrap_or_else(|e| e.into_inner()).push((manager, "Preparing downloads".into()));
    let _reset = Reset(manager);
    let observers = std::sync::Arc::new(Mutex::new([Observer::default(), Observer::default()]));
    let output = crate::execution::capture_output_observed(command, 1_048_576, std::time::Duration::from_secs(600), std::sync::Arc::new(move |stderr, bytes| {
        let mut observers = observers.lock().unwrap_or_else(|e| e.into_inner());
        let observer = &mut observers[usize::from(stderr)];
        observer.feed(bytes, &mut || {});
        if let Some((_, value)) = PREPARING.lock().unwrap_or_else(|e| e.into_inner()).iter_mut().find(|(m, _)| *m == manager) {
            *value = phase_text(manager, &observer.display(), value);
        }
    }))?;
    if output.status.success() { Ok(()) }
    else { Err(io::Error::other(format!("download-only command exited with {}: {}", output.status, crate::utils::sanitize_display_text(&String::from_utf8_lossy(&output.stderr)).chars().take(512).collect::<String>()))) }
}
thread_local! { static SELECTION_DISPLAY: RefCell<Option<Footer>> = const { RefCell::new(None) }; }
thread_local! { static NATIVE_DOWNLOADS: RefCell<Vec<(String, u64)>> = const { RefCell::new(Vec::new()) }; }

/// The native boundary supplies uncached archive sizes from the approved plan.
/// A scoped value also reaches a background reader without global cross-talk.
pub fn native_downloads<T>(targets: Vec<(String, u64)>, work: impl FnOnce() -> T) -> T {
    struct Reset(Vec<(String, u64)>);
    impl Drop for Reset {
        fn drop(&mut self) { NATIVE_DOWNLOADS.with(|d| { d.replace(std::mem::take(&mut self.0)); }); }
    }
    let _reset = Reset(NATIVE_DOWNLOADS.with(|d| d.replace(targets)));
    work()
}

pub fn begin_transaction() {
    source_checks_failed(0);
    stages::clear();
    COMPLETED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    FAILED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    SKIPPED.lock().unwrap_or_else(|e| e.into_inner()).clear();
}
pub fn completed(manager: Manager) {
    stages::finished(manager);
    SKIPPED.lock().unwrap_or_else(|e| e.into_inner()).retain(|&(m, _)| m != manager);
    FAILED.lock().unwrap_or_else(|e| e.into_inner()).retain(|&m| m != manager);
    let mut completed = COMPLETED.lock().unwrap_or_else(|e| e.into_inner());
    if !completed.contains(&manager) { completed.push(manager); }
}

pub fn failed(manager: Manager) {
    stages::finished(manager);
    SKIPPED.lock().unwrap_or_else(|e| e.into_inner()).retain(|&(m, _)| m != manager);
    COMPLETED.lock().unwrap_or_else(|e| e.into_inner()).retain(|&m| m != manager);
    let mut failed = FAILED.lock().unwrap_or_else(|e| e.into_inner());
    if !failed.contains(&manager) { failed.push(manager); }
}

pub(crate) fn skipped(manager: Manager, reason: SkipReason) {
    stages::finished(manager);
    COMPLETED.lock().unwrap_or_else(|e| e.into_inner()).retain(|&m| m != manager);
    FAILED.lock().unwrap_or_else(|e| e.into_inner()).retain(|&m| m != manager);
    let mut skipped = SKIPPED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = skipped.iter_mut().find(|(m, _)| *m == manager) { entry.1 = reason; }
    else { skipped.push((manager, reason)); }
}

/// An outer operation may own work performed by another manager (an AUR
/// rebuild triggered by a hook). Add its actual context without another footer.
pub fn context<T>(manager: Manager, label: &str, work: impl FnOnce() -> T) -> T {
    struct Reset(Option<(Manager, String)>);
    impl Drop for Reset {
        fn drop(&mut self) { *CONTEXT.lock().unwrap_or_else(|e| e.into_inner()) = self.0.take(); }
    }
    let _reset = Reset(CONTEXT.lock().unwrap_or_else(|e| e.into_inner()).replace((manager, label.into())));
    work()
}

/// Display a phase owned by pacpin itself. Child processes use `run` instead;
/// nesting a phase must never replace the terminal restoration owner.
pub fn operation<T>(manager: Manager, label: &str, work: impl FnOnce() -> T) -> T {
    let label = label.to_owned();
    operation_dynamic(manager, move || label.clone(), work)
}

/// Refresh a pacpin-owned phase from live measurements; child execution and
/// terminal restoration remain owned by the same shared renderer.
pub fn operation_dynamic<T>(manager: Manager, label: impl Fn() -> String + Send + 'static, work: impl FnOnce() -> T) -> T {
    crate::execution::init_signal_handlers();
    struct Display {
        stop: std::sync::mpsc::Sender<()>,
        worker: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Display {
        fn drop(&mut self) {
            let _ = self.stop.send(());
            if let Some(worker) = self.worker.take() { let _ = worker.join(); }
        }
    }
    let footer = if interactive() && TERMINAL.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
        Footer::new(manager, false).ok()
    } else { None };
    let _display = footer.map(|mut footer| {
        let _ = footer.draw(&label());
        let (stop, stopped) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            while matches!(stopped.recv_timeout(std::time::Duration::from_millis(100)), Err(std::sync::mpsc::RecvTimeoutError::Timeout)) {
                if footer.resize().and_then(|_| footer.draw(&label())).is_err() { break; }
            }
        });
        Display { stop, worker: Some(worker) }
    });
    work()
}

pub fn register_input_restore(saved: libc::termios) -> bool {
    let mut terminal = TERMINAL.lock().unwrap_or_else(|e| e.into_inner());
    if terminal.is_none() { *terminal = Some((saved, 0)); true } else { false }
}
pub fn unregister_input_restore() {
    let mut terminal = TERMINAL.lock().unwrap_or_else(|e| e.into_inner());
    if terminal.as_ref().is_some_and(|(_, height)| *height == 0) { terminal.take(); }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Manager {
    Custom(u16),
    Native,
    Aur,
    Sources,
    Flatpak,
    Nix,
    Pipx,
    Hooks,
    Orphans,
    Cache,
    Refresh,
    Metadata,
    Checks,
    Review,
    Record,
    Restart,
    Runtime,
}
impl Manager {
    fn label(self) -> &'static str {
        match self {
            Self::Custom(index) => CUSTOM_NAMES.get().and_then(|names| names.get(index as usize)).map(String::as_str).unwrap_or("External"),
            Self::Native => "Pacman",
            Self::Aur => "AUR",
            Self::Sources => "Sources",
            Self::Flatpak => "Flatpak",
            Self::Nix => "Nix",
            Self::Pipx => "Pipx",
            Self::Hooks => "Hooks",
            Self::Orphans => "Orphans",
            Self::Cache => "Cache",
            Self::Refresh => "Repositories",
            Self::Metadata => "Metadata",
            Self::Checks => "Checks",
            Self::Review => "Review",
            Self::Record => "Records",
            Self::Restart => "Restart",
            Self::Runtime => "App",
        }
    }

    fn completion(self) -> &'static str {
        match self {
            Self::Orphans => "Removal complete",
            Self::Cache => "Cleanup complete",
            Self::Hooks => "Hooks complete",
            Self::Refresh => "Refresh complete",
            Self::Metadata => "Metadata refreshed",
            Self::Checks => "Checks complete",
            Self::Review => "Review complete",
            Self::Record => "Records saved",
            Self::Restart => "Restart check complete",
            Self::Runtime => "Preparation complete",
            _ => "Upgrade complete",
        }
    }
}
static CUSTOM_NAMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
pub fn configure_managers(names: Vec<String>) { let _ = CUSTOM_NAMES.set(names); }
pub fn integration_manager(name: &str) -> Manager {
    match name {
        "Sources" => Manager::Sources, "Flatpak" => Manager::Flatpak,
        "Nix" => Manager::Nix, "Pipx" => Manager::Pipx,
        _ => Manager::Custom(CUSTOM_NAMES.get().and_then(|names| names.iter().position(|n| n == name)).unwrap_or(0) as u16),
    }
}

fn interactive() -> bool {
    io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && std::env::var("TERM").is_ok_and(|term| term != "dumb")
}

pub fn source_operation<T>(work: impl FnOnce() -> T) -> T {
    if BACKGROUND.get() || !interactive() {
        return work();
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            SOURCE_DISPLAY.with(|display| {
                display.borrow_mut().take();
            });
        }
    }
    let _reset = Reset;
    SOURCE_DISPLAY.with(|display| {
        // No child PTY owns input during a foreground download. Retain ISIG
        // so Ctrl-C reaches our cleanup handler instead of becoming unread data.
        *display.borrow_mut() = Footer::new(Manager::Sources, false).ok();
        if let Some(footer) = display.borrow().as_ref() { let _ = footer.draw("Preparing source download"); }
    });
    work()
}

pub fn background<T>(work: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            BACKGROUND.set(self.0);
        }
    }
    let _reset = Reset(BACKGROUND.replace(true));
    work()
}

pub fn source_message(text: String) {
    if BACKGROUND.get() {
        let mut state = SOURCE.lock().unwrap_or_else(|e| e.into_inner());
        state.text = text
            .strip_prefix(":: Sources: ")
            .unwrap_or(&text)
            .to_owned();
        state.downloaded = 0;
        state.total = None;
    } else {
        SOURCE_DISPLAY.with(|display| {
            if let Some(footer) = display.borrow_mut().as_mut() {
                let _ = footer.resize();
                let _ = write!(io::stdout(), "{}\r\n", text.replace('\n', "\r\n"));
                let _ = footer.draw(&phase_text(Manager::Sources, &text, "Preparing source package"));
            } else {
                println!("{text}");
            }
        });
    }
}

pub fn source_download(name: &str, downloaded: u64, total: Option<u64>) {
    if BACKGROUND.get() {
        let mut state = SOURCE.lock().unwrap_or_else(|e| e.into_inner());
        state.text = name.into();
        state.downloaded = downloaded;
        state.total = total;
    } else {
        SOURCE_DISPLAY.with(|display| {
            if let Some(footer) = display.borrow_mut().as_mut() {
                let _ = footer.resize();
                let _ = footer.draw(&footer_download(
                    "Downloading",
                    downloaded,
                    total,
                    footer.cols.saturating_sub(10) as usize,
                ));
            } else {
                println!(":: Downloading {}", download_text(name, downloaded, total));
            }
        });
    }
}

#[cfg(test)]
fn queued_sources() {
    let mut state = SOURCE.lock().unwrap_or_else(|e| e.into_inner());
    *state = SourceState {
        text: "Waiting for repository downloads to finish".into(),
        ..Default::default()
    };
}

pub fn finish_sources() {
    let mut state = SOURCE.lock().unwrap_or_else(|e| e.into_inner());
    if !state.text.is_empty() {
        println!(":: Sources: {}", state.text);
    }
    *state = SourceState::default();
}

pub fn clear_sources() {
    *SOURCE.lock().unwrap_or_else(|e| e.into_inner()) = SourceState::default();
}

/// Also called by the signal handler, which exits without running Drop.
pub fn restore_terminal() {
    let saved = TERMINAL.lock().unwrap_or_else(|e| e.into_inner()).take();
    let view = VIEW.lock().unwrap_or_else(|e| e.into_inner()).take();
    let height = saved.as_ref().map(|(_, height)| *height).filter(|height| *height != 0)
        .or_else(|| view.map(|(_, _, height)| height)).unwrap_or(0);
    if saved.is_some() || height != 0 {
        let bottom = terminal_size().ws_row.saturating_sub(height).max(1);
        // SAFETY: saved was returned by tcgetattr for this process's stdin.
        unsafe {
            if let Some((saved, _)) = saved { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved); }
            // Resetting the scroll region homes the cursor. Preserve it so
            // subsequent source logs cannot overwrite earlier pacman text.
            // Signal exit skips Footer::drop. Clear its reserved rows here too.
            let reset = if height == 0 { "\x1b[0m\x1b[?25h".into() } else { format!("\x1b7\x1b[{};1H\x1b[J\x1b[r\x1b8\x1b[0m\x1b[?25h", bottom + 1) };
            libc::write(libc::STDOUT_FILENO, reset.as_ptr().cast(), reset.len());
        }
    }
}

struct Footer {
    rows: u16,
    cols: u16,
    manager: Manager,
    height: u16,
    input_echo: bool,
}
impl Footer {
    fn new(manager: Manager, raw_input: bool) -> io::Result<Self> {
        Self::create(manager, raw_input, true)
    }

    fn pending(manager: Manager) -> io::Result<Self> {
        Self::create(manager, false, false)
    }

    fn create(manager: Manager, raw_input: bool, display: bool) -> io::Result<Self> {
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(0, &mut saved) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = saved;
        if raw_input {
            unsafe {
                libc::cfmakeraw(&mut raw);
            }
        }
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut footer = Self {
            rows: 0,
            cols: 0,
            manager,
            height: 0,
            input_echo: saved.c_lflag & libc::ECHO != 0,
        };
        *TERMINAL.lock().unwrap_or_else(|e| e.into_inner()) = Some((saved, footer.height));
        if let Some((rows, cols, height)) = VIEW.lock().unwrap_or_else(|e| e.into_inner()).take() {
            footer.rows = rows;
            footer.cols = cols;
            footer.height = height;
            if let Some((_, saved_height)) = TERMINAL.lock().unwrap_or_else(|e| e.into_inner()).as_mut() { *saved_height = height; }
        }
        // Release an earlier region before a fresh password prompt can appear.
        if !display { footer.suspend()?; }
        if display { footer.resize()?; }
        Ok(footer)
    }

    fn suspend(&mut self) -> io::Result<()> {
        if self.height == 0 { return Ok(()); }
        let clear_row = footer_cleanup_row(self.rows, terminal_size().ws_row, self.height);
        let mut out = io::stdout().lock();
        write!(out, "\x1b7\x1b[{clear_row};1H\x1b[J\x1b[r\x1b8")?;
        out.flush()?;
        self.height = 0;
        if let Some((_, height)) = TERMINAL.lock().unwrap_or_else(|e| e.into_inner()).as_mut() { *height = 0; }
        Ok(())
    }

    fn resize(&mut self) -> io::Result<bool> {
        let size = terminal_size();
        let height = 1 + self.lines("", size.ws_row, size.ws_col).len() as u16;
        if (self.rows, self.cols, self.height) == (size.ws_row, size.ws_col, height) {
            return Ok(false);
        }
        let mut out = io::stdout().lock();
        // Complete terminal setup before spawning a child: sudo writes its
        // prompt directly to /dev/tty, outside our buffered output relay.
        write!(out, "\x1b7\x1b[r\x1b8")?;
        if self.height != 0 {
            // Erase the previous footer before scrolling reserves the new
            // region. On shrink its rows are clipped to the new viewport;
            // on expansion they remain at their earlier positions. Skipping
            // this cleanup on resize moves old footer text into main output.
            let clear_row = footer_cleanup_row(self.rows, size.ws_row, self.height);
            write!(out, "\x1b7\x1b[{clear_row};1H\x1b[J\x1b8")?;
        }
        // Move existing text into scrollback and keep its cursor in place
        // relative to that text. Never pad each phase with newline characters
        // or jump the main cursor to the bottom of an otherwise empty screen.
        let reserve = if (self.rows, self.cols) != (size.ws_row, size.ws_col) { height } else { height.saturating_sub(self.height) };
        if reserve != 0 { write!(out, "\x1b[{reserve}S\x1b[{reserve}A")?; }
        self.rows = size.ws_row;
        self.cols = size.ws_col;
        self.height = height;
        if let Some((_, saved_height)) = TERMINAL.lock().unwrap_or_else(|e| e.into_inner()).as_mut() { *saved_height = height; }
        // Always retain room for normal output and the active stages.
        let bottom = self.rows.saturating_sub(self.height).max(1);
        write!(out, "\x1b7\x1b[1;{bottom}r\x1b8")?;
        out.flush()?;
        Ok(true)
    }

    fn authentication_input(&self) -> bool {
        let mut current: libc::termios = unsafe { std::mem::zeroed() };
        self.input_echo && unsafe { libc::tcgetattr(0, &mut current) } == 0 && current.c_lflag & libc::ECHO == 0
    }

    fn lines(&self, native: &str, rows: u16, cols: u16) -> Vec<String> {
        let state = SOURCE.lock().unwrap_or_else(|e| e.into_inner());
        let width = cols.saturating_sub(1) as usize;
        let source = if state.text.is_empty() {
            String::new()
        } else if state.total.is_some() || state.downloaded != 0 {
            footer_download(
                "Downloading",
                state.downloaded,
                state.total,
                width.saturating_sub(9),
            )
        } else {
            phase_text(Manager::Sources, &state.text, "Preparing download")
        };
        let label = self.manager.label();
        let current = if native.is_empty() {
            source.as_str()
        } else {
            native
        };
        let current = if self.manager == Manager::Sources {
            crate::ui::truncate_str(&clean_text(current), width.saturating_sub(label.len() + 2))
        } else {
            native_footer(current, width.saturating_sub(label.len() + 2))
        };
        let mut history: Vec<String> = COMPLETED.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|&&m| m != self.manager && overview::history_detail(m, self.manager))
            .map(|manager| format!("{}: ✔ {}", manager.label(), manager.completion())).collect();
        history.extend(FAILED.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|&&m| m != self.manager)
            .map(|manager| format!("{}: ✗ Operation failed", manager.label())));
        history.extend(SKIPPED.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|&&(m, _)| m != self.manager)
            .map(|&(manager, reason)| reason.footer(manager)));
        let stages = stages::snapshot();
        let generic_check = matches!(self.manager, Manager::Checks | Manager::Metadata)
            && matches!(native, "Checking external updates" | "Refreshing external metadata")
            && !stages.activities.is_empty();
        let mut active = Vec::new();
        let mut visible = Vec::new();
        if !generic_check {
            active.push(format!("{label}: {current}"));
            visible.push(self.manager);
        }
        if let Some((manager, phase)) = CONTEXT.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if *manager != self.manager && (*manager != Manager::Metadata || stages.activities.is_empty()) {
                active.push(format!("{}: {phase}", manager.label()));
                visible.push(*manager);
            }
        }
        if self.manager != Manager::Native {
            if let Some(phase) = NATIVE_BACKGROUND.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                active.push(format!("Pacman: {phase}"));
                visible.push(Manager::Native);
            }
        }
        if self.manager != Manager::Sources && !state.text.is_empty() {
            active.push(format!("Sources: {source}"));
            visible.push(Manager::Sources);
        }
        for (manager, phase) in PREPARING.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            if !visible.contains(manager) { active.push(format!("{}: {phase}", manager.label())); visible.push(*manager); }
        }
        stages.append(&mut active, &mut visible);
        let failed_checks = SOURCE_CHECK_FAILURES.load(std::sync::atomic::Ordering::Relaxed);
        annotate_source_checks(&mut history, &mut active, failed_checks);
        fit_rows(history, active, rows.saturating_sub(5) as usize).into_iter()
            .map(|line| crate::ui::truncate_str(&clean_text(&line), width)).collect()
    }

    fn draw(&self, native: &str) -> io::Result<()> {
        let size = terminal_size();
        if (self.rows, self.cols) != (size.ws_row, size.ws_col) {
            // A resize can occur after the caller checked the viewport. Never
            // send coordinates/widths from the previous size to the terminal.
            return Ok(());
        }
        let row = self.rows.saturating_sub(self.height).saturating_add(1);
        let lines = self.lines(native, self.rows, self.cols);
        // DEC save/restore does not change the package manager's output cursor.
        // DECSC/DECRC also preserve autowrap. If the window shrinks during this
        // write, clip footer text instead of wrapping it into ordinary output.
        let native_background = NATIVE_BACKGROUND.lock().unwrap_or_else(|e| e.into_inner()).is_some();
        let preparing = !PREPARING.lock().unwrap_or_else(|e| e.into_inner()).is_empty();
        let source_background = SOURCE.lock().unwrap_or_else(|e| e.into_inner()).downloaded != 0;
        let skipped: Vec<_> = SKIPPED.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(manager, _)| *manager).collect();
        let heading = overview::line(overview::Snapshot {
            current: self.manager, phase: native, pending: &stages::snapshot().pending,
            completed: &COMPLETED.lock().unwrap_or_else(|e| e.into_inner()),
            failed: &FAILED.lock().unwrap_or_else(|e| e.into_inner()), skipped: &skipped,
            source_checks_failed: SOURCE_CHECK_FAILURES.load(std::sync::atomic::Ordering::Relaxed) != 0,
            background_updates: native_background || preparing || source_background,
        }, self.cols.saturating_sub(1) as usize);
        let mut out = io::stdout().lock();
        write!(out, "\x1b7\x1b[?7l\x1b[{row};1H\x1b[0m\x1b[2K{heading}")?;
        for (index, line) in lines.iter().enumerate() {
            write!(out, "\x1b[{};1H\x1b[2K{line}", row + 1 + index as u16)?;
        }
        write!(out, "\x1b8")?;
        out.flush()
    }
}

/// A failed package check must not mark the whole Sources adapter skipped or
/// erase the healthy packages' live progress/completion.
fn annotate_source_checks(history: &mut Vec<String>, active: &mut [String], count: usize) {
    if count == 0 { return; }
    let warning = format!("⚠ {count} check(s) skipped");
    if let Some(line) = active.iter_mut().chain(history.iter_mut()).find(|line| line.starts_with("Sources: ")) {
        line.push_str(&format!(" · {warning}"));
    } else {
        history.push(format!("Sources: {warning}"));
    }
}

fn footer_cleanup_row(previous_rows: u16, current_rows: u16, height: u16) -> u16 {
    previous_rows.min(current_rows).saturating_sub(height).max(1).saturating_add(1)
}

fn fit_rows(history: Vec<String>, active: Vec<String>, capacity: usize) -> Vec<String> {
    let capacity = capacity.max(1);
    if active.len() > capacity {
        let hidden = active.len() - capacity + 1;
        return active.into_iter().take(capacity - 1)
            .chain(std::iter::once(format!("… {hidden} more stages"))).collect();
    }
    let keep = capacity.saturating_sub(active.len());
    let start = history.len().saturating_sub(keep);
    history.into_iter().skip(start).chain(active).collect()
}
impl Drop for Footer {
    fn drop(&mut self) {
        if self.height != 0 {
            let clear_row = footer_cleanup_row(self.rows, terminal_size().ws_row, self.height);
            let reset = if SESSION_VIEW.load(std::sync::atomic::Ordering::Relaxed) { "" } else { "\x1b[r" };
            let _ = write!(io::stdout(), "\x1b7\x1b[{clear_row};1H\x1b[J{reset}\x1b8");
            let _ = io::stdout().flush();
        }
        if self.height != 0 && SESSION_VIEW.load(std::sync::atomic::Ordering::Relaxed) {
            *VIEW.lock().unwrap_or_else(|e| e.into_inner()) = Some((self.rows, self.cols, self.height));
            if let Some((saved, _)) = TERMINAL.lock().unwrap_or_else(|e| e.into_inner()).take() {
                unsafe { libc::tcsetattr(0, libc::TCSANOW, &saved); }
            }
            return;
        }
        restore_terminal();
    }
}

fn terminal_size() -> libc::winsize {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut size) } != 0
        || size.ws_row < 8
        || size.ws_col < 20
    {
        size.ws_row = 24;
        size.ws_col = 80;
    }
    size
}

fn output_pty(size: &libc::winsize) -> io::Result<(File, File)> {
    let (mut master, mut slave) = (-1, -1);
    if unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), size) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openpty returned two fresh, owned descriptors.
    Ok(unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) })
}

/// Pacman's in-place bars wrap their labels in small windows. Keep its stage
/// messages in the text area; the footer still shows the current operation.
pub fn pacman_progress_args() -> &'static [&'static str] {
    if interactive() && crate::ui::get_terminal_width() < 60 {
        &["--noprogressbar"]
    } else {
        &[]
    }
}
pub fn run(
    command: &mut Command,
    manager: Manager,
    label: &str,
    on_ready: impl FnMut(),
) -> io::Result<ExitStatus> {
    run_observed(command, manager, label, on_ready, |_| {})
}

/// Observe normalized output in the shared executor, without intercepting
/// authentication input or hiding the package manager's original diagnostics.
pub fn run_observed(
    command: &mut Command,
    manager: Manager,
    label: &str,
    on_ready: impl FnMut(),
    on_line: impl FnMut(&str),
) -> io::Result<ExitStatus> {
    let result = run_observed_with_stdin(command, manager, label, on_ready, on_line, None);
    if !result.as_ref().is_ok_and(|status| status.success()) { failed(manager); }
    result
}

/// Preserve explicitly prepared input (e.g. approved cache-clean answers)
/// while sharing terminal ownership, output relay and child tracking.
pub fn run_with_stdin(command: &mut Command, manager: Manager, label: &str, stdin: Stdio) -> io::Result<ExitStatus> {
    let result = run_observed_with_stdin(command, manager, label, || {}, |_| {}, Some(stdin));
    if !result.as_ref().is_ok_and(|status| status.success()) { failed(manager); }
    result
}

fn run_observed_with_stdin(
    command: &mut Command,
    manager: Manager,
    label: &str,
    mut on_ready: impl FnMut(),
    mut on_line: impl FnMut(&str),
    stdin: Option<Stdio>,
) -> io::Result<ExitStatus> {
    crate::execution::init_signal_handlers();
    ensure_not_terminating()?;
    // A later operation for the same manager is pending again (e.g. an ABI
    // rebuild hook after an AUR upgrade). Retain only other completed managers.
    COMPLETED.lock().unwrap_or_else(|e| e.into_inner()).retain(|&done| done != manager);
    FAILED.lock().unwrap_or_else(|e| e.into_inner()).retain(|&done| done != manager);
    let interactive = interactive();
    let mut phase = if matches!(command.get_program().to_str(), Some("sudo" | "pkexec" | "doas")) && !(interactive && crate::privilege::authenticated(command.get_program())) { "Authorizing package operation".to_string() } else { label.to_string() };
    let preset_input = stdin.is_some();
    if let Some(stdin) = stdin { command.stdin(stdin); }
    // Pacman's stage names are observed to avoid competing with its downloads.
    command.env("LC_ALL", "C");
    let mut footer = None;
    let mut input: Box<dyn Read>;
    let read_fd;
    if interactive {
        let mut size = terminal_size();
        // Keep stdin and the controlling terminal inherited. A separate input
        // session would invalidate sudo's per-terminal authentication cache.
        // The child reads prompts directly; the parent only relays output.
        // Bottom-anchored text can push an unanswered tty prompt into
        // scrollback when a window shrinks. Defer reservation until the
        // privileged child produces operation output after authentication.
        let display = if phase == "Authorizing package operation" || phase == "Awaiting authorization" { Footer::pending(manager)? } else { Footer::new(manager, false)? };
        if display.height != 0 { display.draw(&scoped_phase(manager, label, &phase))?; }
        size.ws_row = size.ws_row.saturating_sub(display.height);
        let (master, slave) = output_pty(&size)?;
        // Keep PTY newline processing: sudo can temporarily put the inherited
        // terminal in raw mode, so relayed output must already contain CRLF.
        if !preset_input { command.stdin(Stdio::inherit()); }
        command
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        read_fd = master.as_raw_fd();
        input = Box::new(master);
        footer = Some(display);
    } else {
        let (reader, writer) = UnixStream::pair()?;
        command
            .stdout(Stdio::from(std::os::fd::OwnedFd::from(writer.try_clone()?)))
            .stderr(Stdio::from(std::os::fd::OwnedFd::from(writer)));
        read_fd = reader.as_raw_fd();
        input = Box::new(reader);
    }
    let mut child = ChildGuard(command.spawn()?);
    crate::execution::register_child(child.0.id());
    // Close the parent's copies of the slave/socket so EOF can be observed.
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut observer = Observer::default();
    let mut downloads = NativeDownloads::current();
    transfers::emit(manager, transfers::Window::Started);
    let mut buf = [0u8; 8192];
    loop {
        let mut fd = libc::pollfd {
            fd: read_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut fd, 1, 100) };
        if rc < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        // Repair the scroll region before forwarding bytes produced after a
        // resize. Otherwise those bytes can land on the previous footer rows.
        if let Some(footer) = &mut footer {
            if footer.height != 0 && footer.resize()? {
                let mut size = terminal_size();
                size.ws_row = size.ws_row.saturating_sub(footer.height);
                unsafe { libc::ioctl(read_fd, libc::TIOCSWINSZ, &size); }
            }
        }
        if fd.revents != 0 {
            match input.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    io::stdout().write_all(&buf[..n])?;
                    io::stdout().flush()?;
                    // A read can contain a heading followed by many hook/log
                    // lines. Observe every phase before those lines replace
                    // the latest display text.
                    observer.feed_observed(&buf[..n], &mut on_ready, &mut |line| {
                        phase = downloads.phase(manager, line, &phase);
                        if let Some(window) = transfers::window(manager, line) { transfers::emit(manager, window); }
                        on_line(line);
                    });
                }
                Err(error) if interactive && error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        let display = observer.display();
        if !display.is_empty() { phase = downloads.phase(manager, &display, &phase); }
        // sudo writes authentication prompts on the inherited tty. Leave that
        // cursor alone while authorization awaits echo-free input. Once its
        // child produces package output, keep rendering even if sudo retains
        // raw mode for the rest of its own PTY relay. Never read a password.
        let authorizing = phase == "Authorizing package operation" || phase == "Awaiting authorization";
        if let Some(footer) = &mut footer {
            if authorizing && (footer.height == 0 || footer.authentication_input()) {
                footer.suspend()?;
            } else {
            if footer.resize()? {
                let mut size = terminal_size();
                size.ws_row = size.ws_row.saturating_sub(footer.height);
                unsafe {
                    libc::ioctl(read_fd, libc::TIOCSWINSZ, &size);
                }
            }
            footer.draw(&scoped_phase(manager, label, &phase))?;
            }
        }
        if rc == 0 && child.0.try_wait()?.is_some() {
            break;
        }
    }
    let status = child.0.wait()?;
    if observer.interrupted && matches!(manager, Manager::Native | Manager::Aur | Manager::Sources | Manager::Orphans | Manager::Cache | Manager::Refresh | Manager::Record) {
        crate::execution::interrupt();
    }
    ensure_not_terminating()?;
    if !observer.line.is_empty() {
        on_line(&String::from_utf8_lossy(&observer.line));
    }
    crate::pacman_config::invalidate();
    Ok(status)
}

/// Background downloads cannot own the input terminal or emit foreground
/// output while a selector is active. Keep their status in the shared footer.
pub struct BackgroundCommand {
    child: ChildGuard,
    reader: Option<std::thread::JoinHandle<()>>,
}
impl BackgroundCommand {
    pub fn wait(mut self) -> io::Result<ExitStatus> {
        let result = self.wait_with_progress();
        if result.is_err() { let _ = self.child.0.kill(); let _ = self.child.0.wait(); }
        if let Some(reader) = self.reader.take() { let _ = reader.join(); }
        *NATIVE_BACKGROUND.lock().unwrap_or_else(|e| e.into_inner()) = None;
        result
    }

    fn wait_with_progress(&mut self) -> io::Result<ExitStatus> {
        ensure_not_terminating()?;
        if let Some(status) = self.child.0.try_wait()? {
            ensure_not_terminating()?;
            return Ok(status);
        }
        for line in crate::ui::wrap_field(":: Waiting for repository downloads to finish...", "", "  ", crate::ui::get_terminal_width()) {
            println!("{line}");
        }
        // The selector has returned and released its footer. Keep the same
        // background download visible until its actual exit status is known.
        // Input remains in normal mode so Ctrl-C uses the shared signal path.
        let mut footer = if interactive() {
            Footer::new(Manager::Native, false).ok()
        } else {
            None
        };
        loop {
            if let Some(status) = self.child.0.try_wait()? {
                ensure_not_terminating()?;
                return Ok(status);
            }
            let display_failed = footer.as_mut().is_some_and(|display| {
                let phase = NATIVE_BACKGROUND.lock().unwrap_or_else(|e| e.into_inner()).clone();
                display.resize().and_then(|_| display.draw(phase.as_deref().unwrap_or("Waiting for download result"))).is_err()
            });
            if display_failed {
                // A display failure must not terminate a healthy pacman child
                // or replace its real transaction outcome with a UI error.
                footer = None;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
impl Drop for BackgroundCommand {
    fn drop(&mut self) {
        if self.child.0.try_wait().ok().flatten().is_none() { let _ = self.child.0.kill(); let _ = self.child.0.wait(); }
        if let Some(reader) = self.reader.take() { let _ = reader.join(); }
        *NATIVE_BACKGROUND.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
pub fn start_background(command: &mut Command) -> io::Result<BackgroundCommand> {
    ensure_not_terminating()?;
    let mut reader: Box<dyn Read + Send>;
    if interactive() {
        // Pacman suppresses download bars when stdout is a pipe. Give its
        // background output a private PTY without taking selector input or
        // changing the controlling terminal used for the sudo ticket.
        let mut size = terminal_size();
        size.ws_col = size.ws_col.max(200); // Unambiguous archive labels; never displayed directly.
        let (master, slave) = output_pty(&size)?;
        command.stdout(Stdio::from(slave.try_clone()?)).stderr(Stdio::from(slave));
        reader = Box::new(master);
    } else {
        let (input, writer) = UnixStream::pair()?;
        command.stdout(Stdio::from(std::os::fd::OwnedFd::from(writer.try_clone()?)))
            .stderr(Stdio::from(std::os::fd::OwnedFd::from(writer)));
        reader = Box::new(input);
    }
    // A redirected stdin does not stop sudo from opening /dev/tty. Keep the
    // invoking session for its authentication ticket, but give this download
    // a background process group so the helper cannot relay selector input.
    command.env("LC_ALL", "C").stdin(Stdio::null()).process_group(0);
    let child = ChildGuard(command.spawn()?);
    crate::execution::register_child_group(child.0.id());
    command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    *NATIVE_BACKGROUND.lock().unwrap_or_else(|e| e.into_inner()) = Some("Downloading repository packages".into());
    let mut downloads = NativeDownloads::current();
    let thread = std::thread::spawn(move || {
        let mut observer = Observer::default();
        let mut phase = "Downloading repository packages".to_string();
        let mut buf = [0; 8192];
        while let Ok(size) = reader.read(&mut buf) {
            if size == 0 { break; }
            observer.feed_observed(&buf[..size], &mut || {}, &mut |line| downloads.observe(line));
            phase = downloads.phase(Manager::Native, &observer.display(), &phase);
            *NATIVE_BACKGROUND.lock().unwrap_or_else(|e| e.into_inner()) = Some(phase.clone());
        }
        *NATIVE_BACKGROUND.lock().unwrap_or_else(|e| e.into_inner()) = Some("Download process ended; awaiting status".into());
    });
    Ok(BackgroundCommand { child, reader: Some(thread) })
}
pub fn selection_operation<T>(work: impl FnOnce() -> T) -> T {
    if SELECTION_DISPLAY.with(|d| d.borrow().is_some()) { return work(); }
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { SELECTION_DISPLAY.with(|d| { d.borrow_mut().take(); }); } }
    let _reset = Reset;
    if interactive() {
        SELECTION_DISPLAY.with(|d| { *d.borrow_mut() = Footer::new(Manager::Review, false).ok(); });
    }
    work()
}
pub fn selection_height() -> usize {
    SELECTION_DISPLAY.with(|d| terminal_size().ws_row.saturating_sub(d.borrow().as_ref().map(|f| f.height).unwrap_or(0)) as usize)
}
pub fn draw_selection() {
    SELECTION_DISPLAY.with(|d| {
        if let Some(footer) = d.borrow_mut().as_mut() {
            let _ = footer.resize();
            let _ = footer.draw("Choosing orphan cleanup");
        }
    });
}

pub(crate) fn wait_for_preparation<T>(manager: Manager, handle: &std::thread::JoinHandle<T>) {
    let mut footer = if io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && std::env::var("TERM").is_ok_and(|term| term != "dumb")
    {
        Footer::new(manager, true).ok()
    } else {
        None
    };
    while !handle.is_finished() {
        if let Some(footer) = &mut footer {
            let _ = footer.resize();
            let phase = if manager == Manager::Sources { String::new() } else {
                PREPARING.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(m, _)| *m == manager).map(|(_, text)| text.clone()).unwrap_or_else(|| "Preparing downloads".into())
            };
            let _ = footer.draw(&phase);
            // Raw mode sends Ctrl-C as input rather than a terminal signal.
            // During the wait there is no child PTY to forward it to.
            let mut fd = libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut fd, 1, 0) } > 0 && fd.revents & libc::POLLIN != 0 {
                let mut input = [0u8; 64];
                if let Ok(n) = io::stdin().read(&mut input) {
                    if input[..n].contains(&3) {
                        unsafe {
                            libc::raise(libc::SIGINT);
                        }
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
