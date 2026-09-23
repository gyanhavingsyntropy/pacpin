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

use crate::journal::TransactionJournal;
use crate::ui::print_banner;

pub fn cmd_history(tx_id: Option<usize>) {
    print_banner();
    if let Some(id) = tx_id {
        let tx = match TransactionJournal::get_transaction(id) {
            Some(t) => t,
            None => {
                eprintln!(
                    "{}",
                    format!("Error: Transaction #{} not found in history.", id).red()
                );
                return;
            }
        };

        println!(
            "\n{} ({} — Action: {})",
            format!("Transaction #{}", tx.id).bold(),
            tx.timestamp,
            tx.action.green()
        );
        println!("Command: {}\n", tx.command.dimmed());

        println!(
            "{}",
            format!("Packages Involved ({}):", tx.packages.len()).bold()
        );
        for p in &tx.packages {
            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
            let from_repo = p.get("from_repo").and_then(|v| v.as_str()).unwrap_or("?");
            let from_ver = p
                .get("from_version")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let to_repo = p.get("to_repo").and_then(|v| v.as_str()).unwrap_or("?");
            let to_ver = p.get("to_version").and_then(|v| v.as_str()).unwrap_or("?");
            let state = p.get("state").and_then(|v| v.as_str()).unwrap_or("default");
            let state_badge = match state {
                "custom" => "(custom)".cyan(),
                "sticky" => "(sticky)".blue(),
                _ => "(default)".dimmed(),
            };

            if from_ver != "?" && to_ver != "?" {
                println!(
                    "  • {:<28} : [{}] {} ➔ [{}] {} {}",
                    name.bold(),
                    from_repo,
                    from_ver,
                    to_repo,
                    to_ver,
                    state_badge
                );
            } else if let Some(restored) = p.get("restored_version").and_then(|v| v.as_str()) {
                println!("  • {:<28} : restored to {}", name.bold(), restored.green());
            } else {
                let target = p.get("target").and_then(|v| v.as_str()).unwrap_or(name);
                println!("  • {:<28} : {}", name.bold(), target);
            }
        }
        println!();
        return;
    }

    let txs = TransactionJournal::list_transactions(15);
    if txs.is_empty() {
        println!(
            "\n{}",
            "No transactions recorded yet in history.jsonl.".yellow()
        );
        return;
    }

    println!(
        "\n{}",
        format!("Transaction Journal (Last {}):", txs.len()).bold()
    );
    println!(
        "  {:<5} {:<20} {:<10} {:<10} SUMMARY",
        "ID", "TIMESTAMP", "ACTION", "PACKAGES"
    );
    println!(
        "  {:<5} {:<20} {:<10} {:<10} ─────────────────────────",
        "─────", "────────────────────", "──────────", "──────────"
    );

    for tx in &txs {
        let mut repos = Vec::new();
        for p in &tx.packages {
            if let Some(r) = p.get("to_repo").and_then(|v| v.as_str()) {
                if !r.is_empty() && r != "unknown" && !repos.contains(&r) {
                    repos.push(r);
                }
            }
        }
        repos.sort();
        let repo_str = if !repos.is_empty() {
            format!("[{}]", repos.join(", "))
        } else {
            String::new()
        };
        println!(
            "  {:<5} {:<20} {:<10} {:<10} {}",
            tx.id, tx.timestamp, tx.action, tx.total_packages, repo_str
        );
    }

    println!(
        "\n{}",
        "Use 'pacpin history <id>' to inspect full package diff.".dimmed()
    );
    println!(
        "{}",
        "Use 'pacpin rollback [id]' to restore previous package versions.".dimmed()
    );
    println!();
}
