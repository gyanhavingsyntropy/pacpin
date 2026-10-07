//! Strict, opt-in syntax for one-shot upstream source targets.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectTarget {
    Github { project: String },
    Gitlab { project: String, api_base: String },
    Manifest { source: Source },
}

fn valid_project(value: &str, github: bool) -> Result<String, String> {
    if !safe_relative(value)
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("Project must be a slash-separated path without traversal".into());
    }
    let count = value.split('/').count();
    if (github && count != 2) || (!github && count < 2) {
        return Err(if github {
            "GitHub projects need owner/project"
        } else {
            "GitLab projects need group/project (nested groups are allowed)"
        }
        .into());
    }
    Ok(value.into())
}

fn hosted_url(value: &str) -> Result<Option<DirectTarget>, String> {
    if !value.starts_with("https://") {
        return Ok(None);
    }
    let url = catalog::https_url(value)?;
    if url.path().ends_with(".json") {
        return Ok(Some(DirectTarget::Manifest { source: Source { kind: Kind::Manifest, url: value.into(), token_env: None, packages: BTreeMap::new() } }));
    }
    if url.query().is_some() {
        return Err("Direct project URLs cannot contain a query string".into());
    }
    let host = url.host_str().unwrap();
    let path = url.path().trim_matches('/');
    match host {
        "github.com" => Ok(Some(DirectTarget::Github { project: valid_project(path, true)? })),
        "gitlab.com" => Ok(Some(DirectTarget::Gitlab { project: valid_project(path, false)?, api_base: "https://gitlab.com/api/v4".into() })),
        _ => Err("Bare HTTPS project URLs are supported only for github.com and gitlab.com; use gitlab:https://host/group/project for a self-hosted GitLab instance".into()),
    }
}

pub fn parse(value: &str) -> Result<Option<DirectTarget>, String> {
    if let Some(url) = value.strip_prefix("manifest:") {
        if url.starts_with("https://") {
            catalog::https_url(url)?;
            return Ok(Some(DirectTarget::Manifest { source: Source { kind: Kind::Manifest, url: url.into(), token_env: None, packages: BTreeMap::new() } }));
        }
        if std::path::Path::new(url).is_absolute() {
            return Ok(Some(DirectTarget::Manifest { source: Source { kind: Kind::Local, url: url.into(), token_env: None, packages: BTreeMap::new() } }));
        }
        return Err("manifest: needs an HTTPS URL or absolute local catalog path".into());
    }
    if let Some(project) = value.strip_prefix("github:") {
        return Ok(Some(DirectTarget::Github {
            project: valid_project(project, true)?,
        }));
    }
    if let Some(project) = value.strip_prefix("gitlab:") {
        if project.starts_with("https://") {
            let url = catalog::https_url(project)?;
            if url.query().is_some() {
                return Err("Direct project URLs cannot contain a query string".into());
            }
            if url.path().contains("/api/v4") {
                return Err("gitlab: expects a project web URL, not an API URL; configure a source with the exact API base for subpath GitLab instances".into());
            }
            let path = valid_project(url.path().trim_matches('/'), false)?;
            let mut api = url;
            api.set_path("/api/v4");
            api.set_query(None);
            return Ok(Some(DirectTarget::Gitlab {
                project: path,
                api_base: api.to_string().trim_end_matches('/').into(),
            }));
        }
        return Ok(Some(DirectTarget::Gitlab {
            project: valid_project(project, false)?,
            api_base: "https://gitlab.com/api/v4".into(),
        }));
    }
    hosted_url(value)
}

pub fn source(target: &DirectTarget) -> Source {
    match target {
        DirectTarget::Github { .. } => Source { kind: Kind::Github, url: String::new(), token_env: None, packages: BTreeMap::new() },
        DirectTarget::Gitlab { api_base, .. } => Source { kind: Kind::Gitlab, url: api_base.clone(), token_env: None, packages: BTreeMap::new() },
        DirectTarget::Manifest { source } => source.clone(),
    }
}

