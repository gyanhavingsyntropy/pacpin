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

use crate::resolver::{ResolveResult, ResolvedPackage};
use colored::Colorize;
use std::collections::HashSet;
use std::io::{self, Write};

pub fn print_banner() {
    println!(
        "\n{} • {} {}",
        "pacpin v3.1".bold().cyan(),
        "Declarative Package Resolver & Upgrade Engine".dimmed(),
        "(GPLv3)".dimmed()
    );
    crate::alias::AliasManager::check_and_notify_once();
}

pub fn char_width(ch: char) -> usize {
    let cp = ch as u32;
    if cp < 0x20 || (0x7F..=0x9F).contains(&cp) {
        return 0; // Control characters
    }
    // Zero-width characters (ZWJ, soft hyphen, etc.)
    if cp == 0xAD
        || (0x200B..=0x200F).contains(&cp)
        || cp == 0xFEFF
        || (0x2060..=0x206F).contains(&cp)
    {
        return 0;
    }
    // Wide characters and emojis:
    // CJK, Hangul, Fullwidth forms, Miscellaneous Symbols and Pictographs, Emoticons, etc.
    if (0x1100..=0x115F).contains(&cp)
        || (0x231A..=0x231B).contains(&cp)
        || (0x23E9..=0x23FA).contains(&cp)
        || (0x25FD..=0x25FE).contains(&cp)
        || (0x2614..=0x2615).contains(&cp)
        || (0x2648..=0x2653).contains(&cp)
        || (0x267F..=0x27BF).contains(&cp)
        || (0x2B1B..=0x2B55).contains(&cp)
        || (0x2E80..=0xA4CF).contains(&cp)
        || (0xAC00..=0xD7A3).contains(&cp)
        || (0xF900..=0xFAFF).contains(&cp)
        || (0xFE10..=0xFE19).contains(&cp)
        || (0xFE30..=0xFE6F).contains(&cp)
        || (0xFF00..=0xFF60).contains(&cp)
        || (0xFFE0..=0xFFE6).contains(&cp)
        || (0x1F000..=0x1FFFF).contains(&cp)
        || (0x20000..=0x3FFFF).contains(&cp)
    {
        return 2;
    }
    1
}

pub fn str_width(s: &str) -> usize {
    let mut w = 0;
    let mut in_escape = false;
    for ch in s.chars() {
        if ch == '\x1b' {
            in_escape = true;
            continue;
        }
        if in_escape {
            if ch == 'm' {
                in_escape = false;
            }
            continue;
        }
        w += char_width(ch);
    }
    w
}


pub fn format_size(bytes_val: i64) -> String {
    let abs_val = bytes_val.abs() as f64;
    if abs_val < 1024.0 {
        format!("{} B", bytes_val)
    } else if abs_val < 1024.0 * 1024.0 {
        format!("{:.1} KiB", (bytes_val as f64) / 1024.0)
    } else if abs_val < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.2} MiB", (bytes_val as f64) / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GiB", (bytes_val as f64) / (1024.0 * 1024.0 * 1024.0))
    }
}

pub fn format_delta_text(delta: i64) -> (String, colored::Color) {
    if delta > 0 {
        (format!("+{}", format_size(delta)), colored::Color::Yellow)
    } else if delta < 0 {
        (format!("-{}", format_size(delta.abs())), colored::Color::Green)
    } else {
        ("~0 B".to_string(), colored::Color::BrightBlack)
    }
}

pub fn prompt_items_selection(prompt_text: &str, items: &[String]) -> Vec<String> {
    if items.is_empty() {
        return Vec::new();
    }

    print!("\n{} ", prompt_text.bold());
    io::stdout().flush().unwrap();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        println!();
        return Vec::new();
    }
    let raw = input.trim();
    parse_selection_indices(raw, items)
}

