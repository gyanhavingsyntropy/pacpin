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

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut out = stdout();
        let _ = execute!(out, LeaveAlternateScreen, Show);
    }
}

pub fn run_repo_menu(initial_repos: &[String]) -> Option<Vec<String>> {
    if !io::stdin().is_terminal() {
        eprintln!("Interactive repository menu requires a terminal.");
        return None;
    }

    let mut repos: Vec<String> = if initial_repos.is_empty() {
        vec![
            "core".to_string(),
            "extra".to_string(),
            "multilib".to_string(),
        ]
    } else {
        initial_repos.to_vec()
    };

    let mut selected: usize = 0;
    let mut adding_mode = false;
    let mut add_input = String::new();
    let mut status_msg: Option<(String, bool)> = None; // (message, is_error)

    // Setup terminal
    if enable_raw_mode().is_err() {
        eprintln!("Failed to enable raw terminal mode.");
        return None;
    }
    let mut out = stdout();
    if execute!(out, EnterAlternateScreen, Hide).is_err() {
        let _ = disable_raw_mode();
        eprintln!("Failed to initialize alternate screen.");
        return None;
    }
    let _guard = TerminalGuard;

    loop {
        // Determine terminal width
        let term_width = terminal_size::terminal_size()
            .map(|(w, _)| w.0 as usize)
            .unwrap_or(80);
        let box_width = term_width.min(84).max(60);

        // Render UI
        let _ = queue!(out, MoveTo(0, 0), Clear(ClearType::All));

        render_menu_ui(
            &mut out,
            &repos,
            selected,
            box_width,
            adding_mode,
            &add_input,
            &status_msg,
        );
        let _ = out.flush();

        // Read event
        if let Ok(Event::Key(KeyEvent {
            code, modifiers, ..
        })) = event::read()
        {
            if modifiers.contains(KeyModifiers::CONTROL)
                && matches!(code, KeyCode::Char('c') | KeyCode::Char('C'))
            {
                return None;
            }
            if adding_mode {
                match code {
                    KeyCode::Enter => {
                        let trimmed = add_input.trim();
                        let is_valid = !trimmed.is_empty()
                            && trimmed
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                        if is_valid {
                            if repos.iter().any(|r| r == trimmed) {
                                status_msg = Some((
                                    format!("Repository '[{}]' is already in the list", trimmed),
                                    true,
                                ));
                            } else {
                                repos.insert(selected + 1, trimmed.to_string());
                                selected += 1;
                                status_msg =
                                    Some((format!("Added repository '[{}]'", trimmed), false));
                                adding_mode = false;
                                add_input.clear();
                            }
                        } else {
                            status_msg = Some((
                                "Invalid name: use only letters, numbers, hyphens, and underscores"
                                    .to_string(),
                                true,
                            ));
                        }
                    }
                    KeyCode::Esc => {
                        adding_mode = false;
                        add_input.clear();
                        status_msg = None;
                    }
                    KeyCode::Backspace => {
                        add_input.pop();
                    }
                    KeyCode::Char(c) => {
                        if add_input.len() < 32
                            && (c.is_ascii_alphanumeric() || c == '-' || c == '_')
                        {
                            add_input.push(c);
                        }
                    }
                    _ => {}
                }
            } else {
                match code {
                    // Navigation Up
                    KeyCode::Up | KeyCode::Char('k') => {
                        status_msg = None;
                        if modifiers.contains(KeyModifiers::SHIFT) {
                            // Move Up in Priority (Shift+Up or Shift+k)
                            if selected > 0 {
                                repos.swap(selected, selected - 1);
                                selected -= 1;
                            }
                        } else if selected > 0 {
                            selected -= 1;
                        }
                    }
                    // Navigation Down
                    KeyCode::Down | KeyCode::Char('j') => {
                        status_msg = None;
                        if modifiers.contains(KeyModifiers::SHIFT) {
                            // Move Down in Priority (Shift+Down or Shift+j)
                            if selected + 1 < repos.len() {
                                repos.swap(selected, selected + 1);
                                selected += 1;
                            }
                        } else if selected + 1 < repos.len() {
                            selected += 1;
                        }
                    }
                    // Reorder Priority Up: + or =
                    KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Char('K') => {
                        status_msg = None;
                        if selected > 0 {
                            repos.swap(selected, selected - 1);
                            selected -= 1;
                        }
                    }
                    // Reorder Priority Down: - or _
                    KeyCode::Char('-') | KeyCode::Char('_') | KeyCode::Char('J') => {
                        status_msg = None;
                        if selected + 1 < repos.len() {
                            repos.swap(selected, selected + 1);
                            selected += 1;
                        }
                    }
                    // Add repo
                    KeyCode::Char('a') | KeyCode::Char('A') => {
                        adding_mode = true;
                        add_input.clear();
                        status_msg = None;
                    }
                    // Delete repo
                    KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Delete => {
                        if repos.len() > 1 {
                            let removed = repos.remove(selected);
                            if selected >= repos.len() {
                                selected = repos.len() - 1;
                            }
                            status_msg =
                                Some((format!("Removed repository '[{}]'", removed), false));
                        } else {
                            status_msg =
                                Some(("Cannot remove the last repository".to_string(), true));
                        }
                    }
                    // Save and exit
                    KeyCode::Enter => {
                        return Some(repos);
                    }
                    // Cancel and exit
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                        return None;
                    }
                    _ => {}
                }
            }
        }
    }
}

