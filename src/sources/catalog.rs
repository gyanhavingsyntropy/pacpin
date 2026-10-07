use super::*;
use semver::Version;
use std::io::Read;
use std::time::Duration;
use url::Url;

pub const MAX_METADATA: u64 = 10 * 1024 * 1024;
pub const MAX_ARTIFACT: u64 = 2 * 1024 * 1024 * 1024;

pub fn https_url(value: &str) -> Result<Url, String> {
    if value.chars().any(char::is_control) {
        return Err("Control characters in source URL".into());
    }
    let url = Url::parse(value).map_err(|_| "Invalid source URL")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("Sources require HTTPS URLs without embedded credentials or fragments".into());
    }
    Ok(url)
}

fn token(source: &Source) -> Result<Option<String>, String> {
    source
        .token_env
        .as_ref()
        .map(|name| {
            let token = std::env::var(name)
                .map_err(|_| format!("Configured token variable '{name}' is unset"))?;
            if token.is_empty() || token.chars().any(char::is_control) {
                return Err("Invalid source token".into());
            }
            Ok(token)
        })
        .transpose()
}

/// Redirects are handled explicitly: enforce HTTPS on every hop and do not
/// forward credentials to another origin. Private cross-origin downloads must
/// use a pre-authorized asset URL rather than ambient tokens.
pub fn request(url: &str, auth: Option<&str>, origin: &Url) -> Result<ureq::Response, String> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .timeout_write(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .build();
    let mut current = https_url(url)?;
    for _ in 0..6 {
        let mut req = agent
            .get(current.as_str())
            .set("User-Agent", "pacpin-source/1");
        if current.origin() == origin.origin() {
            if let Some(auth) = auth {
                req = req.set("Authorization", &format!("Bearer {auth}"));
            }
        }
        // Do not include URLs or HTTP error text: private query strings may be credentials.
        let response = req.call().map_err(|e| match e {
            ureq::Error::Status(status, _) => format!("Source HTTP request failed ({status}); retry later or configure token_env for a private/rate-limited source"),
            _ => "Source network request failed or timed out".into(),
        })?;
        if matches!(response.status(), 301 | 302 | 303 | 307 | 308) {
            let next = response
                .header("Location")
                .ok_or("Source redirect has no Location")?;
            current = https_url(current.join(next).map_err(|_| "Invalid redirect")?.as_str())?;
        } else if response.status() == 200 {
            return Ok(response);
        } else {
            return Err(format!(
                "Unexpected source HTTP status {}",
                response.status()
            ));
        }
    }
    Err("Too many source redirects".into())
}

pub fn bounded_bytes(reader: impl Read, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read source body")?;
    if bytes.len() as u64 > limit {
        return Err("Source response exceeds size limit".into());
    }
    Ok(bytes)
}

fn open_regular(path: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "Cannot open regular source file")?;
    if !file
        .metadata()
        .map_err(|_| "Cannot stat source file")?
        .is_file()
    {
        return Err("Source files must be regular files, not symlinks/devices/FIFOs".into());
    }
    Ok(file)
}

pub fn local_path(source: &Source, relative: &str) -> Result<std::path::PathBuf, String> {
    if !safe_relative(relative) {
        return Err("Local artifact must be a safe relative path".into());
    }
    let catalog = std::fs::canonicalize(&source.url).map_err(|_| "Cannot resolve local catalog")?;
    let root = catalog.parent().ok_or("Local catalog has no parent")?;
    let path =
        std::fs::canonicalize(root.join(relative)).map_err(|_| "Cannot resolve local artifact")?;
    if !path.starts_with(root) || !path.is_file() {
        return Err("Local artifact escapes catalog directory or is not a file".into());
    }
    Ok(path)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    packages: BTreeMap<String, Vec<Release>>,
}

pub fn manifest_packages(source: &Source) -> Result<Vec<String>, String> {
    if !matches!(source.kind, Kind::Manifest | Kind::Local) { return Err("Not a manifest source".into()); }
    let bytes = if source.kind == Kind::Local {
        bounded_bytes(open_regular(Path::new(&source.url))?, MAX_METADATA)?
    } else {
        let origin = https_url(&source.url)?;
        let response = request(&source.url, None, &origin)?;
        bounded_bytes(response.into_reader(), MAX_METADATA)?
    };
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|_| "Invalid source manifest (expected schema version 1)")?;
    if manifest.version != 1 { return Err("Unsupported source manifest version".into()); }
    Ok(manifest.packages.into_keys().collect())
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    prerelease: bool,
    assets: Vec<GithubAsset>,
}
#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}
#[derive(Deserialize)]
struct GitlabRelease {
    tag_name: String,
    assets: GitlabAssets,
}
#[derive(Deserialize)]
struct GitlabAssets {
    links: Vec<GitlabAsset>,
}
#[derive(Deserialize)]
struct GitlabAsset {
    name: String,
    url: String,
    #[serde(default)]
    direct_asset_url: Option<String>,
}

