//! Own the terminal while a package manager runs, keeping worker output out of
//! its cursor-based progress display. A PTY preserves prompts and native bars.
use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
#[cfg(test)]
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Mutex;

#[derive(Default)]
struct SourceState {
    text: String,
    downloaded: u64,
    total: Option<u64>,
    notices: Vec<String>,
}
static SOURCE: Mutex<SourceState> = Mutex::new(SourceState {
    text: String::new(),
    downloaded: 0,
    total: None,
    notices: Vec::new(),
});
thread_local! { static BACKGROUND: Cell<bool> = const { Cell::new(false) }; }
thread_local! { static SOURCE_DISPLAY: RefCell<Option<Footer>> = const { RefCell::new(None) }; }
static TERMINAL: Mutex<Option<(libc::termios, u16)>> = Mutex::new(None);
static VIEW: Mutex<Option<(u16, u16, u16)>> = Mutex::new(None);
static SESSION_VIEW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Keep one output region across an invocation's successive footer phases.
pub struct SessionView;
pub fn session_view() -> SessionView {
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
static NATIVE_BACKGROUND: Mutex<Option<String>> = Mutex::new(None);
static CONTEXT: Mutex<Option<(Manager, String)>> = Mutex::new(None);
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

#[derive(Default)]
struct NativeDownloads {
    packages: Vec<(String, u64, u8)>,
    ambiguous: bool,
    aggregate: Option<u8>,
    observed: bool,
}
impl NativeDownloads {
    fn current() -> Self {
        Self { packages: NATIVE_DOWNLOADS.with(|d| d.borrow().iter().map(|(name, size)| (name.clone(), *size, 0)).collect()), ..Default::default() }
    }
    fn observe(&mut self, line: &str) {
        let Some(percent) = line_percent(line) else { return; };
        let Some(label) = line.split_whitespace().next() else { return; };
        if label == "Total" || label.starts_with("Total(") { self.aggregate = Some(percent); return; }
        if label.starts_with('(') { return; }
        let matching: Vec<_> = self.packages.iter_mut().filter(|(name, _, _)| {
            label == name.as_str() || label.strip_suffix("...").is_some_and(|prefix| !prefix.is_empty() && name.starts_with(prefix))
        }).collect();
        if matching.len() == 1 {
            matching.into_iter().next().unwrap().2 = percent;
            self.observed = true;
        } else { self.ambiguous = true; }
    }
    fn phase(&mut self, manager: Manager, line: &str, previous: &str) -> String {
        self.observe(line);
        let phase = phase_text(manager, line, previous);
        if manager != Manager::Native || !phase.starts_with("Downloading packages") || line.trim_start().starts_with("Total") { return phase; }
        if let Some(percent) = self.aggregate { return format!("Downloading packages ({percent}%)"); }
        let total: u128 = self.packages.iter().map(|(_, size, _)| u128::from(*size)).sum();
        if total == 0 || self.ambiguous || !self.observed { return phase; }
        let percent = self.packages.iter().map(|(_, size, progress)| u128::from(*size) * u128::from(*progress)).sum::<u128>() / total;
        format!("Downloading packages ({percent}%)")
    }
}

fn line_percent(text: &str) -> Option<u8> {
    text.split_whitespace().find_map(|word| word.strip_suffix('%').and_then(|number| number.parse::<u8>().ok()).filter(|&value| value <= 100))
}

pub fn begin_transaction() {
    COMPLETED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    FAILED.lock().unwrap_or_else(|e| e.into_inner()).clear();
}
pub fn completed(manager: Manager) {
    FAILED.lock().unwrap_or_else(|e| e.into_inner()).retain(|&m| m != manager);
    let mut completed = COMPLETED.lock().unwrap_or_else(|e| e.into_inner());
    if !completed.contains(&manager) { completed.push(manager); }
}

pub fn failed(manager: Manager) {
    COMPLETED.lock().unwrap_or_else(|e| e.into_inner()).retain(|&m| m != manager);
    let mut failed = FAILED.lock().unwrap_or_else(|e| e.into_inner());
    if !failed.contains(&manager) { failed.push(manager); }
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
    crate::sandbox::init_signal_handlers();
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
        let _ = footer.draw(label);
        let label = label.to_owned();
        let (stop, stopped) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            while matches!(stopped.recv_timeout(std::time::Duration::from_millis(100)), Err(std::sync::mpsc::RecvTimeoutError::Timeout)) {
                if footer.resize().and_then(|_| footer.draw(&label)).is_err() { break; }
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
}
impl Manager {
    fn label(self) -> &'static str {
        match self {
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
            _ => "Upgrade complete",
        }
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
            .strip_prefix(":: [Background] ")
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

pub fn source_warning(text: String) {
    if BACKGROUND.get() {
        let mut state = SOURCE.lock().unwrap_or_else(|e| e.into_inner());
        state.text = "Prefetch failed; will retry during source installation".into();
        state.downloaded = 0;
        state.total = None;
        state.notices.push(text);
    } else {
        eprintln!("{text}");
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

fn download_text(name: &str, downloaded: u64, total: Option<u64>) -> String {
    let mib = downloaded as f64 / 1_048_576.0;
    match total.filter(|&n| n != 0) {
        Some(total) => {
            let percent = (downloaded.saturating_mul(100) / total).min(100);
            let filled = (percent / 10) as usize;
            format!(
                "{name} [{}{}] {mib:.1}/{:.1} MiB ({percent}%)",
                "=".repeat(filled),
                " ".repeat(10 - filled),
                total as f64 / 1_048_576.0
            )
        }
        None => format!("{name} [{mib:.1} MiB]"),
    }
}

fn footer_download(name: &str, downloaded: u64, total: Option<u64>, width: usize) -> String {
    let mut suffix = download_text("", downloaded, total);
    if crate::ui::str_width(&suffix) > width {
        if let Some(total) = total.filter(|&n| n != 0) {
            let percent = (downloaded.saturating_mul(100) / total).min(100);
            suffix = format!(
                " {:.0}/{:.0}MiB {percent}%",
                downloaded as f64 / 1_048_576.0,
                total as f64 / 1_048_576.0
            );
        }
    }
    let name_width = width.saturating_sub(crate::ui::str_width(&suffix));
    format!(
        "{}{}",
        crate::ui::truncate_str(&clean_text(name), name_width),
        suffix
    )
}

fn native_footer(text: &str, width: usize) -> String {
    let clean = clean_text(text);
    if let Some((phase, metrics)) = clean.split_once(" (").filter(|(_, metrics)| metrics.ends_with(')') && metrics.contains('%')) {
        let suffix = format!(" ({metrics}");
        if crate::ui::str_width(&suffix) + 2 <= width {
            return format!("{}{suffix}", crate::ui::truncate_str(phase, width - crate::ui::str_width(&suffix)));
        }
        if let Some(percent) = metrics.split(',').next().filter(|percent| percent.trim_end_matches(')').ends_with('%')) {
            let suffix = format!(" ({})", percent.trim_end_matches(')'));
            return format!("{}{suffix}", crate::ui::truncate_str(phase, width.saturating_sub(crate::ui::str_width(&suffix))));
        }
    }
    let words: Vec<_> = clean
        .split_whitespace()
        .filter(|word| {
            // Pacman's long ASCII bar is already visible in the normal text area.
            !(word.starts_with('[')
                && word.ends_with(']')
                && word[1..word.len() - 1]
                    .chars()
                    .all(|c| matches!(c, '#' | '-' | '=' | '>')))
        })
        .collect();
    if let Some(unit) = words
        .iter()
        .position(|word| matches!(*word, "B" | "KiB" | "MiB" | "GiB"))
        .filter(|&unit| unit > 0)
    {
        let suffix = words[unit - 1..].join(" ");
        let name = words[..unit - 1].join(" ");
        let name_width = width.saturating_sub(crate::ui::str_width(&suffix) + 1);
        return format!("{} {suffix}", crate::ui::truncate_str(&name, name_width));
    }
    crate::ui::truncate_str(&words.join(" "), width)
}

fn phase_text(manager: Manager, text: &str, previous: &str) -> String {
    if manager == Manager::Hooks { return previous.into(); }
    let clean = clean_text(text);
    let lower = clean.to_lowercase();
    if lower.contains("waiting for repository downloads") { return "Queued; waiting for repository downloads".into(); }
    if lower.contains("prefetch failed") { return "Prefetch failed; retry pending".into(); }
    if lower.contains("enter a number") || lower.contains("enter a selection") { return "Awaiting package selection".into(); }
    if lower.contains("password") || lower.contains("authentication") { return "Awaiting authorization".into(); }
    if lower.contains("[y/n]") || lower.contains("[y/n") || lower.contains("proceed with") { return "Awaiting confirmation".into(); }
    if lower.starts_with("error:") || lower.contains("failed") { return "Operation failed".into(); }
    let stage = |phase: &str| phase_with_metrics(manager, &clean, phase);
    if manager == Manager::Refresh { return stage("Refreshing repositories"); }
    if manager == Manager::Record { return previous.into(); }
    if manager == Manager::Orphans {
        if lower.contains("checking dependencies") { return "Checking dependencies".into(); }
        if lower.contains("removing") || lower.contains("processing package changes") { return stage("Removing packages"); }
        if lower.contains("hooks") { return "Running removal hooks".into(); }
        return previous.into();
    }
    if manager == Manager::Cache {
        if lower.contains("unused repositories") { return "Cleaning repository metadata".into(); }
        if lower.contains("download") && lower.contains("director") { return "Clearing download leftovers".into(); }
        if lower.contains("removing all files") || lower.contains("cleaning") { return "Cleaning package cache".into(); }
        return previous.into();
    }
    if lower.contains("dkms") { return stage("Building kernel modules"); }
    if lower.contains("post-transaction") || lower.contains("hooks") { return stage("Running post-install hooks"); }
    if lower.contains("resolving dependencies") || lower.contains("conflicting packages") { return stage("Resolving dependencies"); }
    if lower.contains("checking") || lower.contains("verifying") { return stage("Verifying packages"); }
    if lower.contains("completed download") || lower.contains("prefetched artifact") { return "Download verified; ready to package".into(); }
    if lower.contains("fetching") || lower.contains("prefetching") || lower.contains("downloading") || lower.contains("retrieving packages") { return stage("Downloading packages"); }
    if lower.contains("creating package") || lower.contains("compressing package") || lower.contains("packaging") { return "Packaging source".into(); }
    if lower.contains("building") || lower.contains("makepkg") { return stage("Building packages"); }
    if lower.contains("upgrading") || lower.contains("installing") || lower.contains("processing package changes") { return stage("Installing packages"); }
    if matches!(manager, Manager::Native | Manager::Aur) && clean.contains('%') {
        return stage(previous.split(" (").next().unwrap_or(previous));
    }
    previous.into()
}

fn phase_with_metrics(manager: Manager, text: &str, phase: &str) -> String {
    if !matches!(manager, Manager::Native | Manager::Aur | Manager::Orphans) { return phase.into(); }
    if let Some(counter) = text.trim_start().strip_prefix('(').and_then(|line| line.split_once(')').map(|(counter, _)| counter.trim())) {
        if let Some((done, total)) = counter.split_once('/').and_then(|(done, total)| Some((done.trim().parse::<u64>().ok()?, total.trim().parse::<u64>().ok()?))).filter(|(_, total)| *total != 0) {
            let done = done.min(total);
            let progress = line_percent(text).map(|percent| u128::from(done.saturating_sub(1)) * 100 + u128::from(percent)).unwrap_or(u128::from(done) * 100);
            // ALPM's keyring/integrity/loading/conflict/space bars already
            // describe the full verification action. Package installation
            // and removal bars describe the current package instead.
            let percent = if phase == "Verifying packages" {
                line_percent(text).map(u128::from).unwrap_or(u128::from(done) * 100 / u128::from(total))
            } else { (progress / u128::from(total)).min(100) };
            return format!("{phase} ({percent}%, {done}/{total})");
        }
    }
    // Individual download bars are not a transaction percentage. Prefer
    // pacman's aggregate Total bar; planned downloads are weighted separately.
    if text.trim_start().strip_prefix("Total").is_some_and(|rest| rest.starts_with(char::is_whitespace) || rest.starts_with('(')) {
        if let Some(percent) = line_percent(text) { return format!("{phase} ({percent}%)"); }
    }
    phase.into()
}

fn scoped_phase(manager: Manager, label: &str, phase: &str) -> String {
    if manager == Manager::Cache {
        let scope = if label.contains("native") { "Native" } else if label.contains("AUR") { "AUR" } else { return phase.into(); };
        return format!("{scope}: {phase}");
    }
    phase.into()
}

pub fn queued_sources() {
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
    for notice in state.notices.drain(..) {
        eprintln!("{notice}");
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
        let bottom = self.rows.saturating_sub(self.height).max(1);
        let mut out = io::stdout().lock();
        write!(out, "\x1b7\x1b[{};1H\x1b[J\x1b[r\x1b8", bottom + 1)?;
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
        if self.height != 0 && self.rows == size.ws_row && self.cols == size.ws_col {
            let old_bottom = self.rows.saturating_sub(self.height).max(1);
            write!(out, "\x1b7\x1b[{};1H\x1b[J\x1b8", old_bottom + 1)?;
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
        let mut history: Vec<String> = COMPLETED.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|&&m| m != self.manager)
            .map(|manager| format!("{}: ✔ {}", manager.label(), manager.completion())).collect();
        history.extend(FAILED.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|&&m| m != self.manager)
            .map(|manager| format!("{}: ✗ Operation failed", manager.label())));
        let mut active = vec![format!("{label}: {current}")];
        if let Some((manager, phase)) = CONTEXT.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if *manager != self.manager { active.push(format!("{}: {phase}", manager.label())); }
        }
        if self.manager != Manager::Native {
            if let Some(phase) = NATIVE_BACKGROUND.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                active.push(format!("Pacman: {phase}"));
            }
        }
        if matches!(self.manager, Manager::Native | Manager::Aur) && !state.text.is_empty() {
            active.push(format!("Sources: {source}"));
        }
        fit_rows(history, active, rows.saturating_sub(5) as usize).into_iter()
            .map(|line| crate::ui::truncate_str(&clean_text(&line), width)).collect()
    }

    fn draw(&self, native: &str) -> io::Result<()> {
        let row = self.rows.saturating_sub(self.height).saturating_add(1);
        let lines = self.lines(native, self.rows, self.cols);
        let mut out = io::stdout().lock();
        // DEC save/restore does not change the package manager's output cursor.
        write!(out, "\x1b7\x1b[{row};1H\x1b[0m\x1b[2K{}", "─".repeat(self.cols.saturating_sub(1) as usize))?;
        for (index, line) in lines.iter().enumerate() {
            write!(out, "\x1b[{};1H\x1b[2K{line}", row + 1 + index as u16)?;
        }
        write!(out, "\x1b8")?;
        out.flush()
    }
}

fn fit_rows(history: Vec<String>, active: Vec<String>, capacity: usize) -> Vec<String> {
    let capacity = capacity.max(active.len());
    let keep = capacity.saturating_sub(active.len());
    let start = history.len().saturating_sub(keep);
    history.into_iter().skip(start).chain(active).collect()
}
impl Drop for Footer {
    fn drop(&mut self) {
        if self.height != 0 {
            let bottom = self.rows.saturating_sub(self.height).max(1);
            let reset = if SESSION_VIEW.load(std::sync::atomic::Ordering::Relaxed) { "" } else { "\x1b[r" };
            let _ = write!(io::stdout(), "\x1b7\x1b[{};1H\x1b[J{reset}\x1b8", bottom + 1);
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

#[derive(Default)]
struct Observer {
    line: Vec<u8>,
    escape: u8,
    ready: bool,
    latest: String,
    interrupted: bool,
}
impl Observer {
    fn feed(&mut self, bytes: &[u8], on_ready: &mut impl FnMut()) {
        self.feed_observed(bytes, on_ready, &mut |_| {});
    }
    fn feed_observed(&mut self, bytes: &[u8], on_ready: &mut impl FnMut(), on_line: &mut impl FnMut(&str)) {
        for &byte in bytes {
            // Consume CSI and OSC sequences even when split across reads.
            match self.escape {
                1 => {
                    self.escape = match byte {
                        b'[' => 2,
                        b']' => 3,
                        _ => 0,
                    };
                    continue;
                }
                2 => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.escape = 0;
                    }
                    continue;
                }
                3 => {
                    if byte == 7 {
                        self.escape = 0;
                    } else if byte == 27 {
                        self.escape = 4;
                    }
                    continue;
                }
                4 => {
                    self.escape = if byte == b'\\' { 0 } else { 3 };
                    continue;
                }
                _ => {}
            }
            match byte {
                27 => self.escape = 1,
                b'\r' | b'\n' => {
                    let line = String::from_utf8_lossy(&self.line).trim().to_owned();
                    if !line.is_empty() {
                        self.interrupted |= line == "Interrupt signal received";
                        on_line(&line);
                        if !self.ready && downloads_finished(&line) {
                            self.ready = true;
                            on_ready();
                        }
                        self.latest = line;
                    }
                    self.line.clear();
                }
                byte if byte >= 32 && byte != 127 => {
                    if self.line.len() < 8192 {
                        self.line.push(byte);
                    }
                    // A prompt/progress update need not end with a newline.
                    if !self.ready && downloads_finished(&String::from_utf8_lossy(&self.line)) {
                        self.ready = true;
                        on_ready();
                    }
                }
                _ => {}
            }
        }
    }
    fn display(&self) -> String {
        if self.line.is_empty() {
            self.latest.clone()
        } else {
            String::from_utf8_lossy(&self.line).into_owned()
        }
    }
}

fn downloads_finished(line: &str) -> bool {
    [
        "checking keys in keyring",
        "checking keyring",
        "checking package integrity",
        "checking for file conflicts",
        "Processing package changes",
        "there is nothing to do",
    ]
    .iter()
    .any(|marker| line.contains(marker))
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
fn clean_text(text: &str) -> String {
    let mut observer = Observer::default();
    observer.feed(text.as_bytes(), &mut || {});
    observer.display()
}

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
        crate::sandbox::unregister_child(self.0.id());
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
    crate::sandbox::init_signal_handlers();
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
    crate::sandbox::register_child(child.0.id());
    // Close the parent's copies of the slave/socket so EOF can be observed.
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut observer = Observer::default();
    let mut downloads = NativeDownloads::current();
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
        if fd.revents != 0 {
            match input.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    io::stdout().write_all(&buf[..n])?;
                    io::stdout().flush()?;
                    // A read can contain a heading followed by many hook/log
                    // lines. Observe every phase before those lines replace
                    // the latest display text.
                    observer.feed_observed(&buf[..n], &mut on_ready, &mut |line| { phase = downloads.phase(manager, line, &phase); on_line(line); });
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
        crate::sandbox::interrupt();
    }
    ensure_not_terminating()?;
    if !observer.line.is_empty() {
        on_line(&String::from_utf8_lossy(&observer.line));
    }
    Ok(status)
}

fn ensure_not_terminating() -> io::Result<()> {
    // A manager may finish successfully while SIGINT interrupts its hook.
    // The signal handler is still cleaning up; do not race it by accepting
    // that status or starting another operation.
    if crate::sandbox::is_terminating() {
        crate::sandbox::interrupt()
    } else { Ok(()) }
}

/// Bounded read-only tool capture with the same child tracking as foreground
/// operations. Drain both pipes concurrently even after the capture limit.
pub fn capture_output(command: &mut Command) -> io::Result<std::process::Output> {
    ensure_not_terminating()?;
    fn read_bounded(mut stream: impl Read) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let count = stream.read(&mut buffer)?;
            if count == 0 { return Ok(bytes); }
            let keep = count.min(32_768usize.saturating_sub(bytes.len()));
            bytes.extend_from_slice(&buffer[..keep]);
        }
    }
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = ChildGuard(command.spawn()?);
    crate::sandbox::register_child(child.0.id());
    let stdout = child.0.stdout.take().ok_or_else(|| io::Error::other("Missing stdout pipe"))?;
    let stderr = child.0.stderr.take().ok_or_else(|| io::Error::other("Missing stderr pipe"))?;
    let output = std::thread::spawn(move || read_bounded(stdout));
    let errors = std::thread::spawn(move || read_bounded(stderr));
    let status = child.0.wait();
    if status.is_err() { let _ = child.0.kill(); let _ = child.0.wait(); }
    let stdout = output.join().map_err(|_| io::Error::other("Output reader failed"))??;
    let stderr = errors.join().map_err(|_| io::Error::other("Error reader failed"))??;
    ensure_not_terminating()?;
    Ok(std::process::Output { status: status?, stdout, stderr })
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
                let phase = NATIVE_BACKGROUND.lock().unwrap_or_else(|e| e.into_inner());
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
    command.env("LC_ALL", "C").stdin(Stdio::null());
    let child = ChildGuard(command.spawn()?);
    crate::sandbox::register_child(child.0.id());
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

pub fn wait_for_sources(handle: &std::thread::JoinHandle<()>) {
    let mut footer = if io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && std::env::var("TERM").is_ok_and(|term| term != "dumb")
    {
        Footer::new(Manager::Sources, true).ok()
    } else {
        None
    };
    while !handle.is_finished() {
        if let Some(footer) = &mut footer {
            let _ = footer.resize();
            let _ = footer.draw("");
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

#[cfg(test)]
mod tests {
    use super::*;
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
        let _guard = crate::sandbox::CHILD_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    fn bounded_capture_drains_both_streams_and_preserves_failure() {
        let _guard = crate::sandbox::CHILD_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let output = capture_output(Command::new("/bin/sh").args(["-c", "head -c 65536 /dev/zero; head -c 65536 /dev/zero >&2; exit 7"])).unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout.len(), 32768); assert_eq!(output.stderr.len(), 32768);
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
        let _serial = crate::sandbox::CHILD_TEST_LOCK.lock().unwrap();
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
        let _serial = crate::sandbox::CHILD_TEST_LOCK.lock().unwrap();
        let child = start_background(Command::new("sh").args([
            "-c", "printf 'Downloading fixture 25%%\\r'; sleep 0.2; exit 7",
        ])).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(7));
        assert!(NATIVE_BACKGROUND.lock().unwrap().is_none());
        assert!(TERMINAL.lock().unwrap().is_none());
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
}
