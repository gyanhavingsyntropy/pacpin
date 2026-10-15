//! Upstream release sources. Hosting adapters only supply metadata; installation
//! and ownership policy are shared. Native Arch repositories still use ALPM.
mod catalog;
mod direct;
mod install;
mod provider;
mod build;
mod git;
pub use build::BuildRecipe;
pub use git::Revision as GitRevision;

use crate::config::Config;
pub use provider::SourceProvider;
pub use install::clear_prefetched;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::path::{Component, Path};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Github,
    Gitlab,
    Manifest,
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub kind: Kind,
    /// API base for GitHub/GitLab, catalog URL for manifest, absolute catalog path for local.
    #[serde(default)]
    pub url: String,
    /// Explicit opt-in. Never discover tokens from unrelated credential stores.
    #[serde(default)]
    pub token_env: Option<String>,
    pub packages: BTreeMap<String, Recipe>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Archive,
    Binary,
    Native,
    Source,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Alpha,
    Beta,
    Prerelease,
}
fn stable() -> Channel {
    Channel::Stable
}
fn prefix() -> String {
    "v".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    /// Selection window: this many recent API pages, 20 entries per page.
    #[serde(default = "one_page")]
    pub release_pages: u32,
    #[serde(default)]
    pub project: String,
    #[serde(default = "stable")]
    pub channel: Channel,
    #[serde(default = "prefix")]
    pub version_prefix: String,
    #[serde(default)]
    pub tag_pattern: Option<String>,
    #[serde(default)]
    pub asset: String,
    pub format: Format,
    #[serde(default)]
    pub root: String,
    /// /usr/bin name -> relative path within the payload (binary format uses "artifact").
    #[serde(default)]
    pub binaries: BTreeMap<String, String>,
    #[serde(default)]
    pub depends: Vec<String>,
    /// Explicit checksum for a fixed release; otherwise required from catalog/API.
    #[serde(default)]
    pub sha256: Option<String>,
    /// Optional checksum-file asset (e.g. SHA256SUMS), useful for GitLab releases.
    #[serde(default)]
    pub checksum_asset: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    /// Explicit local build instructions; never acquired from remote metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildRecipe>,
}

fn one_page() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub sha256: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub tag: String,
    /// Semantic version independent of vendor-specific tag spelling.
    #[serde(default)]
    pub version: Option<String>,
    /// Native packages may have an Arch version differing from the upstream version.
    #[serde(default)]
    pub package_version: Option<String>,
    #[serde(default)]
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub alias: String,
    pub name: String,
    pub source: Source,
    pub recipe: Recipe,
    pub tag: String,
    pub version: String,
    pub asset: Asset,
    /// Immutable Git commit selected before approval, separate from SHA-256 assets.
    pub git: Option<GitRevision>,
    /// A direct target is retained only after its verified transaction succeeds.
    pub persist_source: bool,
}

pub fn safe_relative(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._+-".contains(&b))
        && Path::new(value)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}