fn parse_releases(kind: &Kind, bytes: &[u8], name: &str) -> Result<Vec<Release>, String> {
    match kind {
        Kind::Github => {
            let releases: Vec<GithubRelease> = serde_json::from_slice(&api_array(bytes)?)
                .map_err(|_| "Invalid GitHub releases response")?;
            Ok(releases
                .into_iter()
                .filter(|r| !r.draft)
                .map(|r| Release {
                    tag: r.tag_name,
                    version: None,
                    package_version: None,
                    prerelease: r.prerelease,
                    assets: r
                        .assets
                        .into_iter()
                        .map(|a| Asset {
                            name: a.name,
                            url: a.browser_download_url,
                            sha256: a
                                .digest
                                .and_then(|d| d.strip_prefix("sha256:").map(str::to_owned)),
                        })
                        .collect(),
                })
                .collect())
        }
        Kind::Gitlab => {
            let releases: Vec<GitlabRelease> = serde_json::from_slice(&api_array(bytes)?)
                .map_err(|_| "Invalid GitLab releases response")?;
            Ok(releases
                .into_iter()
                .map(|r| Release {
                    tag: r.tag_name,
                    version: None,
                    package_version: None,
                    prerelease: false,
                    assets: r
                        .assets
                        .links
                        .into_iter()
                        .map(|a| Asset {
                            name: a.name,
                            url: a.direct_asset_url.unwrap_or(a.url),
                            sha256: None,
                        })
                        .collect(),
                })
                .collect())
        }
        Kind::Manifest | Kind::Local => {
            let mut manifest: Manifest = serde_json::from_slice(bytes)
                .map_err(|_| "Invalid source manifest (expected schema version 1)")?;
            if manifest.version != 1 {
                return Err("Unsupported source manifest version".into());
            }
            manifest
                .packages
                .remove(name)
                .ok_or_else(|| format!("No releases for package '{name}' in manifest"))
        }
    }
}

fn api_array(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "Invalid releases JSON")?;
    serde_json::to_vec(&if value.is_array() {
        value
    } else {
        serde_json::Value::Array(vec![value])
    })
    .map_err(|_| "Invalid releases JSON".to_string())
}

pub fn releases(source: &Source, name: &str, recipe: &Recipe) -> Result<Vec<Release>, String> {
    if source.kind == Kind::Local {
        let file = open_regular(Path::new(&source.url))?;
        if !file
            .metadata()
            .map_err(|_| "Cannot stat source manifest")?
            .is_file()
        {
            return Err("Manifest must be a regular file".into());
        }
        return parse_releases(&source.kind, &bounded_bytes(file, MAX_METADATA)?, name);
    }
    let base = match source.kind {
        Kind::Github if source.url.is_empty() => "https://api.github.com",
        Kind::Gitlab if source.url.is_empty() => "https://gitlab.com/api/v4",
        _ => &source.url,
    };
    let origin = https_url(base)?;
    let auth = token(source)?;
    if source.kind == Kind::Manifest {
        let response = request(base, auth.as_deref(), &origin)?;
        return parse_releases(
            &source.kind,
            &bounded_bytes(response.into_reader(), MAX_METADATA)?,
            name,
        );
    }
    let mut endpoint = origin.clone();
    {
        let mut segments = endpoint
            .path_segments_mut()
            .map_err(|_| "Invalid API base")?;
        segments.pop_if_empty();
        if source.kind == Kind::Github {
            let parts: Vec<_> = recipe.project.split('/').collect();
            if parts.len() != 2 {
                return Err("GitHub project must be owner/repository".into());
            }
            segments
                .push("repos")
                .push(parts[0])
                .push(parts[1])
                .push("releases");
        } else {
            segments
                .push("projects")
                .push(&recipe.project)
                .push("releases");
        }
    }
    if let Some(tag) = &recipe.tag {
        {
            let mut segments = endpoint
                .path_segments_mut()
                .map_err(|_| "Invalid API base")?;
            if source.kind == Kind::Github {
                segments.push("tags");
            }
            segments.push(tag);
        }
        let response = request(endpoint.as_str(), auth.as_deref(), &origin)?;
        return parse_releases(
            &source.kind,
            &bounded_bytes(response.into_reader(), MAX_METADATA)?,
            name,
        );
    }
    let mut all = Vec::new();
    let mut total_bytes = 0usize;
    // Explicit recent-release window, not a full-history scan. Fixed tags use
    // the exact-tag endpoint above even when older than this window.
    for page in 1..=recipe.release_pages {
        endpoint.set_query(Some(&format!("per_page=20&page={page}")));
        let response = request(endpoint.as_str(), auth.as_deref(), &origin)?;
        let next = response
            .header("Link")
            .is_some_and(|h| h.contains("rel=\"next\""))
            || response
                .header("X-Next-Page")
                .is_some_and(|h| !h.is_empty());
        let bytes = bounded_bytes(response.into_reader(), MAX_METADATA)?;
        total_bytes += bytes.len();
        if total_bytes as u64 > MAX_METADATA * 4 {
            return Err("Release window exceeds 40 MiB total metadata budget".into());
        }
        all.extend(parse_releases(&source.kind, &bytes, name)?);
        if !next {
            return Ok(all);
        }
    }
    Ok(all)
}