pub fn parse_selection_indices(raw: &str, items: &[String]) -> Vec<String> {
    if raw.is_empty() || raw.eq_ignore_ascii_case("all") {
        return items.to_vec();
    }
    if raw.eq_ignore_ascii_case("none") || raw == "^" {
        return Vec::new();
    }

    let (invert, tokens_str) = if raw.starts_with('^') {
        (true, &raw[1..])
    } else {
        (false, raw)
    };

    let total = items.len();
    let mut selected_indices = HashSet::new();

    for token in tokens_str.replace(',', " ").split_whitespace() {
        if token.contains('-') {
            let parts: Vec<&str> = token.splitn(2, '-').collect();
            if let (Ok(s), Ok(e)) = (parts[0].parse::<usize>(), parts[1].parse::<usize>()) {
                let start = s.min(e);
                let end = s.max(e);
                for idx in start..=end {
                    if idx >= 1 && idx <= total {
                        selected_indices.insert(idx);
                    }
                }
            }
        } else if let Ok(idx) = token.parse::<usize>() {
            if idx >= 1 && idx <= total {
                selected_indices.insert(idx);
            }
        }
    }

    let mut chosen_indices: Vec<usize> = if invert {
        (1..=total).filter(|i| !selected_indices.contains(i)).collect()
    } else {
        selected_indices.into_iter().collect()
    };
    chosen_indices.sort();

    chosen_indices
        .into_iter()
        .map(|i| items[i - 1].clone())
        .collect()
}

pub fn prompt_multiselect(title: &str, repo: &str, items: &[String]) -> Vec<String> {
    if items.is_empty() {
        return Vec::new();
    }

    println!("\n{} {}:", "::".cyan(), title.bold());
    println!("{} Repository {}", "::".cyan(), repo.green());

    let formatted_items: Vec<String> = items
        .iter()
        .enumerate()
        .map(|(i, name)| format!("{}) {}", i + 1, name))
        .collect();

    let mut current_line = String::from("   ");
    for item_str in &formatted_items {
        if current_line.len() + item_str.len() + 2 > 80 {
            println!("{}", current_line.trim_end());
            current_line = format!("   {}  ", item_str);
        } else {
            current_line.push_str(item_str);
            current_line.push_str("  ");
        }
    }
    if !current_line.trim().is_empty() {
        println!("{}", current_line.trim_end());
    }

    prompt_items_selection("Enter a selection (default=all, 'none' to skip):", items)
}

pub fn render_orphans_summary(orphans: &[crate::db::OrphanPackage]) {
    if orphans.is_empty() {
        return;
    }

    let pure: Vec<(usize, &crate::db::OrphanPackage)> = orphans
        .iter()
        .enumerate()
        .filter(|(_, o)| o.optional_for.is_empty())
        .collect();

    let optional: Vec<(usize, &crate::db::OrphanPackage)> = orphans
        .iter()
        .enumerate()
        .filter(|(_, o)| !o.optional_for.is_empty())
        .collect();

    let tot_size: i64 = orphans.iter().map(|o| o.isize).sum();
    let tot_size_str = format_size(tot_size);

    println!(
        "\n{}",
        format!(
            "🧹 Orphaned Packages Detected ({} unneeded — {}):",
            orphans.len(),
            tot_size_str
        )
        .bold()
        .yellow()
    );

    let render_row = |idx: usize, o: &crate::db::OrphanPackage| {
        let size_str = format_size(o.isize);
        let note = if o.is_projected {
            format!(" [{}]", format!("projected: dropped by {}", o.dropped_by.join(", ")).cyan())
        } else if !o.optional_for.is_empty() {
            format!(" (opt for: {})", o.optional_for.join(", ").dimmed())
        } else {
            String::new()
        };

        let desc_str = if !o.desc.is_empty() {
            format!(" — {}", o.desc.dimmed())
        } else {
            String::new()
        };

        println!(
            "  {:>2}) {:<26} {:<16} {:>10}{}{}",
            idx + 1,
            o.name.bold(),
            o.version.dimmed(),
            size_str.yellow(),
            note,
            desc_str
        );
    };

    if !pure.is_empty() {
        let pure_size: i64 = pure.iter().map(|(_, o)| o.isize).sum();
        println!(
            "  {}",
            format!(
                "▶ Pure Orphans ({} unneeded — {}) [Safe to remove]:",
                pure.len(),
                format_size(pure_size)
            )
            .green()
            .bold()
        );
        for (i, o) in &pure {
            render_row(*i, o);
        }
    }

    if !optional.is_empty() {
        let opt_size: i64 = optional.iter().map(|(_, o)| o.isize).sum();
        if !pure.is_empty() {
            println!();
        }
        println!(
            "  {}",
            format!(
                "▶ Optional Dependencies ({} unneeded strictly — {}) [Used optionally by installed apps]:",
                optional.len(),
                format_size(opt_size)
            )
            .yellow()
            .bold()
        );
        for (i, o) in &optional {
            render_row(*i, o);
        }
    }
}

