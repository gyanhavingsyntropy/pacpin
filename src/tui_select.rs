//! Inline selectors leave the complete inventory in terminal scrollback.
use crossterm::{cursor::{Hide, MoveTo, Show}, event::{self, Event, KeyCode, KeyEventKind, KeyModifiers}, execute, queue, style::Print, terminal::{Clear, ClearType}};
use std::io::{self, IsTerminal, Write};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct CheckboxItem {
    pub id: String,
    pub label: String,
    pub description: String,
    pub is_pure: bool,
    pub checked: bool,
}
impl CheckboxItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>, description: impl Into<String>, is_pure: bool, checked: bool) -> Self {
        Self { id: id.into(), label: label.into(), description: description.into(), is_pure, checked }
    }
}

struct TerminalGuard(libc::termios, bool);
impl TerminalGuard {
    fn new() -> io::Result<Self> {
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(0, &mut saved) } != 0 { return Err(io::Error::last_os_error()); }
        let mut raw = saved;
        unsafe { libc::cfmakeraw(&mut raw); }
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } != 0 { return Err(io::Error::last_os_error()); }
        Ok(Self(saved, crate::progress::register_input_restore(saved)))
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &self.0); }
        if self.1 { crate::progress::unregister_input_restore(); }
        let _ = execute!(io::stdout(), Show, Print("\r\n"));
    }
}

const FIXED_ROWS: usize = 6;
fn menu_layout(height: usize, count: usize) -> (usize, u16) {
    let visible = count.min(height.saturating_sub(FIXED_ROWS + 1)).max(1);
    (visible, height.saturating_sub(FIXED_ROWS + visible + 1) as u16)
}

