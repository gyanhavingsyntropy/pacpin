//! One resolved pacman.conf snapshot per invocation, shared by read-only callers.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
#[derive(Debug, Default)]
pub struct Snapshot { options: BTreeMap<String, Vec<String>>, repositories: BTreeMap<String, BTreeMap<String, Vec<String>>>, pub order: Vec<String> }
impl Snapshot {
    pub fn option(&self, name: &str) -> &[String] { self.options.get(name).map(Vec::as_slice).unwrap_or(&[]) }
    pub fn servers(&self, repo: &str) -> Vec<String> { self.repositories.get(repo).and_then(|r| r.get("Server")).cloned().unwrap_or_default() }
}
static SNAPSHOT: Mutex<Option<Result<Arc<Snapshot>, String>>> = Mutex::new(None);
pub fn invalidate() { *SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner()) = None; }
pub fn snapshot() -> Result<Arc<Snapshot>, String> {
    let mut cached = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
    cached.get_or_insert_with(|| {
        let output = crate::execution::capture_output_with_limit(&mut std::process::Command::new("pacman-conf"), 10 * 1024 * 1024, std::time::Duration::from_secs(30)).map_err(|e| e.to_string())?;
        if !output.status.success() { return Err("pacman-conf failed to resolve native configuration".into()); }
        let body = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
        parse(&body).map(Arc::new)
    }).clone()
}
fn parse(body: &str) -> Result<Snapshot, String> {
    let mut snapshot = Snapshot::default();
    let mut section = String::new();
    for line in body.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = name.into();
            if section != "options" && !snapshot.order.contains(&section) { snapshot.order.push(section.clone()); }
        } else if let Some((key, value)) = line.split_once(" = ") {
            let options = if section == "options" { &mut snapshot.options } else { snapshot.repositories.entry(section.clone()).or_default() };
            options.entry(key.into()).or_default().push(value.into());
        }
    }
    for key in ["RootDir", "DBPath"] {
        if snapshot.option(key).len() != 1 || !std::path::Path::new(&snapshot.option(key)[0]).is_absolute() {
            return Err(format!("Native configuration requires one absolute {key}"));
        }
    }
    Ok(snapshot)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolved_snapshot_retains_repeated_values_and_repository_order() {
        let snapshot = parse("[options]\nRootDir = /\nDBPath = /var/lib/pacman/\nIgnorePkg = held\nIgnorePkg = more\nCacheDir = /cache/a\nCacheDir = /cache/b\nCheckSpace\n[second]\nServer = https://second\n[first]\nServer = https://first\n").unwrap();
        assert_eq!(snapshot.order, ["second", "first"]);
        assert_eq!(snapshot.option("IgnorePkg"), ["held", "more"]);
        assert_eq!(snapshot.option("CacheDir").len(), 2);
        assert_eq!(snapshot.servers("first"), ["https://first"]);
        assert!(parse("[options]\nRootDir = /\nDBPath = relative\n").is_err());
    }
}
