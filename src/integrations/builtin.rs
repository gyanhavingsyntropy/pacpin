//! Manager-specific discovery and verification. No cross-model fallback.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
type Environment = BTreeMap<String, String>;
fn command(executable: &str, env: &Environment) -> Command {
    let mut command = Command::new(executable);
    command.envs(env);
    if executable == "flatpak" {
        // The shared footer owns cursor positioning. Flatpak's cursor queries
        // otherwise send terminal replies into the inherited input stream.
        command.env("FLATPAK_FANCY_OUTPUT", "0");
    }
    command
}

pub(super) fn capture(command: &mut Command) -> io::Result<std::process::Output> {
    command.env("LC_ALL", "C");
    let output = crate::execution::capture_output_with_limit(command, MAX_METADATA_BYTES as usize, std::time::Duration::from_secs(120))?;
    if !output.status.success() {
        let diagnostic = crate::utils::sanitize_display_text(&String::from_utf8_lossy(&output.stderr));
        return Err(io::Error::other(format!("Metadata command exited with {}: {}", output.status, diagnostic.chars().take(512).collect::<String>())));
    }
    Ok(output)
}
fn json(command: &mut Command) -> io::Result<serde_json::Value> {
    serde_json::from_slice(&capture(command)?.stdout).map_err(io::Error::other)
}
fn text(value: &str) -> io::Result<String> {
    if value.is_empty() || value.len() > 4096 || value.starts_with('-') || value.chars().any(char::is_control) {
        return Err(io::Error::other("Invalid manager metadata field"));
    }
    Ok(value.into())
}
fn field(value: &serde_json::Value, name: &str) -> io::Result<String> {
    text(value.get(name).and_then(|v| v.as_str()).ok_or_else(|| io::Error::other(format!("Missing metadata field '{name}'")))?)
}
pub(super) fn check(adapter: &str, env: &Environment) -> io::Result<Vec<ExternalUpdate>> {
    match adapter { "flatpak" => flatpak(env), "nix" => nix(env), "pipx" => pipx(env), _ => Err(io::Error::other("Unknown adapter")) }
}
fn flatpak_scope(installation: &str) -> io::Result<String> {
    match installation { "user" => Ok("--user".into()), "system" | "default" => Ok("--system".into()), name => Ok(format!("--installation={}", text(name)?)) }
}
fn valid_ref(reference: &str) -> bool {
    let parts: Vec<_> = reference.split('/').collect();
    parts.len() == 4 && matches!(parts[0], "app" | "runtime") && parts.iter().all(|p| !p.is_empty()
        && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
}
fn flatpak(env: &Environment) -> io::Result<Vec<ExternalUpdate>> {
    // Environment overrides can select a different installation inventory.
    let cache = if env.is_empty() { flatpak_cache_file() } else { None };
    if let Some(updates) = cache.as_ref().and_then(|path| read_flatpak_cache(path)) {
        if updates.iter().all(|u| valid_ref(&u.id) && u.scope.as_deref().is_some_and(|s| matches!(s, "--user" | "--system") || s.strip_prefix("--installation=").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))) && u.revision.as_ref().is_some_and(|r| r.len() == 64 && r.bytes().all(|c| c.is_ascii_hexdigit()))) {
            return Ok(updates);
        }
    }
    let installations = capture(command("flatpak", env).args(["list", "--columns=installation"]))?;
    let scopes: BTreeSet<_> = String::from_utf8(installations.stdout).map_err(io::Error::other)?.lines()
        .filter(|line| !line.trim().is_empty()).map(|line| flatpak_scope(line.trim())).collect::<io::Result<_>>()?;
    let mut updates = Vec::new();
    for scope in scopes {
        let output = capture(command("flatpak", env).args(["remote-ls", &scope, "--updates", "--columns=name,ref,version,origin,commit"]))?;
        for line in String::from_utf8(output.stdout).map_err(io::Error::other)?.lines().filter(|line| !line.trim().is_empty()) {
            let columns: Vec<_> = line.split('\t').collect();
            if columns.len() != 5 || !valid_ref(columns[1]) || !matches!(columns[4].len(), 12 | 64) || !columns[4].bytes().all(|c| c.is_ascii_hexdigit()) {
                return Err(io::Error::other("Invalid Flatpak update metadata (expected full ref and commit)"));
            }
            // remote-ls abbreviates commit hashes even with :full columns.
            let revision = if columns[4].len() == 64 { columns[4].to_string() } else {
                let origin = text(columns[3])?;
                let output = capture(command("flatpak", env).args(["remote-info", &scope, "--show-commit", "--", &origin, columns[1]]))?;
                let full = String::from_utf8(output.stdout).map_err(io::Error::other)?.trim().to_string();
                if full.len() != 64 || !full.bytes().all(|c| c.is_ascii_hexdigit()) || !full.starts_with(columns[4]) {
                    return Err(io::Error::other("Flatpak remote commit changed or could not be resolved; check again before upgrading"));
                }
                full
            };
            updates.push(ExternalUpdate { runner: "Flatpak".into(), id: columns[1].into(),
                name: crate::utils::sanitize_display_text(columns[0]), repo: format!("{} / {}", crate::utils::sanitize_display_text(columns[3]), scope.trim_start_matches("--")),
                version: if columns[2].is_empty() { columns[4][..12].into() } else { text(columns[2])? },
                revision: Some(revision), scope: Some(scope.clone()), ..ExternalUpdate::default() });
        }
    }
    if let (Some(path), Ok(body)) = (cache, serde_json::to_vec(&updates)) { let _ = write_flatpak_cache(&path, &body); }
    Ok(updates)
}