pub fn run_checkbox_menu(title: &str, subtitle: &str, initial: &[CheckboxItem]) -> Option<Vec<String>> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() || initial.is_empty() { return None; }
    // The viewport cannot show more rows than the physical terminal. Keep all
    // names/descriptions accessible in scrollback before enabling raw input.
    println!("\n{title} — complete list ({} items):", initial.len());
    let width = crate::ui::get_terminal_width();
    for (index, item) in initial.iter().enumerate() {
        for line in crate::ui::wrap_field(&format!("{}) {} — {}", index + 1, item.label, item.description), "  ", "    ", width) { println!("{line}"); }
    }
    println!("{subtitle}");
    if crate::progress::selection_height() < FIXED_ROWS + 2 {
        print!("Select numbers ('all', 'none'; Enter keeps checked defaults): ");
        io::stdout().flush().ok()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok()?;
        if input.trim().is_empty() { return Some(initial.iter().filter(|i| i.checked).map(|i| i.id.clone()).collect()); }
        let ids: Vec<_> = initial.iter().map(|i| i.id.clone()).collect();
        return Some(crate::ui::parse_selection_indices(input.trim(), &ids));
    }
    let _guard = TerminalGuard::new().ok()?;
    let mut out = io::stdout();
    execute!(out, Hide).ok()?;
    let mut items = initial.to_vec();
    let (mut selected, mut offset) = (0usize, 0usize);
    let mut query = String::new();
    let mut searching = false;
    let (visible, _) = menu_layout(crate::progress::selection_height(), items.len());
    for _ in 0..FIXED_ROWS + visible { queue!(out, Print("\r\n")).ok()?; }
    loop {
        let matches: Vec<_> = (0..items.len()).filter(|&i| query.is_empty() || items[i].label.to_lowercase().contains(&query.to_lowercase()) || items[i].description.to_lowercase().contains(&query.to_lowercase())).collect();
        selected = selected.min(matches.len().saturating_sub(1));
        let height = crate::progress::selection_height();
        let (visible, row) = menu_layout(height, items.len());
        offset = offset.min(selected);
        if selected >= offset + visible { offset = selected + 1 - visible; }
        for row in row as usize..height.saturating_sub(1) { queue!(out, MoveTo(0, row as u16), Clear(ClearType::CurrentLine)).ok()?; }
        queue!(out, MoveTo(0, row)).ok()?;
        render_checkbox_ui(&mut out, title, subtitle, &items, &matches, selected, offset, visible, crate::ui::get_terminal_width().saturating_sub(1), searching, &query);
        out.flush().ok()?;
        crate::progress::draw_selection();
        if !event::poll(Duration::from_millis(100)).ok()? { continue; }
        match event::read().ok()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    unsafe { libc::raise(libc::SIGINT); }
                    // The registered signal handler restores input and stops
                    // tracked downloads. Do not start installation while it runs.
                    loop { std::thread::park(); }
                }
                // Enter always confirms the selection, even with a search
                // filter. Never silently consume the first Enter to lock it.
                if key.code == KeyCode::Enter { return Some(items.iter().filter(|i| i.checked).map(|i| i.id.clone()).collect()); }
                if searching {
                    match key.code {
                        KeyCode::Esc => { searching = false; query.clear(); }
                        KeyCode::Backspace => { query.pop(); }
                        KeyCode::Char(c) if query.len() < 128 => { query.push(c); }
                        _ => {}
                    }
                    selected = 0; offset = 0;
                    continue;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(matches.len().saturating_sub(1)),
                    KeyCode::PageUp => selected = selected.saturating_sub(visible),
                    KeyCode::PageDown => selected = (selected + visible).min(matches.len().saturating_sub(1)),
                    KeyCode::Home => selected = 0,
                    KeyCode::End => selected = matches.len().saturating_sub(1),
                    KeyCode::Char(' ') => { if let Some(&i) = matches.get(selected) { items[i].checked = !items[i].checked; } }
                    KeyCode::Char('a' | 'A' | 'n' | 'N' | 'p' | 'P' | 'i' | 'I') => {
                        for &i in &matches { items[i].checked = match key.code { KeyCode::Char('a' | 'A') => true, KeyCode::Char('n' | 'N') => false, KeyCode::Char('p' | 'P') => items[i].is_pure, _ => !items[i].checked }; }
                    }
                    KeyCode::Char('/') => searching = true,
                    KeyCode::Esc | KeyCode::Char('q' | 'Q') => return None,
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_checkbox_ui<W: Write>(out: &mut W, title: &str, _subtitle: &str, items: &[CheckboxItem], matches: &[usize], selected: usize, offset: usize, visible: usize, width: usize, searching: bool, query: &str) {
    let is_orphan = title.to_lowercase().contains("orphan");
    let mut line = |text: String| { let _ = write!(out, "{}\r\n", crate::ui::truncate_str(&text, width)); };
    line(title.into());
    if width < 40 {
        line("Move ↑↓ PgUp/Dn Home/End".into());
        line("Space; a/n/p/i; / find".into());
    } else if width < 60 {
        line("Space toggles; ↑↓ PgUp/Dn Home/End".into());
        line("a all n none p pure i invert / find".into());
    } else {
        line("Space toggle • ↑↓ move • PgUp/PgDn • Home/End".into());
        line("a all • n none • p pure • i invert • / search".into());
    }
    let end = matches.len().min(offset + visible);
    for pos in offset..offset + visible {
        let text = matches.get(pos).filter(|_| pos < end).map(|&i| format!("{} [{}] {}. {} {}", if pos == selected { "▶" } else { " " }, if items[i].checked { "x" } else { " " }, i + 1, items[i].label, if !is_orphan { "" } else if items[i].is_pure { "[pure]" } else { "[opt]" })).unwrap_or_default();
        line(text);
    }
    line(format!("Showing {}–{end}/{} • Selected {}/{}", if matches.is_empty() { 0 } else { offset + 1 }, matches.len(), items.iter().filter(|i| i.checked).count(), items.len()));
    line(if searching { format!("Enter confirms • Search: {query}") } else if !query.is_empty() { format!("Enter confirms • Filter: {query}") } else { "Enter confirms • Esc/q skips selection".into() });
    let detail = matches.get(selected).map(|&i| format!("{} — {}", items[i].label, items[i].description)).unwrap_or_else(|| "No matching packages".into());
    let _ = write!(out, "{}", crate::ui::truncate_str(&detail, width));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn viewport_uses_available_rows_and_reaches_last_item() {
        assert_eq!(menu_layout(24, 50), (17, 0));
        assert_eq!(menu_layout(40, 50), (33, 0));
        assert_eq!(menu_layout(24, 2), (2, 15));
    }
    #[test]
    fn selector_lines_fit_narrow_terminals_and_empty_search() {
        let items = vec![CheckboxItem::new("package", "unicode-🚀-long-package-name", "Optional for installed apps", false, true)];
        for width in [24, 32, 40, 72, 100] {
            for matches in [vec![0], vec![]] {
                let mut out = Vec::new();
                render_checkbox_ui(&mut out, "SELECT ORPHANED PACKAGES TO REMOVE", "subtitle", &items, &matches, 0, 0, 2, width, false, "missing");
                let text = String::from_utf8(out).unwrap();
                assert!(text.lines().all(|line| crate::ui::str_width(line) <= width));
                assert!(text.contains("Enter confirms"));
                assert!(text.contains("Home/End"));
                assert!(text.contains("/ find") || text.contains("/ search"));
            }
        }
    }
}