fn accepts(channel: &Channel, version: &Version, prerelease: bool) -> bool {
    let first = version.pre.as_str().split('.').next().unwrap_or("");
    match channel {
        Channel::Stable => version.pre.is_empty() && !prerelease,
        Channel::Alpha => first == "alpha",
        Channel::Beta => first == "beta",
        Channel::Prerelease => !version.pre.is_empty() || prerelease,
    }
}

pub fn arch_version(version: &Version) -> String {
    let base = format!("{}.{}.{}", version.major, version.minor, version.patch);
    if version.pre.is_empty() {
        format!("{base}-1")
    } else {
        let pre = version.pre.as_str().replace('-', "_");
        let numeric_prefix = if pre.as_bytes()[0].is_ascii_digit() {
            "pre"
        } else {
            ""
        };
        format!("{base}{numeric_prefix}{pre}-1")
    }
}

fn checksum_line(bytes: &[u8], asset: &str) -> Result<String, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Checksum file is not UTF-8")?;
    let mut found = Vec::new();
    for line in text.lines() {
        let Some((digest, filename)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let filename = filename
            .trim_start()
            .strip_prefix('*')
            .unwrap_or(filename.trim_start());
        if filename == asset && valid_digest(digest) {
            found.push(digest.to_ascii_lowercase());
        }
    }
    if found.len() != 1 {
        return Err("Checksum file needs exactly one SHA-256 entry for the selected asset".into());
    }
    Ok(found.remove(0))
}

fn checksum_asset(
    source: &Source,
    release: &Release,
    pattern: &str,
    asset: &str,
) -> Result<String, String> {
    let pattern = glob::Pattern::new(pattern).map_err(|_| "Invalid checksum asset pattern")?;
    let matches: Vec<_> = release
        .assets
        .iter()
        .filter(|a| pattern.matches(&a.name))
        .collect();
    if matches.len() != 1 {
        return Err("Expected exactly one checksum-file asset".into());
    }
    let checksum = matches[0];
    let bytes = if source.kind == Kind::Local {
        bounded_bytes(
            open_regular(&local_path(source, &checksum.url)?)?,
            1024 * 1024,
        )?
    } else {
        let url = if source.kind == Kind::Manifest {
            https_url(&source.url)?
                .join(&checksum.url)
                .map_err(|_| "Invalid checksum URL")?
        } else {
            https_url(&checksum.url)?
        };
        // Never leak API credentials to checksum-asset hosts.
        bounded_bytes(
            request(url.as_str(), None, &url)?.into_reader(),
            1024 * 1024,
        )?
    };
    checksum_line(&bytes, asset)
}