fn nix_manifest(env: &Environment) -> io::Result<BTreeMap<String, serde_json::Value>> {
    let manifest = json(command("nix", env).args(["profile", "list", "--json"]))?;
    let elements = manifest.get("elements").and_then(|v| v.as_object())
        .ok_or_else(|| io::Error::other("Nix integration requires a modern named flake profile; use a command adapter for nix-env"))?;
    Ok(elements.iter().map(|(name, value)| (name.clone(), value.clone())).collect())
}
fn nix_package(url: &str, attribute: &str, env: &Environment) -> io::Result<serde_json::Value> {
    json(command("nix", env).args(["eval", "--json", &format!("{url}#{attribute}"), "--apply", "p: { version = p.version or p.name; path = p.outPath; }"]))
}
fn nix(env: &Environment) -> io::Result<Vec<ExternalUpdate>> {
    let mut updates = Vec::new();
    let mut resolutions: BTreeMap<String, String> = BTreeMap::new();
    for (name, element) in nix_manifest(env)? {
        if element.get("active").and_then(|v| v.as_bool()) == Some(false) { continue; }
        if element.get("originalUrl").is_none() {
            return Err(io::Error::other(format!("Nix: {} has no flake source; automatic upgrade is unsupported. Use a command adapter for this profile.", crate::utils::sanitize_display_text(&name))));
        }
        let original = field(&element, "originalUrl")?;
        let attribute = field(&element, "attrPath")?;
        let locked = if let Some(locked) = resolutions.get(&original) { locked.clone() } else {
            let metadata = json(command("nix", env).args(["flake", "metadata", "--json", "--refresh", &original]))?;
            let locked = field(&metadata, "url")?;
            resolutions.insert(original.clone(), locked.clone()); locked
        };
        let candidate = nix_package(&locked, &attribute, env)?;
        let path = field(&candidate, "path")?;
        if element.get("storePaths").and_then(|v| v.as_array()).is_some_and(|paths| paths.iter().any(|p| p.as_str() == Some(&path))) { continue; }
        let current = nix_package(&field(&element, "url")?, &attribute, env)?;
        updates.push(ExternalUpdate { runner: "Nix".into(), id: text(&name)?, name,
            repo: original.clone(), version: field(&candidate, "version")?, installed_version: Some(field(&current, "version")?),
            revision: Some(locked), scope: Some(original) });
    }
    Ok(updates)
}

fn python_version_order(left: &str, right: &str) -> io::Result<std::cmp::Ordering> {
    let parse = |value: &str| value.parse::<pep440_rs::Version>()
        .map_err(|_| io::Error::other("Invalid PEP 440 version in Pipx/PyPI metadata"));
    Ok(parse(left)?.cmp(&parse(right)?))
}