fn safe_text(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_control)
}
pub fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn validate_config(config: &Config) -> Result<(), String> {
    let native = crate::db::AlpmManager::discover_repos();
    for (alias, source) in &config.sources {
        if !crate::utils::is_safe_pkg_name(alias) || alias == "aur" || native.contains(alias) {
            return Err(format!(
                "Source alias '{alias}' is invalid or shadows a native repository"
            ));
        }
        if source.kind == Kind::Local {
            if !Path::new(&source.url).is_absolute()
                || !safe_text(&source.url)
                || source.token_env.is_some()
            {
                return Err(format!(
                    "Local source '{alias}' needs an absolute manifest path and no token"
                ));
            }
        } else if !source.url.is_empty() {
            catalog::https_url(&source.url)?;
        } else if source.kind == Kind::Manifest {
            return Err(format!("Source '{alias}' needs a manifest URL"));
        }
        if let Some(env) = &source.token_env {
            if env.is_empty()
                || !env.bytes().enumerate().all(|(i, b)| {
                    b.is_ascii_alphabetic() || b == b'_' || (i > 0 && b.is_ascii_digit())
                })
            {
                return Err("Invalid token_env name".into());
            }
        }
        for (name, recipe) in &source.packages {
            if !(1..=20).contains(&recipe.release_pages) {
                return Err("release_pages must be between 1 and 20".into());
            }
            if !crate::utils::is_safe_pkg_name(name) || (!safe_text(&recipe.asset) && recipe.build.as_ref().is_none_or(|b| b.git.is_none())) {
                return Err(format!("Invalid package recipe in '{alias}'"));
            }
            if matches!(source.kind, Kind::Github | Kind::Gitlab) && !safe_relative(&recipe.project)
            {
                return Err(format!("'{alias}/{name}' needs a safe project path"));
            }
            if !recipe.root.is_empty() && !safe_relative(&recipe.root) {
                return Err(format!("Invalid payload root for '{name}'"));
            }
            if recipe.format != Format::Native && recipe.binaries.is_empty() {
                return Err(format!("'{name}' needs at least one declared binary"));
            }
            build::validate(recipe, source)?;
            for (binary, path) in &recipe.binaries {
                if !crate::utils::is_safe_pkg_name(binary) || !safe_relative(path) {
                    return Err(format!("Invalid binary mapping for '{name}'"));
                }
            }
            if recipe
                .depends
                .iter()
                .any(|d| !crate::utils::is_safe_pkg_name(d))
            {
                return Err(format!(
                    "Invalid dependency for '{name}' (use plain package names)"
                ));
            }
            if let Some(digest) = &recipe.sha256 {
                if !valid_digest(digest) || recipe.tag.is_none() {
                    return Err("An explicit sha256 requires a fixed tag and 64 hex digits".into());
                }
            }
            if recipe.tag.as_ref().is_some_and(|tag| !safe_text(tag)) {
                return Err("Invalid fixed tag".into());
            }
            if let Some(pattern) = &recipe.tag_pattern {
                glob::Pattern::new(pattern).map_err(|e| e.to_string())?;
            }
            if recipe
                .checksum_asset
                .as_ref()
                .is_some_and(|p| !safe_text(p) || glob::Pattern::new(p).is_err())
            {
                return Err("Invalid checksum asset pattern".into());
            }
        }
    }
    for (name, alias) in &config.pins {
        if let Some(source) = config.sources.get(alias) {
            if !source.packages.contains_key(name) {
                return Err(format!("Source pin '{alias}/{name}' has no exact recipe"));
            }
        }
    }
    Ok(())
}

/// Source ownership is independent of the optional native repo-priority feature.
pub fn owner<'a>(config: &'a Config, name: &str) -> Option<&'a str> {
    config
        .pins
        .get(name)
        .filter(|alias| config.sources.contains_key(*alias))
        .map(String::as_str)
}

pub fn plan(config: &Config, target: &str) -> Result<Plan, String> {
    let (alias, name) = target
        .split_once('/')
        .ok_or("Source target needs alias/package")?;
    let source = config.sources.get(alias).ok_or("Unknown source alias")?;
    if crate::resolver::ResolverEngine::is_excluded(name, alias, &config.exclude) {
        return Err(format!("'{target}' is excluded by configuration"));
    }
    if config.features.stability_delays {
        let globs = crate::resolver::ResolverEngine::compile_glob_delays(&config.delay);
        if crate::resolver::ResolverEngine::match_delay_days(name, &config.delay, &globs)
            .is_some_and(|days| days > 0)
        {
            return Err(format!("'{target}' is held: release-source delay timestamps are not supported yet; remove this package's delay rule to opt in"));
        }
    }
    let recipe = source
        .packages
        .get(name)
        .ok_or_else(|| format!("No recipe for '{target}'"))?;
    let releases = catalog::releases(source, name, recipe)?;
    catalog::select(alias, name, source, recipe, releases)
}

