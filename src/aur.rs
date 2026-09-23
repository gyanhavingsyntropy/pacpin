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

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct AurRpcResponse {
    #[serde(rename = "version", default)]
    pub version: Option<u32>,
    #[serde(rename = "type", default)]
    pub response_type: Option<String>,
    #[serde(rename = "resultcount", default)]
    pub resultcount: Option<u32>,
    #[serde(default)]
    pub results: Vec<AurItem>,
    #[serde(default)]
    pub error: Option<String>,
}

fn query_aur_chunk(chunk: &[String]) -> HashMap<String, AurItem> {
    let mut results = HashMap::new();
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(6))
        .user_agent("pacpin/3.1 (GPLv3)")
        .build();

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
            Ok(resp) => {
                let text = match resp.into_string() {
                    Ok(t) => t,
                    Err(e) => {
                        if attempts == max_attempts {
                            eprintln!(
                                "{}",
                                format!(":: Warning: Failed to read AUR RPC response: {}", e).yellow()
                            );
                        }
                        continue;
                    }
                };

                match serde_json::from_str::<AurRpcResponse>(&text) {
                    Ok(rpc) => {
                        if let Some(ref err_msg) = rpc.error {
                            if attempts == max_attempts {
                                eprintln!(
                                    "{}",
                                    format!(":: Warning: AUR RPC returned error: {}", err_msg).yellow()
                                );
                            }
                        } else {
                            for item in rpc.results {
                                results.insert(item.name.clone(), item);
                            }
                            success = true;
                        }
                    }
                    Err(e) => {
                        if attempts == max_attempts {
                            eprintln!(
                                "{}",
                                format!(
                                    ":: Warning: Failed to parse AUR RPC response JSON (attempt {}/{}): {}",
                                    attempts, max_attempts, e
                                )
                                .yellow()
                            );
                        }
                    }
                }
            }
            Err(ureq::Error::Status(code, _resp)) => {
                let desc = match code {
                    429 => "Rate limited (HTTP 429). Too many requests to AUR RPC",
                    500 => "Internal server error (HTTP 500)",
                    502 => "Bad gateway (HTTP 502). AUR RPC server upstream is unreachable",
                    503 => "Service unavailable (HTTP 503). AUR is temporarily undergoing maintenance",
                    504 => "Gateway timeout (HTTP 504)",
                    _ => "HTTP error response",
                };
                if attempts == max_attempts {
                    eprintln!(
                        "{}",
                        format!(
                            ":: Warning: AUR RPC query failed (HTTP {}): {} for {} package(s)",
                            code, desc, chunk.len()
                        )
                        .yellow()
                    );
                }
                if code == 429 {
                    std::thread::sleep(std::time::Duration::from_millis(1500 * attempts as u64));
                }
            }
            Err(ureq::Error::Transport(transport_err)) => {
                if attempts == max_attempts {
                    eprintln!(
                        "{}",
                        format!(
                            ":: Warning: AUR RPC network error after {} attempts: {}",
                            attempts, transport_err
                        )
                        .yellow()
                    );
                }
            }
        }
    }

    results
}

pub fn query_aur(pkg_names: &[String]) -> HashMap<String, AurItem> {
    if pkg_names.is_empty() {
        return HashMap::new();
    }

    if pkg_names.len() <= 50 {
        return query_aur_chunk(pkg_names);
    }

    // Query multiple 50-package chunks concurrently in parallel
    let chunks: Vec<Vec<String>> = pkg_names.chunks(50).map(|c| c.to_vec()).collect();
    let mut handles = Vec::new();

    for chunk in chunks {
        handles.push(std::thread::spawn(move || query_aur_chunk(&chunk)));
    }

    let mut results = HashMap::new();
    for h in handles {
        if let Ok(chunk_res) = h.join() {
            results.extend(chunk_res);
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

    #[test]
    fn test_aur_error_response_deserialize() {
        let json = r#"{
            "version": 5,
            "type": "error",
            "error": "Rate limit exceeded"
        }"#;
        let rpc: AurRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(rpc.response_type.as_deref(), Some("error"));
        assert_eq!(rpc.error.as_deref(), Some("Rate limit exceeded"));
        assert!(rpc.results.is_empty());
    }

    #[test]
    fn test_aur_multiinfo_empty_deserialize() {
        let json = r#"{
            "version": 5,
            "type": "multiinfo",
            "resultcount": 0,
            "results": []
        }"#;
        let rpc: AurRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(rpc.response_type.as_deref(), Some("multiinfo"));
        assert_eq!(rpc.resultcount, Some(0));
        assert!(rpc.results.is_empty());
        assert!(rpc.error.is_none());
    }
}