pub fn prompt_orphan_selection(
    orphans: &[crate::db::OrphanPackage],
    is_after_upgrade: bool,
) -> Vec<String> {
    if orphans.is_empty() {
        return Vec::new();
    }

    let pure_names: Vec<String> = orphans
        .iter()
        .filter(|o| o.optional_for.is_empty())
        .map(|o| o.name.clone())
        .collect();

    let all_names: Vec<String> = orphans.iter().map(|o| o.name.clone()).collect();

    let prompt_msg = if is_after_upgrade {
        if !pure_names.is_empty() {
            "Remove orphaned packages after upgrade? (default=pure, 'p' for pure, 'all' for all, 'none' to skip):"
        } else {
            "Remove orphaned packages after upgrade? (default=all, 'none' to skip):"
        }
    } else {
        if !pure_names.is_empty() {
            "Enter orphaned packages to remove (default=pure, 'p' for pure, 'all' for all, 'none' to skip):"
        } else {
            "Enter orphaned packages to remove (default=all, 'none' to skip):"
        }
    };

    print!("\n{} ", prompt_msg.bold());
    io::stdout().flush().unwrap();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        println!();
        return Vec::new();
    }
    let raw = input.trim();

    if raw.is_empty() {
        if !pure_names.is_empty() {
            return pure_names;
        } else {
            return all_names;
        }
    }

    if raw.eq_ignore_ascii_case("p")
        || raw.eq_ignore_ascii_case("pure")
        || raw.eq_ignore_ascii_case("safe")
    {
        return pure_names;
    }

    if raw.eq_ignore_ascii_case("a") || raw.eq_ignore_ascii_case("all") {
        return all_names;
    }

    if raw.eq_ignore_ascii_case("n") || raw.eq_ignore_ascii_case("none") || raw == "^" {
        return Vec::new();
    }

    parse_selection_indices(raw, &all_names)
}