fn pipx(env: &Environment) -> io::Result<Vec<ExternalUpdate>> {
    let inventory = json(command("pipx", env).args(["list", "--json"]))?;
    let venvs = inventory.get("venvs").and_then(|v| v.as_object()).ok_or_else(|| io::Error::other("Invalid Pipx inventory"))?;
    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(15)).build();
    let mut targets = Vec::new();
    for (id, value) in venvs {
        let main = value.get("metadata").and_then(|v| v.get("main_package")).ok_or_else(|| io::Error::other("Missing Pipx package metadata"))?;
        let package = field(main, "package")?;
        if main.get("package_or_url").and_then(|v| v.as_str()).is_some_and(|s| s.contains([':', '/', '\\', '@']))
            || main.get("editable").and_then(|v| v.as_bool()) == Some(true) {
            return Err(io::Error::other(format!("Pipx environment {id} uses a direct/local/editable source; use a command adapter rather than PyPI discovery")));
        }
        if !package.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) { return Err(io::Error::other("Invalid PyPI package name")); }
        targets.push((text(id)?, package, field(main, "package_version")?));
    }
    let mut results = Vec::new();
    for batch in targets.chunks(4) {
        std::thread::scope(|scope| -> io::Result<()> {
            let workers: Vec<_> = batch.iter().map(|(id, package, installed)| {
                let agent = agent.clone();
                scope.spawn(move || -> io::Result<Option<ExternalUpdate>> {
                    let response = agent.get(&format!("https://pypi.org/pypi/{package}/json")).call()
                        .map_err(|_| io::Error::other(format!("PyPI check failed for {package}")))?;
                    let version = pypi_version(response.into_reader(), MAX_METADATA_BYTES)?;
                    Ok(python_version_order(&version, installed)?.is_gt().then(|| ExternalUpdate { runner: "Pipx".into(), id: id.clone(), name: package.clone(), repo: "pypi".into(), version,
                        installed_version: Some(installed.clone()), ..ExternalUpdate::default() }))
                })
            }).collect();
            for worker in workers { if let Some(update) = worker.join().map_err(|_| io::Error::other("Pipx metadata worker failed"))?? { results.push(update); } }
            Ok(())
        })?;
    }
    Ok(results)
}

