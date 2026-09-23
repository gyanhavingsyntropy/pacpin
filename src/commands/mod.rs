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

pub mod check;
pub mod delays;
pub mod history;
pub mod maintenance;
pub mod orphans;
pub mod pins;
pub mod query;
pub mod upgrade;

use std::env;
use std::process::exit;

use colored::Colorize;

use crate::pm;

pub use check::cmd_check;
pub use delays::{cmd_delay, cmd_undelay};
pub use history::cmd_history;
pub use maintenance::{cmd_clean, cmd_remove, cmd_reset};
pub use orphans::cmd_orphans;
pub use pins::{cmd_list, cmd_pin, cmd_repos, cmd_unpin};
pub use query::{cmd_info, cmd_print_uris, cmd_search, cmd_sync_groups, cmd_sync_list};
pub use upgrade::{cmd_install, cmd_upgrade, parse_install_flags};

pub fn check_pacman_lock() {
    let args: Vec<String> = env::args().collect();
    let mut wait_secs = None;
    for (i, a) in args.iter().enumerate() {
        if a == "--wait" {
            if let Some(next) = args.get(i + 1) {
                if let Ok(s) = next.parse::<u64>() {
                    wait_secs = Some(s);
                    break;
                }
            }
            wait_secs = Some(600);
            break;
        } else if let Some(rest) = a.strip_prefix("--wait=") {
            if let Ok(s) = rest.parse::<u64>() {
                wait_secs = Some(s);
                break;
            }
        }
    }

    let res = if let Some(s) = wait_secs {
        pm::wait_for_lock(s)
    } else {
        pm::check_lock()
    };

    if let Err(e) = res {
        eprintln!("{}", format!("Error: {}", e).red());
        exit(1);
    }
}
