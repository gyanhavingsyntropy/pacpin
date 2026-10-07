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
use std::collections::{HashMap, HashSet};
use std::io::Read;

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

fn parse_aur_rpc_response(body: &str) -> Result<Vec<AurItem>, String> {
    let rpc: AurRpcResponse = serde_json::from_str(body)
        .map_err(|e| format!("Invalid AUR RPC JSON: {}", e))?;
    if let Some(error) = rpc.error {
        return Err(format!("AUR RPC returned error: {}", crate::utils::sanitize_display_text(&error)));
    }
    if rpc.version != Some(5) || rpc.response_type.as_deref() != Some("multiinfo") {
        return Err("Unexpected AUR RPC response version or type".to_string());
    }
    if rpc.resultcount != Some(rpc.results.len() as u32) {
        return Err("AUR RPC result count does not match response data".to_string());
    }
    let mut items = rpc.results;
    let mut names = HashSet::new();
    for item in &mut items {
        // Identity and version fields must be rejected, not rewritten: stripping
        // controls could make a malicious result alias a different package.
        if !crate::utils::is_safe_pkg_name(&item.name)
            || !crate::utils::is_safe_version(&item.version)
            || item.package_base.as_deref().is_some_and(|base| !crate::utils::is_safe_pkg_name(base))
            || !names.insert(item.name.clone())
            || item.depends.iter().chain(&item.make_depends).chain(&item.conflicts)
                .any(|dep| dep.chars().any(char::is_control))
        {
            return Err("Invalid or duplicate AUR package metadata".into());
        }
        item.description = item.description.as_deref().map(crate::utils::sanitize_display_text);
    }
    Ok(items)
}

fn read_rpc_body(reader: impl Read, limit: u64) -> Result<String, String> {
    let mut text = String::new();
    reader.take(limit + 1).read_to_string(&mut text).map_err(|e| e.to_string())?;
    if text.len() as u64 > limit {
        return Err("AUR RPC response exceeds the size limit".into());
    }
    Ok(text)
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
                const MAX_AUR_RESPONSE_BYTES: u64 = 10 * 1024 * 1024; // 10 MB safety cap
                let text = match read_rpc_body(resp.into_reader(), MAX_AUR_RESPONSE_BYTES) {
                    Ok(text) => text,
                    Err(e) => {
                        if attempts == max_attempts {
                            eprintln!("{}", format!(":: Warning: Failed to read AUR RPC response: {}", e).yellow());
                        }
                        continue;
                    }
                };

                match parse_requested_aur_response(&text, chunk) {
                    Ok(items) => {
                        for item in items {
                            results.insert(item.name.clone(), item);
                        }
                        success = true;
                    }
                    Err(e) => {
                        if attempts == max_attempts {
                            eprintln!(
                                "{}",
                                format!(
                                    ":: Warning: AUR RPC response rejected (attempt {}/{}): {}",
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

fn parse_requested_aur_response(body: &str, requested: &[String]) -> Result<Vec<AurItem>, String> {
    let items = parse_aur_rpc_response(body)?;
    if items.iter().any(|item| !requested.contains(&item.name)) {
        return Err("AUR RPC returned a package that was not requested".into());
    }
    Ok(items)
}

pub fn query_aur(pkg_names: &[String]) -> HashMap<String, AurItem> {
    if pkg_names.is_empty() {
        return HashMap::new();
    }

    if pkg_names.len() <= 50 {
        return query_aur_chunk(pkg_names);
    }

    // Query 50-package chunks concurrently with bounded worker concurrency (max 4 parallel connections)
    let chunks: Vec<Vec<String>> = pkg_names.chunks(50).map(|c| c.to_vec()).collect();
    let mut results = HashMap::new();
    const MAX_CONCURRENT_AUR_WORKERS: usize = 4;

    for chunk_batch in chunks.chunks(MAX_CONCURRENT_AUR_WORKERS) {
        let mut handles = Vec::new();
        for chunk in chunk_batch {
            let chunk = chunk.clone();
            handles.push(std::thread::spawn(move || query_aur_chunk(&chunk)));
        }
        for h in handles {
            if let Ok(chunk_res) = h.join() {
                results.extend(chunk_res);
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
    fn rpc_metadata_cannot_inject_terminal_controls_or_alias_names() {
        let envelope = |name: &str, version: &str| serde_json::json!({
            "version": 5, "type": "multiinfo", "resultcount": 1,
            "results": [{"Name": name, "Version": version, "Description": "safe\u{1b}[2Jtext"}]
        }).to_string();
        let valid = envelope("test-pkg", "1:2.0.r123-1");
        let items = parse_requested_aur_response(&valid, &["test-pkg".into()]).unwrap();
        assert_eq!(items[0].description.as_deref(), Some("safetext"));
        assert!(parse_aur_rpc_response(&envelope("test\u{1b}[2J-pkg", "1.0")).is_err());
        assert!(parse_aur_rpc_response(&envelope("test-pkg", "1.0\u{1b}]0;spoof\u{7}")).is_err());
        assert!(parse_requested_aur_response(&valid, &["another-pkg".into()]).is_err());
        let error = serde_json::json!({"error": "bad\u{1b}[2Jresponse"}).to_string();
        assert!(!parse_aur_rpc_response(&error).unwrap_err().contains('\u{1b}'));
        let mut duplicate: serde_json::Value = serde_json::from_str(&valid).unwrap();
        let item = duplicate["results"][0].clone();
        duplicate["results"].as_array_mut().unwrap().push(item);
        duplicate["resultcount"] = 2.into();
        assert!(parse_aur_rpc_response(&duplicate.to_string()).is_err());
    }

    #[test]
    fn rpc_size_cap_rejects_truncation_instead_of_accepting_a_prefix() {
        assert_eq!(read_rpc_body(&b"{}"[..], 2).unwrap(), "{}");
        assert!(read_rpc_body(&b"{}garbage"[..], 2).is_err());
        assert!(read_rpc_body(&b"\xff"[..], 2).is_err());
    }

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

    #[test]
    fn test_aur_rpc_rejects_malformed_and_inconsistent_responses() {
        assert!(parse_aur_rpc_response("not json").is_err());
        assert!(parse_aur_rpc_response("{}").is_err());
        assert!(parse_aur_rpc_response(r#"{"version":5,"type":"error","error":"rate limited"}"#).is_err());
        assert!(parse_aur_rpc_response(r#"{"version":5,"type":"multiinfo","resultcount":1,"results":[]}"#).is_err());
        assert!(parse_aur_rpc_response(r#"{"version":5,"type":"multiinfo","resultcount":0,"results":[]}"#).unwrap().is_empty());
    }
}