pub fn render_transaction_view(
    data: &ResolveResult,
    helper: &str,
    external_updates: &[crate::integrations::ExternalUpdate],
) -> Vec<ResolvedPackage> {
    let updates = &data.updates;
    let held = &data.held_packages;

    if !data.unresolved_pins.is_empty() {
        println!("\n{}", format!("⚠ Unresolved Custom Pins ({}):", data.unresolved_pins.len()).yellow().bold());
        for (pkg, repo) in &data.unresolved_pins {
            println!("  • {:<28} ➔ [{}] NOT FOUND", pkg.bold(), repo.red());
        }
    }

    if !held.is_empty() {
        println!("\n{}", format!("⏸ Held Packages ({}):", held.len()).yellow().bold());
        for p in held {
            let cand_ver = p
                .candidate
                .as_ref()
                .map(|c| c.version.as_str())
                .unwrap_or("unknown");
            let reason = if p.hold_reason.is_empty() {
                "held"
            } else {
                p.hold_reason.as_str()
            };
            println!(
                "  • {:<28} : {} ➔ {}  ({})",
                p.name.bold(),
                p.installed_ver,
                cand_ver,
                reason.yellow()
            );
        }
    }

    if updates.is_empty() && external_updates.is_empty() {
        if held.is_empty() {
            println!("\n{}", "✔ System is fully up to date!".green());
            println!(
                "  {} packages inspected across {} repositories.",
                data.installed_count,
                data.repos.len()
            );
        }
        return Vec::new();
    }

    let mut tot_csize: i64 = 0;
    let mut tot_isize: i64 = 0;
    let mut tot_delta: i64 = 0;
    let mut custom_count = 0;
    let mut default_count = 0;
    let mut pacman_count = 0;
    let mut aur_count = 0;

    if !updates.is_empty() {
        println!(
            "\n{}",
            format!("Pending Package Transactions ({}):", updates.len()).bold()
        );

        let header = format!(
            "  {:<9} {:<28} {:<12} {:<32} {:>10} {:>10} {:>11}",
            "STATE", "PACKAGE", "REPO", "VERSION", "DOWNLOAD", "INSTALLED", "NET DELTA"
        );
        println!("{}", header.bold());
        println!(
            "  {:<9} {:<28} {:<12} {:<32} {:>10} {:>10} {:>11}",
            "─────────",
            "────────────────────────────",
            "────────────",
            "────────────────────────────────",
            "──────────",
            "──────────",
            "───────────"
        );

        for p in updates {
            let cand = match p.candidate.as_ref() {
                Some(c) => c,
                None => continue,
            };

            if cand.is_aur {
                aur_count += 1;
            } else {
                pacman_count += 1;
            }

            let state_str = if p.state == "custom" {
                custom_count += 1;
                if p.is_downgrade {
                    format!("{:<9}", "[down]".red())
                } else if p.update_type == "pin_sync" {
                    format!("{:<9}", "[sync]".magenta())
                } else {
                    format!("{:<9}", "[custom]".cyan())
                }
            } else {
                default_count += 1;
                format!("{:<9}", "[default]".green())
            };

            let repo_display = format!("[{}]", cand.repo);
            let repo_str = if p.state == "custom" {
                format!("{:<12}", repo_display.yellow())
            } else {
                format!("{:<12}", repo_display.dimmed())
            };

            let ver_str = format!("{} ➔ {}", p.installed_ver, cand.version.bold());
            let csize_str = if cand.csize > 0 {
                format_size(cand.csize)
            } else {
                "-".to_string()
            };
            let isize_str = if cand.isize > 0 {
                format_size(cand.isize)
            } else {
                "-".to_string()
            };
            let (delta_str, delta_color) = format_delta_text(p.net_delta);

            tot_csize += cand.csize;
            tot_isize += cand.isize;
            tot_delta += p.net_delta;

            println!(
                "  {} {:<28} {} {:<32} {:>10} {:>10} {:>11}",
                state_str,
                p.name.bold(),
                repo_str,
                ver_str,
                csize_str.dimmed(),
                isize_str.dimmed(),
                delta_str.color(delta_color)
            );
        }
    }

    if !external_updates.is_empty() {
        println!(
            "\n{}",
            format!("External Package Transactions ({}):", external_updates.len()).bold()
        );

        let ext_header = format!(
            "  {:<10} {:<32} {:<12} {:<24}",
            "RUNNER", "PACKAGE / APP ID", "REPO", "TARGET VERSION"
        );
        println!("{}", ext_header.bold());
        println!(
            "  {:<10} {:<32} {:<12} {:<24}",
            "──────────",
            "────────────────────────────────",
            "────────────",
            "────────────────────────"
        );

        for ext in external_updates {
            let repo_display = format!("[{}]", ext.repo);
            println!(
                "  {:<10} {:<32} {:<12} {:<24}",
                ext.runner.cyan(),
                ext.name.bold(),
                repo_display.dimmed(),
                ext.version.green()
            );
        }
    }

    let mut flatpak_count = 0;
    let mut nix_count = 0;
    for ext in external_updates {
        if ext.runner == "Flatpak" {
            flatpak_count += 1;
        } else if ext.runner == "Nix" {
            nix_count += 1;
        }
    }

    let card_inner_w = 58;
    let (tot_delta_str, tot_delta_color) = format_delta_text(tot_delta);

    let print_card_line = |label: &str, val_str: &str, colored_val: String| {
        let label_w = str_width(label);
        let val_w = str_width(val_str);
        let used = label_w + val_w;
        let pad = if card_inner_w >= used + 2 {
            card_inner_w - used - 2
        } else {
            0
        };
        println!("  │ {}{}{} │", label, " ".repeat(pad), colored_val);
    };

    println!("\n  ┌{}┐", "─".repeat(card_inner_w));
    let total_all = updates.len() + external_updates.len();
    let pkg_summary = format!(
        "{} ({} custom, {} default)",
        total_all,
        custom_count,
        default_count + external_updates.len()
    );
    print_card_line(
        "📦 Packages to Upgrade : ",
        &pkg_summary,
        pkg_summary.bold().to_string(),
    );

    if pacman_count > 0 {
        let s = format!("{} updates", pacman_count);
        print_card_line(
            "  • Pacman             : ",
            &s,
            s.cyan().to_string(),
        );
    }

    if aur_count > 0 {
        let helper_title = match helper.to_lowercase().as_str() {
            "paru" => "Paru",
            "yay" => "Yay",
            _ => helper,
        };
        let label = format!("  • {:<18} : ", helper_title);
        let s = format!("{} updates", aur_count);
        print_card_line(
            &label,
            &s,
            s.yellow().to_string(),
        );
    }

    if flatpak_count > 0 {
        let s = format!("{} updates", flatpak_count);
        print_card_line(
            "  • Flatpak            : ",
            &s,
            s.blue().to_string(),
        );
    }

    if nix_count > 0 {
        let s = format!("{} updates", nix_count);
        print_card_line(
            "  • Nix                : ",
            &s,
            s.magenta().to_string(),
        );
    }

    if !held.is_empty() {
        let held_summary = format!("{} packages", held.len());
        print_card_line(
            "⏸ Packages Held Back  : ",
            &held_summary,
            held_summary.yellow().to_string(),
        );
    }

    let dl_summary = format_size(tot_csize);
    print_card_line(
        "📥 Total Download Size : ",
        &dl_summary,
        dl_summary.cyan().to_string(),
    );

    let inst_summary = format_size(tot_isize);
    print_card_line(
        "💾 Total Install Size  : ",
        &inst_summary,
        inst_summary.dimmed().to_string(),
    );

    print_card_line(
        "📊 Net Space Delta     : ",
        &tot_delta_str,
        tot_delta_str.color(tot_delta_color).to_string(),
    );
    println!("  └{}┘\n", "─".repeat(card_inner_w));

    updates.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_card_line_alignment() {
        let card_inner_w = 58;
        let test_cases = vec![
            ("📦 Packages to Upgrade : ", "1 (0 custom, 1 default)"),
            ("⏸ Packages Held Back  : ", "1 packages"),
            ("📥 Total Download Size : ", "2.30 MiB"),
            ("💾 Total Install Size  : ", "7.60 MiB"),
            ("📊 Net Space Delta     : ", "+601.3 KiB"),
        ];

        let top_border = format!("  ┌{}┐", "─".repeat(card_inner_w));
        let expected_width = str_width(&top_border);

        for (label, val) in test_cases {
            let label_w = str_width(label);
            let val_w = str_width(val);
            let used = label_w + val_w;
            let pad = card_inner_w - used - 2;
            let line = format!("  │ {}{}{} │", label, " ".repeat(pad), val);
            assert_eq!(
                str_width(&line),
                expected_width,
                "Line '{}' visual width mismatch: got {}, expected {}",
                label,
                str_width(&line),
                expected_width
            );
        }
    }

    #[test]
    fn test_parse_selection_indices() {
        let items = vec!["pkgA".to_string(), "pkgB".to_string(), "pkgC".to_string(), "pkgD".to_string()];
        
        assert_eq!(parse_selection_indices("", &items), items);
        assert_eq!(parse_selection_indices("all", &items), items);
        assert_eq!(parse_selection_indices("ALL", &items), items);
        assert_eq!(parse_selection_indices("none", &items), Vec::<String>::new());
        assert_eq!(parse_selection_indices("^", &items), Vec::<String>::new());

        assert_eq!(parse_selection_indices("1 3", &items), vec!["pkgA", "pkgC"]);
        assert_eq!(parse_selection_indices("1-2", &items), vec!["pkgA", "pkgB"]);
        assert_eq!(parse_selection_indices("2-4", &items), vec!["pkgB", "pkgC", "pkgD"]);
        assert_eq!(parse_selection_indices("^2", &items), vec!["pkgA", "pkgC", "pkgD"]);
    }
}



