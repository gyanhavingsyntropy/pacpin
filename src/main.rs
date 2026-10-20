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

mod alias;
mod archive;
mod aur;
pub mod commands;
mod config;
mod db;
mod discovery;
mod download;
mod hooks;
pub mod pm;
mod integrations;
mod manager_config;
mod pacman_config;
mod journal;
mod orphans;
mod preview;
mod privilege;
mod progress;
mod repo_menu;
mod resolver;
mod restart;
mod runtime;
mod sandbox;
mod scratch;
mod sources;
mod tui_select;
mod ui;
mod utils;
mod wizard;

mod cli;
mod execution;

pub use commands::upgrade::parse_install_flags;
pub use commands::pins::find_matching_pin;
pub(crate) use commands::pins::check_and_prompt_smart_unpin;
pub use utils::is_command_available;

fn main() {
    cli::run();
}
