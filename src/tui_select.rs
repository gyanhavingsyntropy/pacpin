// pacpin - Declarative Package Resolver & Upgrade Engine for Arch Linux / CachyOS
// Copyright (C) 2026 Gyan <330976822+gyanhavingsyntropy@users.noreply.github.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use colored::Colorize;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute, queue,
    style::Print,
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use std::io::{self, stdout, IsTerminal, Write};

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
        Self {
            id: id.into(),
            label: label.into(),
            description: description.into(),
            is_pure,
            checked,
        }
    }
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut out = stdout();
        let _ = execute!(out, LeaveAlternateScreen, Show);
    }
}

pub fn run_checkbox_menu(
    title: &str,
    subtitle: &str,
    initial_items: &[CheckboxItem],
) -> Option<Vec<String>> {
    if !io::stdin().is_terminal() || initial_items.is_empty() {
        return None;
    }

    let mut items: Vec<CheckboxItem> = initial_items.to_vec();
    let mut selected: usize = 0;
    let mut viewport_offset: usize = 0;
    let mut search_mode = false;
    let mut search_query = String::new();

    // Setup terminal
    if enable_raw_mode().is_err() {
        return None;
    }
    let mut out = stdout();
    if execute!(out, EnterAlternateScreen, Hide).is_err() {
        let _ = disable_raw_mode();
        return None;
    }
    let _guard = TerminalGuard;

    loop {
        // Filter items if searching
        let matching_indices: Vec<usize> = if search_query.is_empty() {
            (0..items.len()).collect()
        } else {
            let q = search_query.to_lowercase();
            (0..items.len())
                .filter(|&idx| {
                    items[idx].label.to_lowercase().contains(&q)
                        || items[idx].description.to_lowercase().contains(&q)
                })
                .collect()
        };

        if selected >= matching_indices.len() && !matching_indices.is_empty() {
            selected = matching_indices.len() - 1;
        }

        // Terminal dimensions
        let (term_width, term_height) = terminal_size::terminal_size()
            .map(|(w, h)| (w.0 as usize, h.0 as usize))
            .unwrap_or((80, 24));
        let box_width = term_width.min(88).max(64);
        let max_visible = (term_height.saturating_sub(13)).min(18).max(5);

        // Adjust viewport offset
        if selected < viewport_offset {
            viewport_offset = selected;
        } else if selected >= viewport_offset + max_visible {
            viewport_offset = selected - max_visible + 1;
        }

        // Render UI
        let _ = queue!(out, MoveTo(0, 0), Clear(ClearType::All));

        render_checkbox_ui(
            &mut out,
            title,
            subtitle,
            &items,
            &matching_indices,
            selected,
            viewport_offset,
            max_visible,
            box_width,
            search_mode,
            &search_query,
        );
        let _ = out.flush();

        // Read event
        if let Ok(Event::Key(KeyEvent {
            code, modifiers, ..
        })) = event::read()
        {
            if search_mode {
                match code {
                    KeyCode::Enter => {
                        search_mode = false;
                    }
                    KeyCode::Esc => {
                        search_mode = false;
                        search_query.clear();
                        selected = 0;
                        viewport_offset = 0;
                    }
                    KeyCode::Backspace => {
                        search_query.pop();
                        selected = 0;
                        viewport_offset = 0;
                    }
                    KeyCode::Char(c) => {
                        if search_query.len() < 32 {
                            search_query.push(c);
                            selected = 0;
                            viewport_offset = 0;
                        }
                    }
                    _ => {}
                }
            } else {
                match code {
                    // Navigation Up
                    KeyCode::Up | KeyCode::Char('k') => {
                        if selected > 0 {
                            selected -= 1;
                        }
                    }
                    // Navigation Down
                    KeyCode::Down | KeyCode::Char('j') => {
                        if !matching_indices.is_empty() && selected + 1 < matching_indices.len() {
                            selected += 1;
                        }
                    }
                    // Page Up
                    KeyCode::PageUp => {
                        selected = selected.saturating_sub(max_visible);
                    }
                    // Page Down
                    KeyCode::PageDown => {
                        if !matching_indices.is_empty() {
                            selected = (selected + max_visible).min(matching_indices.len() - 1);
                        }
                    }
                    // Home
                    KeyCode::Home => {
                        selected = 0;
                    }
                    // End
                    KeyCode::End => {
                        if !matching_indices.is_empty() {
                            selected = matching_indices.len() - 1;
                        }
                    }
                    // Toggle Space
                    KeyCode::Char(' ') => {
                        if let Some(&real_idx) = matching_indices.get(selected) {
                            items[real_idx].checked = !items[real_idx].checked;
                        }
                    }
                    // Select All: 'a'
                    KeyCode::Char('a') | KeyCode::Char('A') => {
                        for &idx in &matching_indices {
                            items[idx].checked = true;
                        }
                    }
                    // Select None: 'n'
                    KeyCode::Char('n') | KeyCode::Char('N') => {
                        for &idx in &matching_indices {
                            items[idx].checked = false;
                        }
                    }
                    // Select Pure/Safe Only: 'p'
                    KeyCode::Char('p') | KeyCode::Char('P') => {
                        for &idx in &matching_indices {
                            items[idx].checked = items[idx].is_pure;
                        }
                    }
                    // Invert Selection: 'i'
                    KeyCode::Char('i') | KeyCode::Char('I') => {
                        for &idx in &matching_indices {
                            items[idx].checked = !items[idx].checked;
                        }
                    }
                    // Search / Filter: '/'
                    KeyCode::Char('/') => {
                        search_mode = true;
                    }
                    // Confirm: Enter
                    KeyCode::Enter => {
                        let selected_ids: Vec<String> = items
                            .iter()
                            .filter(|item| item.checked)
                            .map(|item| item.id.clone())
                            .collect();
                        return Some(selected_ids);
                    }
                    // Cancel: Esc or 'q'
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                        return None;
                    }
                    _ => {
                        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
                            return None;
                        }
                    }
                }
            }
        }
    }
}