fn render_menu_ui<W: Write>(
    out: &mut W,
    repos: &[String],
    selected: usize,
    box_width: usize,
    adding_mode: bool,
    add_input: &str,
    status_msg: &Option<(String, bool)>,
) {
    let inner_width = box_width.saturating_sub(2);
    let border_color = |s: &str| s.cyan().bold();

    // Top border
    let top = format!("┌{}┐\r\n", "─".repeat(inner_width));
    let _ = queue!(out, Print(border_color(&top)));

    // Header Title
    let title = "BIOS REPOSITORY PRIORITY & SEARCH ORDER";
    let title_pad = inner_width.saturating_sub(title.len()) / 2;
    let title_line = format!(
        "│{}{}{}│\r\n",
        " ".repeat(title_pad),
        title.bold().yellow(),
        " ".repeat(inner_width.saturating_sub(title_pad + title.len()))
    );
    let _ = queue!(out, Print(title_line));

    // Subtitle
    let sub = "Higher priority repositories are searched first for packages";
    let sub_pad = inner_width.saturating_sub(sub.len()) / 2;
    let sub_line = format!(
        "│{}{}{}│\r\n",
        " ".repeat(sub_pad),
        sub.dimmed(),
        " ".repeat(inner_width.saturating_sub(sub_pad + sub.len()))
    );
    let _ = queue!(out, Print(sub_line));

    // Divider
    let div = format!("├{}┤\r\n", "─".repeat(inner_width));
    let _ = queue!(out, Print(border_color(&div)));

    // Blank line
    let _ = queue!(out, Print(format!("│{}│\r\n", " ".repeat(inner_width))));

    // Repo list
    for (i, repo) in repos.iter().enumerate() {
        let is_selected = i == selected;
        let (prio_raw, prio_colored) = if i == 0 {
            (
                "(Priority 1 - Highest)".to_string(),
                if is_selected {
                    "(Priority 1 - Highest)".green().bold()
                } else {
                    "(Priority 1 - Highest)".green().dimmed()
                },
            )
        } else if i == repos.len() - 1 {
            let s = format!("(Priority {} - Lowest)", i + 1);
            let c = s.clone().dimmed();
            (s, c)
        } else {
            let s = format!("(Priority {})", i + 1);
            let c = if is_selected {
                s.clone().cyan()
            } else {
                s.clone().dimmed()
            };
            (s, c)
        };

        let pointer = if is_selected { " ▶ " } else { "   " };
        let tag = if is_selected { " ◄ SELECTED " } else { "" };
        let num = format!("{:>2}.", i + 1);
        let repo_display = format!(" [{}]", repo);

        // Visual formatting
        let raw_len = 3 + num.len() + repo_display.len() + 1 + prio_raw.len() + tag.len();
        let pad_len = inner_width.saturating_sub(raw_len.min(inner_width));

        let formatted_line = if is_selected {
            format!(
                "│{}{}{} {} {}{}│\r\n",
                pointer.cyan().bold(),
                num.bold(),
                repo_display.yellow().bold(),
                prio_colored,
                tag.cyan().bold(),
                " ".repeat(pad_len),
            )
        } else {
            format!(
                "│{}{}{} {}{}│\r\n",
                pointer,
                num.dimmed(),
                repo_display.white(),
                prio_colored,
                " ".repeat(pad_len),
            )
        };

        let _ = queue!(out, Print(formatted_line));
    }

    // Blank line
    let _ = queue!(out, Print(format!("│{}│\r\n", " ".repeat(inner_width))));

    // Status / message line if any
    if let Some((msg, is_err)) = status_msg {
        let msg_str = if *is_err {
            format!(" ⚠ {}", msg).red().bold().to_string()
        } else {
            format!(" ✔ {}", msg).green().bold().to_string()
        };
        let raw_len = msg.len() + 3;
        let pad = inner_width.saturating_sub(raw_len);
        let _ = queue!(
            out,
            Print(format!("│{}{}{}│\r\n", msg_str, " ".repeat(pad), ""))
        );
    } else {
        let _ = queue!(out, Print(format!("│{}│\r\n", " ".repeat(inner_width))));
    }

    // Lower divider
    let _ = queue!(out, Print(border_color(&div)));

    // Prompt mode vs navigation footer
    if adding_mode {
        let prompt_text = format!(" Enter repository name: {}█", add_input);
        let raw_len = prompt_text.len();
        let pad = inner_width.saturating_sub(raw_len);
        let line1 = format!(
            "│{}{}{}│\r\n",
            prompt_text.yellow().bold(),
            " ".repeat(pad),
            ""
        );
        let _ = queue!(out, Print(line1));

        let help = " [Enter] Confirm   [Esc] Cancel";
        let help_pad = inner_width.saturating_sub(help.len());
        let line2 = format!("│{}{}{}│\r\n", help.dimmed(), " ".repeat(help_pad), "");
        let _ = queue!(out, Print(line2));
    } else {
        let help1 = " [↑/↓] or [k/j] Select    [+/-] Move Priority    [a] Add Repo";
        let help1_pad = inner_width.saturating_sub(help1.len());
        let line1 = format!("│{}{}{}│\r\n", help1.cyan(), " ".repeat(help1_pad), "");
        let _ = queue!(out, Print(line1));

        let help2 = " [d] Delete Repo          [Enter] Save Order     [Esc/q] Cancel";
        let help2_pad = inner_width.saturating_sub(help2.len());
        let line2 = format!("│{}{}{}│\r\n", help2.dimmed(), " ".repeat(help2_pad), "");
        let _ = queue!(out, Print(line2));
    }

    // Bottom border
    let bottom = format!("└{}┘\r\n", "─".repeat(inner_width));
    let _ = queue!(out, Print(border_color(&bottom)));
}