pub fn select(
    alias: &str,
    name: &str,
    source: &Source,
    recipe: &Recipe,
    releases: Vec<Release>,
) -> Result<Plan, String> {
    let pattern = recipe
        .tag_pattern
        .as_ref()
        .map(|p| glob::Pattern::new(p))
        .transpose()
        .map_err(|_| "Invalid tag pattern")?;
    let mut matches = Vec::new();
    for release in releases {
        if !safe_text(&release.tag) {
            return Err("Release contains unsafe tag text".into());
        }
        if recipe.tag.as_ref().is_some_and(|tag| *tag != release.tag)
            || pattern.as_ref().is_some_and(|p| !p.matches(&release.tag))
        {
            continue;
        }
        let value = release.version.as_deref().unwrap_or_else(|| {
            release
                .tag
                .strip_prefix(&recipe.version_prefix)
                .unwrap_or(&release.tag)
        });
        let Ok(version) = Version::parse(value) else {
            continue;
        };
        if accepts(&recipe.channel, &version, release.prerelease) {
            matches.push((version, release));
        }
    }
    matches.sort_by(|a, b| b.0.cmp_precedence(&a.0));
    let (version, release) = matches.first().ok_or_else(|| format!("No matching semantic release for '{alias}/{name}' (check channel, tag_pattern, and version_prefix)"))?;
    if matches
        .get(1)
        .is_some_and(|other| other.0.cmp_precedence(version).is_eq())
    {
        return Err("Ambiguous releases with identical version precedence".into());
    }
    let asset_pattern = recipe
        .asset
        .replace("{arch}", std::env::consts::ARCH)
        .replace("{version}", &version.to_string())
        .replace("{tag}", &release.tag);
    let pattern = glob::Pattern::new(&asset_pattern).map_err(|_| "Invalid asset pattern")?;
    let assets: Vec<_> = release
        .assets
        .iter()
        .filter(|a| pattern.matches(&a.name))
        .collect();
    if assets.len() != 1 {
        return Err(format!(
            "Expected exactly one asset for '{alias}/{name}', found {}",
            assets.len()
        ));
    }
    let mut asset = assets[0].clone();
    if !safe_text(&asset.name) {
        return Err("Unsafe asset name".into());
    }
    asset.sha256 = recipe.sha256.clone().or(asset.sha256);
    if asset.sha256.is_none() {
        if let Some(pattern) = &recipe.checksum_asset {
            asset.sha256 = Some(checksum_asset(source, release, pattern, &asset.name)?);
        }
    }
    if !asset.sha256.as_deref().is_some_and(valid_digest) {
        return Err(format!("'{alias}/{name}' requires a SHA-256 digest from the catalog/API or a fixed tag with sha256"));
    }
    if source.kind == Kind::Local {
        local_path(source, &asset.url)?;
    } else {
        if source.kind == Kind::Manifest {
            asset.url = https_url(&source.url)?
                .join(&asset.url)
                .map_err(|_| "Invalid manifest asset URL")?
                .to_string();
        }
        https_url(&asset.url)?;
    }
    let package_version = release
        .package_version
        .clone()
        .unwrap_or_else(|| arch_version(version));
    if !package_version
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b".:+_-".contains(&b))
        || package_version.is_empty()
    {
        return Err("Invalid package version".into());
    }
    if recipe.format != Format::Native && package_version != arch_version(version) {
        return Err("package_version overrides are only valid for native packages".into());
    }
    Ok(Plan {
        alias: alias.into(),
        name: name.into(),
        source: source.clone(),
        recipe: recipe.clone(),
        tag: release.tag.clone(),
        version: package_version,
        asset,
        persist_source: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn recipe() -> Recipe {
        Recipe {
            release_pages: 1,
            project: "owner/app".into(),
            channel: Channel::Alpha,
            version_prefix: "v".into(),
            tag_pattern: None,
            asset: "app-{arch}.tar.gz".into(),
            format: Format::Archive,
            root: String::new(),
            binaries: BTreeMap::from([("app".into(), "bin/app".into())]),
            depends: vec![],
            sha256: None,
            checksum_asset: None,
            tag: None,
        }
    }
    fn source() -> Source {
        Source {
            kind: Kind::Github,
            url: String::new(),
            token_env: None,
            packages: BTreeMap::new(),
        }
    }
    fn release(tag: &str) -> Release {
        Release {
            tag: tag.into(),
            version: None,
            package_version: None,
            prerelease: true,
            assets: vec![Asset {
                name: format!("app-{}.tar.gz", std::env::consts::ARCH),
                url: "https://example.com/app".into(),
                sha256: Some("a".repeat(64)),
            }],
        }
    }
    #[test]
    fn semantic_alpha_order_and_stable_exclusion() {
        let plan = select(
            "test",
            "app",
            &source(),
            &recipe(),
            vec![
                release("v1.0.0-alpha.2"),
                release("v1.0.0-alpha.10"),
                release("v2.0.0"),
            ],
        )
        .unwrap();
        assert_eq!(plan.tag, "v1.0.0-alpha.10");
        assert_eq!(plan.version, "1.0.0alpha.10-1");
        assert!(alpm::vercmp("1.0.0alpha.10-1", "1.0.0alpha.2-1").is_gt());
        assert!(alpm::vercmp("1.0.0-1", "1.0.0alpha.10-1").is_gt());
    }
    #[test]
    fn ambiguous_assets_and_missing_digest_fail_closed() {
        let mut r = release("v1.0.0-alpha.1");
        r.assets.push(r.assets[0].clone());
        assert!(select("test", "app", &source(), &recipe(), vec![r]).is_err());
        let mut r = release("v1.0.0-alpha.1");
        r.assets[0].sha256 = None;
        assert!(select("test", "app", &source(), &recipe(), vec![r]).is_err());
    }
    #[test]
    fn rejects_duplicate_versions_and_non_https_assets() {
        let r = release("v1.0.0-alpha.1");
        assert!(select("test", "app", &source(), &recipe(), vec![r.clone(), r]).is_err());
        let mut r = release("v1.0.0-alpha.1");
        r.assets[0].url = "file:///etc/passwd".into();
        assert!(select("test", "app", &source(), &recipe(), vec![r]).is_err());
    }
    #[test]
    fn parses_real_adapter_shapes() {
        let gh = br#"[{"tag_name":"v1.2.3","draft":false,"prerelease":false,"assets":[{"name":"app","browser_download_url":"https://example.com/app","digest":"sha256:abcd"}]}]"#;
        assert_eq!(
            parse_releases(&Kind::Github, gh, "app").unwrap()[0].assets[0]
                .sha256
                .as_deref(),
            Some("abcd")
        );
        let gl = br#"[{"tag_name":"v1.2.3","assets":{"links":[{"name":"app","url":"https://example.com/link","direct_asset_url":"https://example.com/app"}]}}]"#;
        assert_eq!(
            parse_releases(&Kind::Gitlab, gl, "app").unwrap()[0].assets[0].url,
            "https://example.com/app"
        );
        assert!(parse_releases(&Kind::Manifest, br#"{"version":2,"packages":{}}"#, "app").is_err());
        assert!(parse_releases(&Kind::Github, br#"{"message":"rate limit"}"#, "app").is_err());
    }
    #[test]
    fn bounded_response_and_url_policy() {
        assert!(bounded_bytes(&b"12345"[..], 4).is_err());
        assert_eq!(bounded_bytes(&b"1234"[..], 4).unwrap(), b"1234");
        for u in [
            "http://example.com",
            "https://user:pass@example.com",
            "https://example.com/#fragment",
            "file:///tmp/x",
            "https://example.com/\n",
        ] {
            assert!(https_url(u).is_err());
        }
    }

    #[test]
    fn special_local_metadata_files_fail_without_blocking() {
        use std::os::unix::ffi::OsStrExt;
        let dir =
            std::env::temp_dir().join(format!("pacpin-catalog-special-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let fifo = dir.join("catalog.json");
        let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(open_regular(&fifo).is_err());
        std::os::unix::fs::symlink("/etc/passwd", dir.join("link")).unwrap();
        assert!(open_regular(&dir.join("link")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unusual_prereleases_generate_valid_arch_versions() {
        let version = arch_version(&Version::parse("1.0.0-alpha-dev.2").unwrap());
        assert_eq!(version, "1.0.0alpha_dev.2-1");
        assert!(alpm::vercmp(
            "1.0.0-1",
            arch_version(&Version::parse("1.0.0-1").unwrap()).as_str()
        )
        .is_gt());
    }
    #[test]
    fn checksum_files_require_exact_unambiguous_filenames() {
        let digest = "a".repeat(64);
        assert_eq!(
            checksum_line(format!("{digest} *app.tar.gz\n").as_bytes(), "app.tar.gz").unwrap(),
            digest
        );
        assert!(checksum_line(
            format!("{digest} app.tar.gz.sig\n").as_bytes(),
            "app.tar.gz"
        )
        .is_err());
        assert!(checksum_line(
            format!("{digest} app.tar.gz\n{digest} app.tar.gz\n").as_bytes(),
            "app.tar.gz"
        )
        .is_err());
    }
}