pub fn ignored(name: &str, flags: &[String]) -> bool {
    flags
        .iter()
        .enumerate()
        .filter_map(|(index, flag)| {
            flag.strip_prefix("--ignore=").or_else(|| {
                if flag == "--ignore" {
                    flags.get(index + 1).map(String::as_str)
                } else {
                    None
                }
            })
        })
        .flat_map(|value| value.split(','))
        .any(|pattern| glob::Pattern::new(pattern).is_ok_and(|p| p.matches(name)))
}

pub fn is_direct_syntax(value: &str) -> bool {
    value.starts_with("github:") || value.starts_with("gitlab:") || value.starts_with("manifest:") || value.starts_with("https://")
}

pub fn install_targets(
    config: &mut Config,
    targets: &[String],
    options: &crate::pm::InstallOptions,
) -> Result<Vec<String>, String> {
    validate_config(config)?;
    if !options.forwarded_flags.is_empty() || options.refresh || options.sysupgrade {
        return Err("Source installs do not accept native transaction flags; run 'pin -Syu' separately, then 'pin -S alias/package'".into());
    }
    let plans: Vec<_> = targets
        .iter()
        .map(|target| plan(config, target))
        .collect::<Result<_, _>>()?;
    for plan in &plans {
        println!(
            ":: Source: {}/{} -> {} ({}, {})",
            plan.alias, plan.name, plan.version, plan.tag,
            if plan.git.is_some() { "Git commit fixed before approval" } else { "SHA-256 verified before installation" }
        );
        build::preview(plan);
        build::check_prerequisites(config, plan, None)?;
    }
    if options.dry_run {
        if let Some(description) = crate::scratch::describe_plan(true, false) { println!("Dry-Run: {description}"); }
        return Ok(Vec::new());
    }
    install::confirm(options.noconfirm)?;
    let mut installed = Vec::new();
    for plan in plans {
        install::execute(config, &plan, options.needed)?;
        installed.push(format!("{}/{}", plan.alias, plan.name));
    }
    Ok(installed)
}

/// Plans a direct provider target without broadening native package syntax.
pub fn install_direct(
    config: &mut Config,
    target: &str,
    channel: Channel,
    options: &crate::pm::InstallOptions,
) -> Result<Vec<String>, String> {
    if !options.forwarded_flags.is_empty() || options.refresh || options.sysupgrade {
        return Err("Direct source installs do not accept native transaction flags; run 'pin -Syu' separately, then install the direct source".into());
    }
    let direct = direct::parse(target)?.ok_or("Not a direct source target")?;
    let (name, source, recipe) = if let Some(known) = direct::known_recipe(&direct, channel.clone()) {
        known
    } else {
        if !io::stdin().is_terminal() { return Err("An unreviewed direct source needs interactive recipe input on a terminal; refusing to guess artifacts, layout, dependencies, or checksums".into()); }
        let mut source = direct::source(&direct);
        let manifest_names = if matches!(direct, direct::DirectTarget::Manifest { .. }) { Some(catalog::manifest_packages(&source)?) } else { None };
        let project = if let Some(names) = manifest_names {
            choose_manifest_package(&names)?
        } else { direct::project(&direct).unwrap_or_default().to_string() };
        let (name, recipe) = prompt_recipe(&project, channel.clone())?;
        source.packages.insert(name.clone(), recipe.clone());
        (name, source, recipe)
    };
    // Preserve an equivalent source saved by the earlier direct-target flow;
    // otherwise use the collision-resistant identity alias.
    let alias = config.sources.iter().find_map(|(alias, existing)| {
        (existing == &source).then(|| alias.clone())
    }).unwrap_or_else(|| direct::alias(&direct, &channel, &name));
    if let Some(existing) = config.sources.get(&alias) {
        if existing != &source { return Err(format!("Automatic source alias '{alias}' already belongs to another definition")); }
    }
    if let Some(existing) = owner(config, &name) {
        if existing != alias { return Err(format!("'{name}' is already owned by source '{existing}'; unpin it before switching sources")); }
    }
    let releases = catalog::releases(&source, &name, &recipe)?;
    let mut plan = catalog::select(&alias, &name, &source, &recipe, releases)?;
    plan.persist_source = !config.sources.contains_key(&alias);
    println!(
        ":: Direct source: {target} -> {}/{} (version {}, tag {})",
        plan.alias, plan.name, plan.version, plan.tag
    );
    println!(
        ":: Recipe: project={}, channel={:?}, format={:?}, asset={}, root={}, binaries={:?}, deps=[{}]",
        plan.recipe.project,
        plan.recipe.channel,
        plan.recipe.format,
        plan.asset.name,
        if plan.recipe.root.is_empty() { "/" } else { &plan.recipe.root },
        plan.recipe.binaries,
        plan.recipe.depends.join(", ")
    );
    build::preview(&plan);
    build::check_prerequisites(config, &plan, None)?;
    if options.dry_run {
        if let Some(description) = crate::scratch::describe_plan(true, false) { println!("Dry-Run: {description}"); }
        return Ok(Vec::new());
    }
    install::confirm(options.noconfirm)?;
    install::execute(config, &plan, options.needed)?;
    Ok(vec![format!("{}/{}", plan.alias, plan.name)])
}

