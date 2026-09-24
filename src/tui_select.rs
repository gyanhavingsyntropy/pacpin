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
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType},
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
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        is_pure: bool,
        checked: bool,
    ) -> Self {
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
        // Keep the menu in scrollback and leave the cursor below it.
        let _ = execute!(out, Show, Print("\r\n"));
    }
}

const MENU_FIXED_ROWS: usize = 12;

fn menu_layout(term_height: usize, item_count: usize) -> (usize, u16) {
    // Preserve at least a third of the screen for the update plan above the menu.
    let history_rows = (term_height / 3).max(4);
    let visible = item_count.min(18).min(
        term_height
            .saturating_sub(history_rows + MENU_FIXED_ROWS)
            .max(1),
    );
    let start_row = term_height.saturating_sub(MENU_FIXED_ROWS + visible) as u16;
    (visible, start_row)
}

pub fn run_checkbox_menu(
    title: &str,
    subtitle: &str,
    initial_items: &[CheckboxItem],
) -> Option<Vec<String>> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() || initial_items.is_empty() {
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
    if execute!(out, Hide).is_err() {
        let _ = disable_raw_mode();
        return None;
    }
    let _guard = TerminalGuard;

    // Allocate space in the primary screen so the preceding update summary
    // remains above the selector and can still be reached via scrollback.
    let (_, initial_height) = terminal_size::terminal_size()
        .map(|(w, h)| (w.0 as usize, h.0 as usize))
        .unwrap_or((80, 24));
    if initial_height < MENU_FIXED_ROWS + 1 {
        return None;
    }
    let (initial_visible, _) = menu_layout(initial_height, items.len());
    for _ in 0..MENU_FIXED_ROWS + initial_visible {
        let _ = queue!(out, Print("\r\n"));
    }
    let _ = out.flush();
    let mut previous_region: Option<(u16, usize)> = None;

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
        let box_width = term_width.clamp(72, 100).min(term_width);
        let (max_visible, start_row) = menu_layout(term_height, items.len());

        // Adjust viewport offset
        if selected < viewport_offset {
            viewport_offset = selected;
        } else if selected >= viewport_offset + max_visible {
            viewport_offset = selected - max_visible + 1;
        }

        // Erase only the previous selector, never the update summary above it.
        if let Some((old_start, old_height)) = previous_region {
            for row in old_start as usize..(old_start as usize + old_height).min(term_height) {
                let _ = queue!(out, MoveTo(0, row as u16), Clear(ClearType::CurrentLine));
            }
        }
        let _ = queue!(out, MoveTo(0, start_row));

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
        previous_region = Some((start_row, MENU_FIXED_ROWS + max_visible));

        // Read event
        match event::read() {
            Ok(Event::Key(KeyEvent {
                code, modifiers, ..
            })) => {
                if modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(code, KeyCode::Char('c') | KeyCode::Char('C'))
                {
                    return None;
                }
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
                            selected = selected.saturating_sub(1);
                        }
                        // Navigation Down
                        KeyCode::Down | KeyCode::Char('j') => {
                            if !matching_indices.is_empty() && selected + 1 < matching_indices.len()
                            {
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
                        _ => {}
                    }
                }
            }
            Ok(_) => {}
            Err(_) => return None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_checkbox_ui<W: Write>(
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
    let b_vert = border_color("│");
    let has_purity = items.iter().any(|i| !i.is_pure) || title.to_lowercase().contains("orphan");

    // Top border
    let top = format!(
        "{}\r\n",
        border_color(&format!("┌{}┐", "─".repeat(inner_width)))
    );
    let _ = queue!(out, Print(top));

    // Header Title
    let title_w = crate::ui::str_width(title);
    let title_pad = inner_width.saturating_sub(title_w) / 2;
    let title_right_pad = inner_width.saturating_sub(title_pad + title_w);
    let title_line = format!(
        "{}{}{}{}{}\r\n",
        b_vert,
        " ".repeat(title_pad),
        title.bold().yellow(),
        " ".repeat(title_right_pad),
        b_vert
    );
    let _ = queue!(out, Print(title_line));

    // Subtitle
    let sub_disp = if crate::ui::str_width(subtitle) > inner_width {
        crate::ui::truncate_str(subtitle, inner_width)
    } else {
        subtitle.to_string()
    };
    let sub_w = crate::ui::str_width(&sub_disp);
    let sub_pad = inner_width.saturating_sub(sub_w) / 2;
    let sub_right_pad = inner_width.saturating_sub(sub_pad + sub_w);
    let sub_line = format!(
        "{}{}{}{}{}\r\n",
        b_vert,
        " ".repeat(sub_pad),
        sub_disp.dimmed(),
        " ".repeat(sub_right_pad),
        b_vert
    );
    let _ = queue!(out, Print(sub_line));

    // Divider
    let div = format!(
        "{}\r\n",
        border_color(&format!("├{}┤", "─".repeat(inner_width)))
    );
    let _ = queue!(out, Print(div.clone()));

    // Shortcuts Bar
    let bar1 = if has_purity {
        " [Space] Toggle    [a] All     [n] None    [p] Pure Only  [i] Invert"
    } else {
        " [Space] Toggle    [a] All     [n] None    [i] Invert"
    };
    let bar1_disp = if crate::ui::str_width(bar1) > inner_width {
        crate::ui::truncate_str(bar1, inner_width)
    } else {
        bar1.to_string()
    };
    let bar1_w = crate::ui::str_width(&bar1_disp);
    let bar1_pad = inner_width.saturating_sub(bar1_w);
    let _ = queue!(
        out,
        Print(format!(
            "{}{}{}{}\r\n",
            b_vert,
            bar1_disp.cyan(),
            " ".repeat(bar1_pad),
            b_vert
        ))
    );

    let bar2 = " [↑/↓] Navigate    [PgUp/Dn] Scroll        [/] Search     [Enter] Confirm";
    let bar2_disp = if crate::ui::str_width(bar2) > inner_width {
        crate::ui::truncate_str(bar2, inner_width)
    } else {
        bar2.to_string()
    };
    let bar2_w = crate::ui::str_width(&bar2_disp);
    let bar2_pad = inner_width.saturating_sub(bar2_w);
    let _ = queue!(
        out,
        Print(format!(
            "{}{}{}{}\r\n",
            b_vert,
            bar2_disp.dimmed(),
            " ".repeat(bar2_pad),
            b_vert
        ))
    );

    let _ = queue!(out, Print(div.clone()));

    // Top Scroll Indicator
    if viewport_offset > 0 {
        let more_top = format!("   ▲ {} more items above...", viewport_offset);
        let more_w = crate::ui::str_width(&more_top);
        let more_pad = inner_width.saturating_sub(more_w);
        let _ = queue!(
            out,
            Print(format!(
                "{}{}{}{}\r\n",
                b_vert,
                more_top.yellow().bold(),
                " ".repeat(more_pad),
                b_vert
            ))
        );
    } else {
        let _ = queue!(
            out,
            Print(format!(
                "{}{}{}\r\n",
                b_vert,
                " ".repeat(inner_width),
                b_vert
            ))
        );
    }

    // Visible Items
    let visible_indices = &matching_indices
        [viewport_offset..matching_indices.len().min(viewport_offset + max_visible)];
    for (rel_i, &real_idx) in visible_indices.iter().enumerate() {
        let is_cursor = (viewport_offset + rel_i) == selected;
        let item = &items[real_idx];

        let pointer = if is_cursor { " ▶ " } else { "   " };
        let box_raw = if item.checked { "[x] " } else { "[ ] " };
        let box_colored = if item.checked {
            "[x] ".green().bold()
        } else {
            "[ ] ".dimmed()
        };
        let tag_raw = if is_cursor { " ◄" } else { "" };
        let num_raw = format!("{:>2}. ", viewport_offset + rel_i + 1);

        // Label with truncation or padding to fixed column width
        let label_width = 24;
        let item_label_w = crate::ui::str_width(&item.label);
        let label_str = if item_label_w > label_width {
            crate::ui::truncate_str(&item.label, label_width)
        } else {
            format!("{}{}", item.label, " ".repeat(label_width - item_label_w))
        };

        // Purity badge: [pure] in green bold, [opt] in yellow bold
        let (badge_raw, badge_colored) = if has_purity {
            if item.is_pure {
                ("[pure] ", "[pure] ".green().bold())
            } else {
                ("[opt]  ", "[opt]  ".yellow().bold())
            }
        } else {
            ("", "".normal())
        };

        // Calculate available description width
        let prefix_w = 3
            + 4
            + 4
            + (if has_purity { 7 } else { 0 })
            + label_width
            + (if is_cursor { 2 } else { 0 });
        let desc_max = inner_width.saturating_sub(prefix_w + 1);
        let desc_str = if crate::ui::str_width(&item.description) > desc_max && desc_max > 3 {
            crate::ui::truncate_str(&item.description, desc_max)
        } else {
            item.description.clone()
        };

        let content_raw =
            format!("{pointer}{box_raw}{num_raw}{badge_raw}{label_str}{desc_str}{tag_raw}");
        let used_w = crate::ui::str_width(&content_raw);
        let pad_len = inner_width.saturating_sub(used_w);
        let pad_spaces = " ".repeat(pad_len);

        let formatted_line = if is_cursor {
            format!(
                "{}{}{}{}{}{}{}{}{}{}\r\n",
                b_vert,
                pointer.cyan().bold(),
                box_colored,
                num_raw.bold(),
                badge_colored,
                label_str.yellow().bold(),
                desc_str.cyan(),
                tag_raw.cyan().bold(),
                pad_spaces,
                b_vert
            )
        } else {
            format!(
                "{}{}{}{}{}{}{}{}{}\r\n",
                b_vert,
                pointer,
                box_colored,
                num_raw.dimmed(),
                badge_colored,
                label_str.white(),
                desc_str.dimmed(),
                pad_spaces,
                b_vert
            )
        };

        let _ = queue!(out, Print(formatted_line));
    }

    // Pad empty lines if fewer visible items
    let rendered_count = visible_indices.len();
    if rendered_count < max_visible {
        for _ in 0..(max_visible - rendered_count) {
            let _ = queue!(
                out,
                Print(format!(
                    "{}{}{}\r\n",
                    b_vert,
                    " ".repeat(inner_width),
                    b_vert
                ))
            );
        }
    }

    // Bottom Scroll Indicator & Status Summary
    let total_matched = matching_indices.len();
    let checked_count = items.iter().filter(|i| i.checked).count();
    let remaining_below = total_matched.saturating_sub(viewport_offset + max_visible);

    let status_str = if remaining_below > 0 {
        format!(
            "   ▼ {} more below... (Selected: {}/{} items)",
            remaining_below,
            checked_count,
            items.len()
        )
    } else {
        format!("   (Selected: {}/{} items)", checked_count, items.len())
    };
    let status_w = crate::ui::str_width(&status_str);
    let status_pad = inner_width.saturating_sub(status_w);
    let status_colored = if remaining_below > 0 {
        status_str.yellow().bold()
    } else {
        status_str.dimmed()
    };
    let _ = queue!(
        out,
        Print(format!(
            "{}{}{}{}\r\n",
            b_vert,
            status_colored,
            " ".repeat(status_pad),
            b_vert
        ))
    );

    let _ = queue!(out, Print(div));

    // Search bar or Footer
    if search_mode {
        let search_text = format!(" Search: {}█ (Enter to lock, Esc to clear)", search_query);
        let s_w = crate::ui::str_width(&search_text);
        let s_pad = inner_width.saturating_sub(s_w);
        let _ = queue!(
            out,
            Print(format!(
                "{}{}{}{}\r\n",
                b_vert,
                search_text.yellow().bold(),
                " ".repeat(s_pad),
                b_vert
            ))
        );
    } else if !search_query.is_empty() {
        let search_text = format!(
            " Filter active: '{}' ({} matches) - press [/] to edit",
            search_query, total_matched
        );
        let s_w = crate::ui::str_width(&search_text);
        let s_pad = inner_width.saturating_sub(s_w);
        let _ = queue!(
            out,
            Print(format!(
                "{}{}{}{}\r\n",
                b_vert,
                search_text.cyan(),
                " ".repeat(s_pad),
                b_vert
            ))
        );
    } else {
        let footer = " [Enter] Confirm Selection    [Esc/q] Cancel / Abort";
        let f_w = crate::ui::str_width(footer);
        let f_pad = inner_width.saturating_sub(f_w);
        let _ = queue!(
            out,
            Print(format!(
                "{}{}{}{}\r\n",
                b_vert,
                footer.dimmed(),
                " ".repeat(f_pad),
                b_vert
            ))
        );
    }

    // Bottom border
    let bottom = border_color(&format!("└{}┘", "─".repeat(inner_width)));
    let _ = queue!(out, Print(bottom));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_menu_keeps_history_visible() {
        assert_eq!(menu_layout(24, 50), (4, 8));
        assert_eq!(menu_layout(40, 50), (15, 13));
        assert_eq!(menu_layout(24, 2), (2, 10));
    }

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
        assert!(crate::ui::str_width(&truncated) <= 5);

        let desc = "这是一个很长的描述字符串用于测试中文字符截断边界";
        let desc_trunc = crate::ui::truncate_str(desc, 10);
        assert!(desc_trunc.ends_with('…'));
        assert!(crate::ui::str_width(&desc_trunc) <= 10);
    }

    #[test]
    fn test_render_checkbox_ui_box_alignment() {
        let items = vec![
            CheckboxItem::new(
                "libyaml",
                "libyaml",
                "0.8 MiB — YAML 1.1 parser",
                true,
                true,
            ),
            CheckboxItem::new(
                "python-pillow",
                "python-pillow",
                "1.2 MiB (for: gimp, krita) — Python Imaging Library",
                false,
                false,
            ),
            CheckboxItem::new(
                "unicode-test",
                "unicode-🚀-pkg",
                "4.5 MiB — 测试中文与EM-DASH—符号",
                true,
                false,
            ),
        ];

        for box_width in [72, 80, 88, 100] {
            let mut buf = Vec::new();
            let matching = vec![0, 1, 2];
            render_checkbox_ui(
                &mut buf,
                "SELECT ORPHANED PACKAGES TO REMOVE",
                "Pure orphans are pre-selected ([x]). Optional plugins are unchecked ([ ]).",
                &items,
                &matching,
                0,
                0,
                5,
                box_width,
                false,
                "",
            );

            let output = String::from_utf8(buf).expect("valid utf8");
            assert_eq!(output.lines().count(), MENU_FIXED_ROWS + 5);
            assert!(!output.ends_with("\r\n"));
            if box_width == 88 {
                println!("\n--- RENDERED CHECKBOX UI (WIDTH 88) ---\n{}", output);
            }
            let lines: Vec<&str> = output.lines().filter(|l| !l.is_empty()).collect();
            assert!(!lines.is_empty());
            for line in lines {
                let w = crate::ui::str_width(line);
                assert_eq!(
                    w, box_width,
                    "Line '{}' has display width {} instead of expected {} for box_width {}",
                    line, w, box_width, box_width
                );
            }
        }
    }
}