pub(super) fn defaults(adapter: &str) -> Vec<String> {
    let command: &[&str] = match adapter {
        "flatpak" => &["flatpak", "update", "-y", "{scope}", "--commit={revision}", "--", "{id}"],
        "nix" => &["nix", "profile", "upgrade", "--override-flake", "{scope}", "{revision}", "--", "{id}"],
        "pipx" => &["pipx", "upgrade", "--", "{id}"],
        _ => &[],
    };
    command.iter().map(|s| (*s).into()).collect()
}
pub(super) fn expand(command: &[String], update: &ExternalUpdate) -> io::Result<Vec<String>> {
    command.iter().map(|argument| {
        let mut expanded = String::new();
        let mut remainder = argument.as_str();
        while let Some(start) = remainder.find('{') {
            expanded.push_str(&remainder[..start]);
            let end = remainder[start..].find('}').ok_or_else(|| io::Error::other("Malformed command placeholder"))? + start;
            let key = &remainder[start..=end];
            let value = match key {
                "{id}" => Some(update.id.as_str()), "{version}" => Some(update.version.as_str()),
                "{revision}" => update.revision.as_deref(), "{scope}" => update.scope.as_deref(),
                _ => return Err(io::Error::other("Unknown command placeholder")),
            }.ok_or_else(|| io::Error::other(format!("Missing {key} in approved update")))?;
            expanded.push_str(value);
            remainder = &remainder[end+1..];
        }
        expanded.push_str(remainder);
        if expanded.chars().any(char::is_control) { return Err(io::Error::other("Control character in approved command argument")); }
        Ok(expanded)
    }).collect()
}
pub(super) fn display(command: &[String]) -> String { command.iter().map(|s| crate::utils::shell_quote(s)).collect::<Vec<_>>().join(" ") }
pub(super) fn preview(adapter: &str, updates: &[ExternalUpdate]) -> String {
    updates.iter().map(|u| expand(&defaults(adapter), u).map(|c| display(&c)).unwrap_or_else(|e| format!("Invalid plan: {e}"))).collect::<Vec<_>>().join(" && ")
}
pub(super) fn execute(adapter: &str, updates: &[ExternalUpdate]) -> io::Result<bool> {
    for update in updates {
        let arguments = expand(&defaults(adapter), update)?;
        let mut command = command(&arguments[0], &Environment::new()); command.args(&arguments[1..]);
        if !crate::progress::run(&mut command, crate::progress::integration_manager(&update.runner), "Upgrading packages", || {})?.success() { return Ok(false); }
        verify(adapter, update, &Environment::new())?;
    }
    Ok(true)
}
pub(super) fn verify(adapter: &str, update: &ExternalUpdate, env: &Environment) -> io::Result<()> {
    let correct = match adapter {
        "flatpak" => {
            let output = capture(command("flatpak", env).args(["info", update.scope.as_deref().ok_or_else(|| io::Error::other("Missing Flatpak installation"))?, "--show-commit", "--", &update.id]))?;
            String::from_utf8_lossy(&output.stdout).trim() == update.revision.as_deref().unwrap_or("")
        }
        "nix" => nix_manifest(env)?.get(&update.id).and_then(|v| v.get("url")).and_then(|v| v.as_str()) == update.revision.as_deref(),
        "pipx" => {
            let inventory = json(command("pipx", env).args(["list", "--json"]))?;
            let installed = inventory.get("venvs").and_then(|v| v.get(&update.id)).and_then(|v| v.get("metadata")).and_then(|v| v.get("main_package"))
                .and_then(|v| v.get("package_version")).and_then(|v| v.as_str())
                .ok_or_else(|| io::Error::other("Missing installed Pipx version after upgrade"))?;
            python_version_order(installed, &update.version)?.is_eq()
        }
        _ => true,
    };
    if !correct { return Err(io::Error::other(format!("{} installed result differs from approved update for {}", update.runner, update.id))); }
    Ok(())
}
pub(super) fn refresh(adapter: &str, env: &Environment) -> io::Result<()> {
    if adapter == "flatpak" {
        if let Some(cache) = flatpak_cache_file() { let _ = std::fs::remove_file(cache); }
        capture(command("flatpak", env).args(["update", "--appstream", "-y"]))?;
    }
    Ok(())
}
pub(super) fn clean_preview(adapter: &str) -> String {
    match adapter { "flatpak" => "flatpak uninstall --unused -y".into(), "nix" => "nix-collect-garbage".into(), _ => String::new() }
}
pub(super) fn clean(adapter: &str, env: &Environment) -> io::Result<bool> {
    let mut command = match adapter { "flatpak" => { let mut c = command("flatpak", env); c.args(["uninstall", "--unused", "-y"]); c }, "nix" => command("nix-collect-garbage", env), _ => return Ok(true) };
    Ok(crate::progress::run(&mut command, crate::progress::integration_manager(match adapter { "flatpak" => "Flatpak", _ => "Nix" }), "Cleaning unused packages", || {})?.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pipx_uses_python_version_order_and_normalized_equality() {
        for (new, old) in [("1.0", "1.0rc1"), ("1.0.post1", "1.0"), ("1!1.0", "99.0"), ("1.0a1", "1.0.dev1")] {
            assert!(python_version_order(new, old).unwrap().is_gt());
            assert!(python_version_order(old, new).unwrap().is_lt());
        }
        assert!(python_version_order("1.0", "1.0.0").unwrap().is_eq());
        assert!(python_version_order("1.0", "1.0+local").unwrap().is_lt());
        assert!(python_version_order("not-a-version", "1.0").is_err());
    }
    #[test]
    fn flatpak_cursor_control_is_disabled_for_shared_terminal() {
        let env = Environment::from([("FLATPAK_FANCY_OUTPUT".into(), "1".into())]);
        let cmd = command("flatpak", &env);
        assert!(cmd.get_envs().any(|(key, value)| key == "FLATPAK_FANCY_OUTPUT" && value == Some(std::ffi::OsStr::new("0"))));
    }
    #[test]
    fn scoped_flatpak_and_nix_commands_preserve_exact_targets() {
        let update = ExternalUpdate { id: "app/org.example.App/x86_64/stable".into(), version: "2".into(), revision: Some("abc".into()), scope: Some("--user".into()), ..ExternalUpdate::default() };
        let command = expand(&defaults("flatpak"), &update).unwrap();
        assert!(command.contains(&"--user".into())); assert!(command.contains(&"--commit=abc".into()));
        assert_eq!(command.last(), Some(&update.id));
        assert!(!defaults("nix").contains(&".*".into()));
        assert!(!defaults("pipx").contains(&"upgrade-all".into()));
        assert!(expand(&defaults("flatpak"), &ExternalUpdate::default()).is_err());
        let literal = ExternalUpdate { id: "literal{version};$(command)".into(), version: "2".into(), ..ExternalUpdate::default() };
        assert_eq!(expand(&["manager".into(), "{id}".into()], &literal).unwrap()[1], literal.id);
    }
}