pub fn project(target: &DirectTarget) -> Option<&str> {
    match target { DirectTarget::Github { project } | DirectTarget::Gitlab { project, .. } => Some(project), DirectTarget::Manifest { .. } => None }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeInput {
    pub name: String, pub asset: String, pub format: String, pub version_prefix: String,
    pub tag_pattern: Option<String>, pub tag: Option<String>, pub root: String,
    pub binaries: BTreeMap<String, String>, pub depends: Vec<String>,
    pub checksum_asset: Option<String>, pub sha256: Option<String>,
}

pub fn build_recipe(input: RecipeInput, project: String, channel: Channel) -> Result<Recipe, String> {
    let format = match input.format.as_str() { "archive" => Format::Archive, "binary" => Format::Binary, "native" => Format::Native, _ => return Err("format must be archive, binary, or native".into()) };
    if !crate::utils::is_safe_pkg_name(&input.name) || !safe_text(&input.asset) || (!project.is_empty() && !safe_relative(&project)) || (!input.root.is_empty() && !safe_relative(&input.root)) { return Err("Invalid package name, asset, project, or payload root".into()); }
    if format != Format::Native && input.binaries.is_empty() { return Err("archive and binary recipes need at least one executable mapping".into()); }
    if input.binaries.iter().any(|(name, path)| !crate::utils::is_safe_pkg_name(name) || !safe_relative(path)) || input.depends.iter().any(|dep| !crate::utils::is_safe_pkg_name(dep)) { return Err("Invalid executable mapping or dependency".into()); }
    if input.sha256.as_ref().is_some_and(|digest| !valid_digest(digest) || input.tag.is_none()) { return Err("A fixed sha256 needs a fixed tag and 64 hex digits".into()); }
    if input.tag_pattern.as_ref().is_some_and(|pattern| glob::Pattern::new(pattern).is_err()) || input.checksum_asset.as_ref().is_some_and(|pattern| !safe_text(pattern) || glob::Pattern::new(pattern).is_err()) { return Err("Invalid tag or checksum glob".into()); }
    Ok(Recipe { release_pages: 1, project, channel, version_prefix: if input.version_prefix.is_empty() { "v".into() } else { input.version_prefix }, tag_pattern: input.tag_pattern, asset: input.asset, format, root: input.root, binaries: input.binaries, depends: input.depends, sha256: input.sha256, checksum_asset: input.checksum_asset, tag: input.tag })
}

pub fn known_recipe(target: &DirectTarget, channel: Channel) -> Option<(String, Source, Recipe)> {
    let DirectTarget::Github { project } = target else {
        return None;
    };
    if project != "openai/codex" {
        return None;
    }
    let name = "openai-codex".to_string();
    let recipe = Recipe {
        release_pages: 1,
        project: project.clone(),
        channel: channel.clone(),
        version_prefix: "rust-v".into(),
        tag_pattern: matches!(channel, Channel::Alpha).then(|| "rust-v*-alpha.*".into()),
        asset: "codex-package-{arch}-unknown-linux-musl.tar.gz".into(),
        format: Format::Archive,
        root: String::new(),
        binaries: BTreeMap::from([("codex".into(), "bin/codex".into())]),
        depends: vec![
            "glibc".into(),
            "alsa-lib".into(),
            "bubblewrap".into(),
            "ripgrep".into(),
        ],
        sha256: None,
        checksum_asset: None,
        tag: None,
    };
    let source = Source {
        kind: Kind::Github,
        url: String::new(),
        token_env: None,
        packages: BTreeMap::from([(name.clone(), recipe.clone())]),
    };
    Some((name, source, recipe))
}

pub fn alias(target: &DirectTarget, channel: &Channel, package: &str) -> String {
    let (kind, label, identity) = match target {
        DirectTarget::Github { project } => ("github", project.clone(), format!("github\0https://api.github.com\0{project}")),
        DirectTarget::Gitlab { project, api_base } => ("gitlab", project.clone(), format!("gitlab\0{api_base}\0{project}")),
        DirectTarget::Manifest { source } => {
            let label = url::Url::parse(&source.url).ok().and_then(|url| url.path_segments().and_then(|mut parts| parts.next_back()).map(str::to_string)).unwrap_or_else(|| std::path::Path::new(&source.url).file_name().and_then(|part| part.to_str()).unwrap_or("catalog").into());
            ("manifest", label, format!("{:?}\0{}", source.kind, source.url))
        }
    };
    let channel = match channel {
        Channel::Stable => "stable",
        Channel::Alpha => "alpha",
        Channel::Beta => "beta",
        Channel::Prerelease => "prerelease",
    };
    let safe_label: String = label.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect::<String>().trim_matches('-').chars().take(32).collect();
    let digest = Sha256::digest(format!("{identity}\0{package}\0{channel}").as_bytes());
    let short = digest[..6].iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    format!("{kind}-{}-{short}-{channel}", if safe_label.is_empty() { "source" } else { &safe_label })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_project_syntax() {
        assert!(matches!(
            parse("github:openai/codex"),
            Ok(Some(DirectTarget::Github { .. }))
        ));
        assert!(matches!(
            parse("gitlab:team/sub/app"),
            Ok(Some(DirectTarget::Gitlab { .. }))
        ));
        assert!(parse("github:owner/../app").is_err());
        assert!(parse("github:owner/app/extra").is_err());
        assert!(parse("gitlab:one").is_err());
    }
    #[test]
    fn url_syntax_is_host_strict() {
        assert!(matches!(
            parse("https://github.com/openai/codex"),
            Ok(Some(DirectTarget::Github { .. }))
        ));
        assert!(matches!(
            parse("https://gitlab.com/team/app"),
            Ok(Some(DirectTarget::Gitlab { .. }))
        ));
        let Some(DirectTarget::Gitlab { project, api_base }) =
            parse("gitlab:https://git.company.example/team/sub/app").unwrap()
        else {
            panic!()
        };
        assert_eq!(project, "team/sub/app");
        assert_eq!(api_base, "https://git.company.example/api/v4");
        assert!(parse("https://git.company.example/team/app").is_err());
        assert!(parse("https://github.com/openai/codex#bad").is_err());
    }
    #[test]
    fn codex_recipe_has_reviewed_alpha_shape() {
        let target = parse("github:openai/codex").unwrap().unwrap();
        let (_, _, recipe) = known_recipe(&target, Channel::Alpha).unwrap();
        assert_eq!(recipe.version_prefix, "rust-v");
        assert_eq!(recipe.tag_pattern.as_deref(), Some("rust-v*-alpha.*"));
        assert_eq!(
            recipe.binaries.get("codex").map(String::as_str),
            Some("bin/codex")
        );
    }
    #[test]
    fn recipe_input_is_strict_and_allows_native() {
        let input = RecipeInput { name: "app".into(), asset: "app.tar.gz".into(), format: "native".into(), version_prefix: "v".into(), tag_pattern: None, tag: None, root: String::new(), binaries: BTreeMap::new(), depends: vec![], checksum_asset: Some("SHA256SUMS".into()), sha256: None };
        assert_eq!(build_recipe(input.clone(), "group/app".into(), Channel::Stable).unwrap().format, Format::Native);
        let mut bad = input; bad.format = "archive".into(); assert!(build_recipe(bad, "group/app".into(), Channel::Stable).is_err());
        assert!(matches!(parse("manifest:https://packages.example/catalog"), Ok(Some(DirectTarget::Manifest { .. }))));
        assert!(matches!(parse("https://packages.example/catalog.json"), Ok(Some(DirectTarget::Manifest { .. }))));
    }
    #[test]
    fn aliases_are_safe_stable_and_bind_full_manifest_identity() {
        let first = parse("https://one.example/releases/catalog.json?signature=one").unwrap().unwrap();
        let same = parse("https://one.example/releases/catalog.json?signature=one").unwrap().unwrap();
        let other_origin = parse("https://two.example/releases/catalog.json?signature=one").unwrap().unwrap();
        let other_query = parse("https://one.example/releases/catalog.json?signature=two").unwrap().unwrap();
        let generated = alias(&first, &Channel::Stable, "app");
        assert_eq!(generated, alias(&same, &Channel::Stable, "app"));
        assert_ne!(generated, alias(&other_origin, &Channel::Stable, "app"));
        assert_ne!(generated, alias(&other_query, &Channel::Stable, "app"));
        assert!(crate::utils::is_safe_pkg_name(&generated));
        let mut config = Config::default();
        let DirectTarget::Manifest { source } = first else { panic!() };
        config.sources.insert(generated, source);
        validate_config(&config).unwrap();
    }
}