fn ask(label: &str, default: Option<&str>) -> Result<String, String> {
    print!(":: {label}");
    if let Some(default) = default { print!(" [{default}]"); }
    print!(": "); io::stdout().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new(); io::stdin().read_line(&mut answer).map_err(|e| e.to_string())?;
    let answer = answer.trim().to_string();
    Ok(if answer.is_empty() { default.unwrap_or("").into() } else { answer })
}

fn choose_manifest_package(names: &[String]) -> Result<String, String> {
    if names.len() == 1 { return Ok(names[0].clone()); }
    if names.is_empty() { return Err("Manifest contains no packages".into()); }
    println!(":: Manifest packages: {}", names.join(", "));
    let choice = ask("Catalog package (exact name)", None)?;
    if names.iter().any(|name| name == &choice) { Ok(choice) } else { Err("Choose one exact package name from the manifest".into()) }
}

fn prompt_recipe(project: &str, channel: Channel) -> Result<(String, Recipe), String> {
    println!(":: This upstream is not reviewed by pacpin. Enter every recipe field; no dependencies or executable layout are inferred.");
    let suggested = project.rsplit('/').next().unwrap_or("app");
    let name = ask("Package name", Some(suggested))?;
    let asset = ask("Release asset filename or glob", None)?;
    let format = ask("Format (archive, binary, native)", Some("archive"))?;
    let version_prefix = ask("Version prefix", Some("v"))?;
    let tag_pattern = ask("Optional tag glob (blank for none)", None)?;
    let tag = ask("Optional exact tag (blank for none)", None)?;
    let root = ask("Optional archive root (blank for payload root)", None)?;
    let mut binaries = BTreeMap::new();
    if format != "native" {
        loop {
            let binary = ask("Executable name (blank when done)", None)?;
            if binary.is_empty() { break; }
            let path = ask("Relative executable path", None)?;
            binaries.insert(binary, path);
        }
    }
    let deps = ask("Runtime dependencies, comma-separated (enter empty to explicitly choose none)", None)?;
    let depends = if deps.is_empty() { vec![] } else { deps.split(',').map(str::trim).filter(|value| !value.is_empty()).map(str::to_string).collect() };
    let checksum_asset = ask("Checksum-file asset glob (blank only if API supplies digest or fixed sha256 follows)", None)?;
    let sha256 = ask("Fixed SHA-256 (requires exact tag; blank otherwise)", None)?;
    let input = direct::RecipeInput { name: name.clone(), asset, format, version_prefix, tag_pattern: (!tag_pattern.is_empty()).then_some(tag_pattern), tag: (!tag.is_empty()).then_some(tag), root, binaries, depends, checksum_asset: (!checksum_asset.is_empty()).then_some(checksum_asset), sha256: (!sha256.is_empty()).then_some(sha256) };
    Ok((name, direct::build_recipe(input, project.into(), channel)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_paths_and_bad_checksums() {
        for value in ["/etc/passwd", "../x", "a/../x", "a\nx", ""] {
            assert!(!safe_relative(value));
        }
        assert!(safe_relative("bin/my-app"));
        assert!(valid_digest(&"a".repeat(64)));
        assert!(!valid_digest(&"g".repeat(64)));
        assert!(ignored("app", &["--ignore=app,other".into()]));
        assert!(ignored("app", &["--ignore".into(), "a*".into()]));
        assert!(!ignored("app", &["--ignore=other".into()]));
    }
    #[test]
    fn ownership_survives_disabled_pinning() {
        let mut config = Config::default();
        config.features.pinning = false;
        config.sources.insert(
            "company".into(),
            Source {
                kind: Kind::Local,
                url: "/tmp/catalog.json".into(),
                token_env: None,
                packages: BTreeMap::new(),
            },
        );
        config.pins.insert("app".into(), "company".into());
        assert_eq!(owner(&config, "app"), Some("company"));
    }

    #[test]
    fn invalid_recipes_and_orphaned_source_pins_fail_validation() {
        let mut config: Config = toml::from_str("[sources.company]\nkind='local'\nurl='/tmp/catalog.json'\n[sources.company.packages.app]\nasset='app'\nformat='binary'\n[sources.company.packages.app.binaries]\napp='artifact'\n").unwrap();
        validate_config(&config).unwrap();
        config
            .sources
            .get_mut("company")
            .unwrap()
            .packages
            .get_mut("app")
            .unwrap()
            .binaries
            .insert("bad".into(), "x$(touch-danger)".into());
        assert!(validate_config(&config).is_err());
        config
            .sources
            .get_mut("company")
            .unwrap()
            .packages
            .get_mut("app")
            .unwrap()
            .binaries
            .remove("bad");
        config.pins.insert("missing".into(), "company".into());
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn source_owned_package_is_not_replaced_by_native_repo_candidate() {
        use crate::db::test_support::{AlpmFixture, FixturePackage};
        let fixture = AlpmFixture::new(
            &[FixturePackage::new("app", "1.0-1")],
            &[("extra", &[FixturePackage::new("app", "2.0-1")])],
        );
        let mut config = Config::default();
        config.features.pinning = false;
        config.sources.insert(
            "company".into(),
            Source {
                kind: Kind::Local,
                url: "/tmp/catalog.json".into(),
                token_env: None,
                packages: BTreeMap::new(),
            },
        );
        config.pins.insert("app".into(), "company".into());
        let result = crate::resolver::ResolverEngine::new(&fixture.manager)
            .resolve_with_options(&config, true);
        assert!(result.updates.is_empty());
    }

    #[test]
    fn private_native_pins_do_not_need_aur_fallback_queries() {
        use crate::db::test_support::{AlpmFixture, FixturePackage};
        let fixture = AlpmFixture::new(&[FixturePackage::new("company-private-app", "1.0-1")], &[("company-repo", &[])]);
        let mut config = Config::default();
        config.pins.insert("company-private-app".into(), "company-repo".into());
        let result = crate::resolver::ResolverEngine::new(&fixture.manager).resolve_all(&config);
        assert!(result.updates.is_empty());
        assert_eq!(result.unresolved_pins, [("company-private-app".into(), "company-repo".into())]);
    }
}
