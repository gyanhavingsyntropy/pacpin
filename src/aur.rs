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
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct AurItem {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "PackageBase", default)]
    pub package_base: Option<String>,
    #[serde(rename = "Version")]
    pub version: String,
    #[serde(rename = "Description")]
    pub description: Option<String>,
    #[serde(rename = "LastModified", default)]
    pub last_modified: Option<i64>,
    #[serde(rename = "Depends", default)]
    pub depends: Vec<String>,
    #[serde(rename = "MakeDepends", default)]
    pub make_depends: Vec<String>,
    #[serde(rename = "Conflicts", default)]
    pub conflicts: Vec<String>,
}

pub fn clean_dep_name(dep: &str) -> &str {
    dep.split(['>', '<', '=', ':']).next().unwrap_or(dep).trim()
}

#[derive(Debug, Deserialize)]
struct AurResponse {
    results: Vec<AurItem>,
}

pub fn query_aur(pkg_names: &[String]) -> HashMap<String, AurItem> {
    let mut results = HashMap::new();
    if pkg_names.is_empty() {
        return results;
    }
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(6))
        .user_agent("pacpin/3.1 (GPLv3)")
        .build();

    for chunk in pkg_names.chunks(50) {
        let params: Vec<String> = chunk
            .iter()
            .map(|name| format!("arg[]={}", urlencoding(name)))
            .collect();
        let query = params.join("&");
        let url = format!("https://aur.archlinux.org/rpc/v5/info?{}", query);

        let mut attempts = 0;
        let max_attempts = 3;
        let mut success = false;

        while attempts < max_attempts && !success {
            attempts += 1;
            if attempts > 1 {
                std::thread::sleep(std::time::Duration::from_millis(400 * attempts as u64));
            }

            match agent.get(&url).call() {
                Ok(resp) => match resp.into_json::<AurResponse>() {
                    Ok(data) => {
                        for item in data.results {
                            results.insert(item.name.clone(), item);
                        }
                        success = true;
                    }
                    Err(e) => {
                        if attempts == max_attempts {
                            eprintln!(
                                "{}",
                                format!(":: Warning: Failed to parse AUR RPC response after {} attempts: {}", attempts, e).yellow()
                            );
                        }
                    }
                },
                Err(e) => {
                    if attempts == max_attempts {
                        eprintln!(
                            "{}",
                            format!(
                                ":: Warning: AUR RPC query failed for {} package(s) after {} attempts: {}",
                                chunk.len(),
                                attempts,
                                e
                            )
                            .yellow()
                        );
                    }
                }
            }
        }
    }

    results
}

fn urlencoding(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_dep_name() {
        assert_eq!(clean_dep_name("qt6-base>=6.5"), "qt6-base");
        assert_eq!(clean_dep_name("openssl=3.0"), "openssl");
        assert_eq!(clean_dep_name("libzip<1.9"), "libzip");
        assert_eq!(clean_dep_name("glibc:2.35"), "glibc");
        assert_eq!(clean_dep_name("ripgrep"), "ripgrep");
        assert_eq!(clean_dep_name("  python>=3.11  "), "python");
    }

    #[test]
    fn test_aur_item_deserialize() {
        let json = r#"{
            "Name": "mcpelauncher-ui",
            "PackageBase": "mcpelauncher",
            "Version": "1.0.0",
            "Description": "Minecraft Bedrock launcher UI",
            "LastModified": 1700000000,
            "Depends": ["mcpelauncher-linux", "qt6-base>=6.5"],
            "MakeDepends": ["cmake"],
            "Conflicts": ["mcpelauncher-git"]
        }"#;
        let item: AurItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.name, "mcpelauncher-ui");
        assert_eq!(item.package_base.as_deref(), Some("mcpelauncher"));
        assert_eq!(item.depends, vec!["mcpelauncher-linux", "qt6-base>=6.5"]);
        assert_eq!(item.make_depends, vec!["cmake"]);
        assert_eq!(item.conflicts, vec!["mcpelauncher-git"]);
    }
}