fn render_checkbox_ui<W: Write>(
    out: &mut W,
    title: &str,
    subtitle: &str,
    items: &[CheckboxItem],
    matching_indices: &[usize],
    selected: usize,
    viewport_offset: usize,
    max_visible: usize,
    box_width: usize,
    search_mode: bool,
    search_query: &str,
) {
    let inner_width = box_width.saturating_sub(2);
    let border_color = |s: &str| s.cyan().bold();

    // Top border
    let top = format!("┌{}┐\r\n", "─".repeat(inner_width));
    let _ = queue!(out, Print(border_color(&top)));

    // Header Title
    let title_pad = inner_width.saturating_sub(title.len()) / 2;
    let title_line = format!(
        "│{}{}{}│\r\n",
        " ".repeat(title_pad),
        title.bold().yellow(),
        " ".repeat(inner_width.saturating_sub(title_pad + title.len()))
    );
    let _ = queue!(out, Print(title_line));

    // Subtitle
    let sub_pad = inner_width.saturating_sub(subtitle.len()) / 2;
    let sub_line = format!(
        "│{}{}{}│\r\n",
        " ".repeat(sub_pad),
        subtitle.dimmed(),
        " ".repeat(inner_width.saturating_sub(sub_pad + subtitle.len()))
    );
    let _ = queue!(out, Print(sub_line));

    // Divider
    let div = format!("├{}┤\r\n", "─".repeat(inner_width));
    let _ = queue!(out, Print(border_color(&div)));

    // Shortcuts Bar
    let bar1 = " [Space] Toggle    [a] All     [n] None    [p] Pure Only  [i] Invert";
    let bar1_pad = inner_width.saturating_sub(bar1.len());
    let _ = queue!(
        out,
        Print(format!("│{}{}{}│\r\n", bar1.cyan(), " ".repeat(bar1_pad), ""))
    );

    let bar2 = " [↑/↓] Navigate    [PgUp/Dn] Scroll        [/] Search     [Enter] Confirm";
    let bar2_pad = inner_width.saturating_sub(bar2.len());
    let _ = queue!(
        out,
        Print(format!("│{}{}{}│\r\n", bar2.dimmed(), " ".repeat(bar2_pad), ""))
    );

    let _ = queue!(out, Print(border_color(&div)));

    // Top Scroll Indicator
    if viewport_offset > 0 {
        let more_top = format!("   ▲ {} more items above...", viewport_offset);
        let more_pad = inner_width.saturating_sub(more_top.len());
        let _ = queue!(
            out,
            Print(format!("│{}{}{}│\r\n", more_top.yellow().bold(), " ".repeat(more_pad), ""))
        );
    } else {
        let _ = queue!(out, Print(format!("│{}│\r\n", " ".repeat(inner_width))));
    }

    // Visible Items
    let visible_indices = &matching_indices[viewport_offset..matching_indices.len().min(viewport_offset + max_visible)];
    for (rel_i, &real_idx) in visible_indices.iter().enumerate() {
        let is_cursor = (viewport_offset + rel_i) == selected;
        let item = &items[real_idx];

        let pointer = if is_cursor { " ▶ " } else { "   " };
        let box_glyph = if item.checked {
            "[x]".green().bold().to_string()
        } else {
            "[ ]".dimmed().to_string()
        };
        let tag = if is_cursor { " ◄" } else { "" };
        let num = format!("{:>2}.", viewport_offset + rel_i + 1);

        // Label and description with truncation if needed
        let label_width = 24;
        let label_str = if item.label.chars().count() > label_width {
            crate::ui::truncate_str(&item.label, label_width)
        } else {
            format!("{:<width$}", item.label, width = label_width)
        };

        // Calculate available description width
        // pointer (3) + box_glyph (3) + 1 + num (3) + 1 + label_width (24) + 1 + tag (2) = 38
        let desc_max = inner_width.saturating_sub(42);
        let desc_str = if item.description.chars().count() > desc_max && desc_max > 3 {
            crate::ui::truncate_str(&item.description, desc_max)
        } else {
            item.description.clone()
        };

        let raw_len = 3 + 3 + 1 + num.len() + 1 + label_str.len() + 1 + desc_str.len() + tag.len();
        let pad_len = inner_width.saturating_sub(raw_len.min(inner_width));

        let formatted_line = if is_cursor {
            format!(
                "│{}{}{} {} {} {} {}{}│\r\n",
                pointer.cyan().bold(),
                box_glyph,
                num.bold(),
                label_str.yellow().bold(),
                desc_str.cyan(),
                tag.cyan().bold(),
                " ".repeat(pad_len),
                ""
            )
        } else {
            format!(
                "│{}{}{} {} {} {}{}│\r\n",
                pointer,
                box_glyph,
                num.dimmed(),
                label_str.white(),
                desc_str.dimmed(),
                " ".repeat(pad_len),
                ""
            )
        };

        let _ = queue!(out, Print(formatted_line));
    }

    // Pad empty lines if fewer visible items
    let rendered_count = visible_indices.len();
    if rendered_count < max_visible {
        for _ in 0..(max_visible - rendered_count) {
            let _ = queue!(out, Print(format!("│{}│\r\n", " ".repeat(inner_width))));
        }
    }

    // Bottom Scroll Indicator & Status Summary
    let total_matched = matching_indices.len();
    let checked_count = items.iter().filter(|i| i.checked).count();
    let remaining_below = total_matched.saturating_sub(viewport_offset + max_visible);

    let status_str = if remaining_below > 0 {
        format!(
            "   ▼ {} more below... (Selected: {}/{} items)",
            remaining_below, checked_count, items.len()
        )
    } else {
        format!(
            "   (Selected: {}/{} items)",
            checked_count, items.len()
        )
    };
    let status_pad = inner_width.saturating_sub(status_str.len());
    let _ = queue!(
        out,
        Print(format!(
            "│{}{}{}│\r\n",
            if remaining_below > 0 { status_str.yellow().bold() } else { status_str.dimmed() },
            " ".repeat(status_pad),
            ""
        ))
    );

    let _ = queue!(out, Print(border_color(&div)));

    // Search bar or Footer
    if search_mode {
        let search_text = format!(" Search: {}█ (Enter to lock, Esc to clear)", search_query);
        let s_pad = inner_width.saturating_sub(search_text.len());
        let _ = queue!(
            out,
            Print(format!("│{}{}{}│\r\n", search_text.yellow().bold(), " ".repeat(s_pad), ""))
        );
    } else if !search_query.is_empty() {
        let search_text = format!(" Filter active: '{}' ({} matches) - press [/] to edit", search_query, total_matched);
        let s_pad = inner_width.saturating_sub(search_text.len());
        let _ = queue!(
            out,
            Print(format!("│{}{}{}│\r\n", search_text.cyan(), " ".repeat(s_pad), ""))
        );
    } else {
        let footer = " [Enter] Confirm Selection    [Esc/q] Cancel / Abort";
        let f_pad = inner_width.saturating_sub(footer.len());
        let _ = queue!(
            out,
            Print(format!("│{}{}{}│\r\n", footer.dimmed(), " ".repeat(f_pad), ""))
        );
    }

    // Bottom border
    let bottom = format!("└{}┘\r\n", "─".repeat(inner_width));
    let _ = queue!(out, Print(border_color(&bottom)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checkbox_item_creation() {
        let item = CheckboxItem::new("libyaml", "libyaml", "0.8 MiB [Pure]", true, true);
        assert_eq!(item.id, "libyaml");
        assert_eq!(item.label, "libyaml");
        assert!(item.is_pure);
        assert!(item.checked);
    }

    #[test]
    fn test_batch_toggle_all_and_none() {
        let mut items = vec![
            CheckboxItem::new("a", "a", "", true, false),
            CheckboxItem::new("b", "b", "", false, false),
            CheckboxItem::new("c", "c", "", true, false),
        ];

        // Select All
        for item in &mut items {
            item.checked = true;
        }
        assert!(items.iter().all(|i| i.checked));

        // Select None
        for item in &mut items {
            item.checked = false;
        }
        assert!(items.iter().all(|i| !i.checked));

        // Select Pure Only
        for item in &mut items {
            item.checked = item.is_pure;
        }
        assert!(items[0].checked);
        assert!(!items[1].checked);
        assert!(items[2].checked);

        // Invert
        for item in &mut items {
            item.checked = !item.checked;
        }
        assert!(!items[0].checked);
        assert!(items[1].checked);
        assert!(!items[2].checked);
    }

    #[test]
    fn test_multibyte_utf8_truncation() {
        let label = "🚀🦀日本語ラベル";
        let truncated = crate::ui::truncate_str(label, 5);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncated.chars().count(), 5);

        let desc = "这是一个很长的描述字符串用于测试中文字符截断边界";
        let desc_trunc = crate::ui::truncate_str(desc, 10);
        assert!(desc_trunc.ends_with('…'));
        assert_eq!(desc_trunc.chars().count(), 10);
    }
}
