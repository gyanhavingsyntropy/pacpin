use super::*;
use crate::execution::{register_cleanup_target, unregister_cleanup_target};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

pub struct PrefetchedPlan {
    pub work: Workdir,
    pub artifact: PathBuf,
}

static PREFETCH_CACHE: Mutex<Option<HashMap<String, PrefetchedPlan>>> = Mutex::new(None);

const BUILD_RESERVE: u64 = 64 * 1024 * 1024;
use crate::scratch::{capacity, fits, mem_available_bytes, Capacity, OS_RESERVE};
#[cfg(test)]
use crate::scratch::quota_remaining;

fn choose_scratch_base(bytes: u64, inodes: u64) -> Result<PathBuf, String> {
    if crate::scratch::policy() != crate::scratch::Policy::Auto {
        return crate::scratch::choose(crate::scratch::policy(), bytes, inodes);
    }
    select_scratch_base(
        [std::env::temp_dir(), PathBuf::from("/var/tmp")]
            .into_iter()
            .map(|base| {
                let space = capacity(&base);
                (base, space)
            }),
        bytes,
        inodes,
        mem_available_bytes().unwrap_or(0),
    )
}

fn describe_capacity(space: Capacity, bytes: u64, inodes: u64, available_memory: u64) -> String {
    let inode_status = if space.inodes == u64::MAX {
        "no fixed inode limit".to_string()
    } else {
        format!("{} free inodes (need {inodes})", space.inodes)
    };
    let ram_status = if space.ram {
        format!(
            "; RAM: {:.1} MiB available, {:.1} MiB required (workspace + {:.1} MiB OS reserve)",
            available_memory as f64 / 1_048_576.0,
            bytes.saturating_add(OS_RESERVE) as f64 / 1_048_576.0,
            OS_RESERVE as f64 / 1_048_576.0,
        )
    } else {
        String::new()
    };
    format!(
        "storage: {:.1} MiB available after quotas (need {:.1} MiB); {inode_status}{ram_status}",
        space.bytes as f64 / 1_048_576.0,
        bytes as f64 / 1_048_576.0,
    )
}

fn select_scratch_base(
    candidates: impl IntoIterator<Item = (PathBuf, Result<Capacity, String>)>,
    bytes: u64,
    inodes: u64,
    available_memory: u64,
) -> Result<PathBuf, String> {
    let mut reasons = Vec::new();
    for (base, space) in candidates {
        match space {
            Ok(space) if fits(space, bytes, inodes, available_memory) => return Ok(base),
            Ok(space) => reasons.push(format!(
                "{}: {}",
                base.display(),
                describe_capacity(space, bytes, inodes, available_memory),
            )),
            Err(error) => reasons.push(format!("{}: {error}", base.display())),
        }
    }
    Err(format!(
        "Insufficient source scratch capacity: need {:.1} MiB and {inodes} inodes.\n  {}\nFree RAM or unused scratch files, or set TMPDIR to a writable disk filesystem with enough space.",
        bytes as f64 / 1_048_576.0, reasons.join("\n  "),
    ))
}

pub struct Workdir(pub PathBuf);
impl Workdir {
    pub fn new_with_base(base: &Path) -> Result<Self, String> {
        crate::execution::init_signal_handlers();
        let template = base.join("pacpin-source-XXXXXX");
        use std::os::unix::ffi::OsStrExt;
        let mut bytes = template.as_os_str().as_bytes().to_vec();
        bytes.push(0);
        let ptr = unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) };
        if ptr.is_null() {
            return Err(format!(
                "Cannot create private source directory in {}: {}",
                base.display(),
                io::Error::last_os_error()
            ));
        }
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[..bytes.len() - 1]));
        register_cleanup_target(path.clone());
        Ok(Self(path))
    }

    #[allow(dead_code)]
    pub fn new() -> Result<Self, String> {
        Self::new_with_base(&std::env::temp_dir())
    }

    fn ensure_capacity(
        &mut self,
        additional_bytes: u64,
        additional_inodes: u64,
    ) -> Result<(), String> {
        let space = capacity(&self.0)?;
        let available_memory = mem_available_bytes().unwrap_or(0);
        if crate::scratch::accepts(crate::scratch::policy(), space)
            && fits(space, additional_bytes, additional_inodes, available_memory) {
            return Ok(());
        }
        // Before extraction the workspace contains only the verified artifact.
        // Recheck prefetched workspaces, since foreground work may consume space.
        let artifact = self.0.join("artifact");
        let existing_bytes = match fs::metadata(&artifact) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.to_string()),
        };
        let base = choose_scratch_base(
            additional_bytes.saturating_add(existing_bytes),
            additional_inodes.saturating_add(u64::from(existing_bytes != 0)),
        )?;
        let replacement = Self::new_with_base(&base)?;
        if existing_bytes != 0 {
            fs::copy(&artifact, replacement.0.join("artifact"))
                .map_err(|e| format!("Cannot relocate source artifact: {e}"))?;
        }
        crate::progress::source_message(format!(
            ":: Moving source workspace to {}. Current scratch capacity: {}.",
            base.display(),
            describe_capacity(space, additional_bytes, additional_inodes, available_memory),
        ));
        *self = replacement;
        Ok(())
    }

    pub fn new_for_plan(_plan: &Plan, expected_bytes: Option<u64>) -> Result<Self, String> {
        // Only the download is needed initially. Its length and the expanded
        // payload are checked separately as soon as they become known.
        let (bytes, inodes) = if let Some(build) = &_plan.recipe.build {
            (build.initial_bytes().saturating_add(expected_bytes.unwrap_or(64 * 1024)), build.scratch_inodes.saturating_add(32))
        } else { (expected_bytes.unwrap_or(64 * 1024), 1) };
        let base = choose_scratch_base(bytes, inodes)?;
        let work = Self::new_with_base(&base)?;
        if crate::scratch::policy() != crate::scratch::Policy::Auto {
            crate::progress::source_message(format!(":: Sources temporary workspace: {} ({:?})", work.0.display(), crate::scratch::policy()));
        }
        Ok(work)
    }
}
impl Drop for Workdir {
    fn drop(&mut self) {
        if let Err(error) = crate::execution::remove_owned_tree(&self.0) {
            eprintln!("Warning: Cannot remove source workspace {}: {error}", self.0.display());
        }
        unregister_cleanup_target(&self.0);
    }
}

pub fn prefetch(plan: &Plan) -> Result<(), String> {
    crate::progress::background(|| prefetch_inner(plan))
}

fn prefetch_inner(plan: &Plan) -> Result<(), String> {
    // Git subprocesses retain normal foreground progress/input ownership.
    if plan.git.is_some() { return Ok(()); }
    let key = format!("{}/{}/{}", plan.alias, plan.name, plan.tag);
    if let Ok(lock) = PREFETCH_CACHE.lock() {
        if let Some(map) = lock.as_ref() {
            if map.contains_key(&key) {
                return Ok(());
            }
        }
    }
    crate::progress::source_message(format!(
            ":: Sources: Downloading source package: {}/{} ({})",
            plan.alias, plan.name, plan.tag
        ));
    let mut work = Workdir::new_for_plan(plan, None)?;
    let artifact = download(plan, &mut work)?;
    crate::progress::source_message(format!(
            ":: Sources: Completed download & verification for {}/{}",
            plan.alias, plan.name
        ));
    let mut lock = PREFETCH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = lock.get_or_insert_with(HashMap::new);
    map.insert(key, PrefetchedPlan { work, artifact });
    Ok(())
}

pub fn take_prefetched(plan: &Plan) -> Option<PrefetchedPlan> {
    let key = format!("{}/{}/{}", plan.alias, plan.name, plan.tag);
    let mut lock = PREFETCH_CACHE.lock().ok()?;
    let map = lock.as_mut()?;
    map.remove(&key)
}

pub fn clear_prefetched() {
    // Static caches are not dropped on process exit. Release workdirs explicitly
    // even when another manager fails or only some source packages are installed.
    if let Some(map) = PREFETCH_CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        map.clear();
    }
}

pub fn confirm(noconfirm: bool) -> Result<(), String> {
    if noconfirm {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        return Err("Source install needs confirmation; use --noconfirm in scripts".into());
    }
    println!(":: Upstream packages are not distribution-reviewed. Native package hooks run as root; checksums verify integrity, not publisher trust.");
    print!(":: Install these source packages? [y/N] ");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err("Source installation cancelled".into())
    }
}

fn download(plan: &Plan, work: &mut Workdir) -> Result<PathBuf, String> {
    if let Some(revision) = &plan.git { return super::git::fetch(plan, revision, &work.0); }
    crate::progress::source_operation(|| download_inner(plan, work))
}

fn download_inner(plan: &Plan, work: &mut Workdir) -> Result<PathBuf, String> {
    let (mut reader, content_length): (Box<dyn Read>, Option<u64>) =
        if plan.source.kind == Kind::Local {
            let local = catalog::local_path(&plan.source, &plan.asset.url)?;
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(local)
                .map_err(|_| "Cannot open local artifact")?;
            let meta = file.metadata().map_err(|_| "Cannot stat artifact")?;
            if !meta.is_file() {
                return Err("Artifact is not a regular file".into());
            }
            (Box::new(file), Some(meta.len()))
        } else {
            // Asset requests deliberately receive no API token. Signed URLs work;
            // private same-origin assets may use an explicitly configured token.
            let url = catalog::https_url(&plan.asset.url)?;
            let api = match plan.source.kind {
                Kind::Github if plan.source.url.is_empty() => "https://api.github.com",
                Kind::Gitlab if plan.source.url.is_empty() => "https://gitlab.com/api/v4",
                _ => &plan.source.url,
            };
            let origin = catalog::https_url(api)?;
            let auth = if url.origin() == origin.origin() {
                plan.source
                    .token_env
                    .as_ref()
                    .map(|env| std::env::var(env).map_err(|_| "Configured source token is unset"))
                    .transpose()?
            } else {
                None
            };
            if auth
                .as_ref()
                .is_some_and(|t| t.is_empty() || t.chars().any(char::is_control))
            {
                return Err("Invalid source token".into());
            }
            let response = catalog::request(url.as_str(), auth.as_deref(), &origin)?;
            let content_length = response
                .header("Content-Length")
                .and_then(|s| s.parse::<u64>().ok());
            if let Some(size) = content_length {
                if size > catalog::MAX_ARTIFACT {
                    return Err("Artifact exceeds 2 GiB download limit".into());
                }
            }
            let size_str = content_length
                .map(|sz| format!(" [{:.1} MiB]", sz as f64 / 1_048_576.0))
                .unwrap_or_default();
            crate::progress::source_message(format!(
                ":: Fetching {} from {}{}...",
                plan.asset.name, plan.alias, size_str
            ));
            (response.into_reader(), content_length)
        };
    if content_length.is_some_and(|size| size > catalog::MAX_ARTIFACT) {
        return Err("Artifact exceeds 2 GiB download limit".into());
    }
    work.ensure_capacity(content_length.unwrap_or(64 * 1024), 1)?;
    let path = work.0.join("artifact");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let mut remaining_capacity = capacity(&work.0)?;
    let mut buf = [0u8; 64 * 1024];
    let mut count = 0u64;
    let mut last_reported_mib = 0u64;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|_| "Artifact download failed")?;
        if n == 0 {
            break;
        }
        if count.saturating_add(n as u64) > catalog::MAX_ARTIFACT {
            return Err("Artifact exceeds 2 GiB download limit".into());
        }
        if remaining_capacity.bytes < n as u64
            || count / (5 * 1024 * 1024) != count.saturating_add(n as u64) / (5 * 1024 * 1024)
        {
            remaining_capacity = capacity(&work.0)?;
            if !fits(
                remaining_capacity,
                n as u64,
                0,
                mem_available_bytes().unwrap_or(0),
            ) {
                return Err(format!("Insufficient scratch space, quota or RAM headroom during source download in {}; use TMPDIR on a larger filesystem", work.0.display()));
            }
        }
        if remaining_capacity.bytes < n as u64 {
            return Err(format!("Source download exhausted space or quota in {}; free unused scratch files or use TMPDIR on a larger filesystem", work.0.display()));
        }
        output
            .write_all(&buf[..n])
            .map_err(|e| format!("Cannot write source artifact in {}: {e}", work.0.display()))?;
        remaining_capacity.bytes = remaining_capacity.bytes.saturating_sub(n as u64);
        count += n as u64;
        let current_mib = count / (1024 * 1024);
        if current_mib >= last_reported_mib + 5 {
            last_reported_mib = current_mib;
            crate::progress::source_download(&plan.asset.name, count, content_length);
        }
    }
    if count == 0 {
        return Err("Artifact is empty or exceeds 2 GiB download limit".into());
    }
    output.sync_all().map_err(|e| e.to_string())?;
    crate::progress::source_download(&plan.asset.name, count, content_length);
    crate::progress::source_message(format!(":: Verifying SHA-256 checksum for {}...", plan.asset.name));
    let digest = crate::download::compute_sha256(&path)?;
    if !plan
        .asset
        .sha256
        .as_deref()
        .is_some_and(|expected| digest.eq_ignore_ascii_case(expected))
    {
        return Err("Artifact SHA-256 mismatch; refusing installation".into());
    }
    crate::progress::source_message(format!(
        ":: Verified {} (SHA-256: {})",
        plan.asset.name,
        &digest[..digest.len().min(12)]
    ));
    Ok(path)
}

fn prepare_build(plan: &Plan, work: &mut Workdir) -> Result<PathBuf, String> {
    let artifact = work.0.join("artifact");
    if plan.git.is_some() {
        super::build::check_capacity(plan, &work.0)?;
        return Ok(work.0.join("git-source"));
    }
    let (bytes, entries) = if matches!(plan.recipe.format, Format::Archive | Format::Source) {
        crate::archive::footprint(&artifact)?
    } else {
        (fs::metadata(&artifact).map_err(|e| e.to_string())?.len(), 1)
    };
    // The validated payload is moved into pkgdir, not copied. It coexists
    // with a worst-case compressed package. Keep room for package metadata.
    work.ensure_capacity(
        bytes.saturating_mul(2).saturating_add(BUILD_RESERVE).saturating_add(plan.recipe.build.as_ref().map_or(0, |b| b.headroom_bytes())),
        entries.saturating_add(16).saturating_add(plan.recipe.build.as_ref().map_or(0, |b| b.scratch_inodes)),
    )?;
    Ok(work.0.join("artifact"))
}

pub(super) fn pkgbuild(plan: &Plan, payload: &Path) -> String {
    let quote = crate::utils::shell_quote;
    let pkgver = plan.version.strip_suffix("-1").unwrap_or(&plan.version);
    let depends = plan
        .recipe
        .depends
        .iter()
        .map(|d| quote(d))
        .collect::<Vec<_>>()
        .join(" ");
    let mut text = format!("pkgname={}\npkgver={}\npkgrel=1\narch=({})\nlicense=('custom')\noptions=('!strip' '!debug')\ndepends=({depends})\npackage() {{\n  install -dm755 \"$pkgdir/opt\" \"$pkgdir/usr/bin\"\n  mv -T -- {} \"$pkgdir/opt/{}\"\n  chmod 755 \"$pkgdir/opt/{}\"\n", quote(&plan.name), quote(pkgver), quote(std::env::consts::ARCH), quote(&payload.to_string_lossy()), plan.name, plan.name);
    for (binary, relative) in &plan.recipe.binaries {
        text.push_str(&format!(
            "  chmod 755 -- \"$pkgdir/opt/{}/{}\"\n  ln -s -- {} \"$pkgdir/usr/bin/{}\"\n",
            plan.name,
            relative,
            quote(&format!("../../opt/{}/{relative}", plan.name)),
            binary
        ));
    }
    text.push_str("}\n");
    text
}

pub(super) fn verify_elf(path: &Path, required: bool) -> Result<(), String> {
    let mut file = File::open(path).map_err(|_| "Cannot read declared executable")?;
    let mut header = [0u8; 20];
    let count = file
        .read(&mut header)
        .map_err(|_| "Cannot read executable header")?;
    if count < 4 || header[..4] != *b"\x7fELF" {
        return if required {
            Err("Binary recipes require an ELF executable, not a script".into())
        } else {
            Ok(())
        };
    }
    if count < 20 || !matches!(header[5], 1 | 2) {
        return Err("Malformed ELF executable header".into());
    }
    let machine = if header[5] == 1 {
        u16::from_le_bytes([header[18], header[19]])
    } else {
        u16::from_be_bytes([header[18], header[19]])
    };
    let expected = match std::env::consts::ARCH {
        "x86_64" => (2, 62),
        "x86" => (1, 3),
        "aarch64" => (2, 183),
        "arm" => (1, 40),
        "riscv64" => (2, 243),
        _ => return Err("Unsupported source executable architecture".into()),
    };
    if (header[4], machine) != expected {
        return Err("Declared ELF executable is for a different CPU architecture".into());
    }
    Ok(())
}

fn build(plan: &Plan, work: &Path, artifact: &Path) -> Result<PathBuf, String> {
    if plan.recipe.format == Format::Source { return super::build::execute(plan, work, artifact); }
    if unsafe { libc::geteuid() } == 0 {
        return Err("Source packaging must run unprivileged, not through sudo/pkexec".into());
    }
    if !crate::is_command_available("makepkg") {
        return Err("Source packaging requires makepkg (pacman package); install it first".into());
    }
    let payload = work.join("payload");
    fs::create_dir(&payload).map_err(|e| e.to_string())?;
    fs::set_permissions(&payload, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    if plan.recipe.format == Format::Archive {
        crate::progress::operation(crate::progress::Manager::Sources, "Extracting source archive", || crate::download::extract_archive(artifact, &payload))?;
    } else {
        verify_elf(artifact, true)?;
        fs::copy(artifact, payload.join("artifact")).map_err(|e| e.to_string())?;
    }
    let root = if plan.recipe.root.is_empty() {
        payload.clone()
    } else {
        payload.join(&plan.recipe.root)
    };
    let canonical_root = fs::canonicalize(&root).map_err(|_| "Declared payload root is missing")?;
    if !canonical_root.starts_with(&payload) || !canonical_root.is_dir() {
        return Err("Payload root escapes extraction directory".into());
    }
    for relative in plan.recipe.binaries.values() {
        let binary =
            fs::canonicalize(root.join(relative)).map_err(|_| "Declared binary is missing")?;
        if !binary.starts_with(&canonical_root) || !binary.is_file() {
            return Err("Declared binary escapes payload root or is not a file".into());
        }
        verify_elf(&binary, false)?;
    }
    let provenance = serde_json::json!({"schema":1,"source":plan.alias,"package":plan.name,"tag":plan.tag,"version":plan.version,"sha256":plan.asset.sha256});
    // create_new: reject an archive supplied provenance file/symlink.
    let mut receipt = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o644)
        .open(root.join(".pacpin-source.json"))
        .map_err(|_| "Cannot create trusted package provenance")?;
    receipt
        .write_all(provenance.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    drop(receipt);
    // Integrity and executable validation are complete. Release the download
    // before makepkg writes the output package, keeping the peak smaller.
    fs::remove_file(artifact).map_err(|e| format!("Cannot release verified source artifact: {e}"))?;
    // This is a locally generated, quoted recipe. No upstream PKGBUILD or install script executes.
    fs::write(work.join("PKGBUILD"), pkgbuild(plan, &root)).map_err(|e| e.to_string())?;
    let mut command = Command::new("makepkg");
    command
        .current_dir(work)
        .args(["--nodeps", "--noconfirm", "--nocheck", "--nocolor"])
        .env("PKGDEST", work)
        .env("BUILDDIR", work)
        .env("SRCDEST", work)
        .env("LOGDEST", work);
    let status = crate::progress::run(&mut command, crate::progress::Manager::Sources, "Packaging verified source", || {}).map_err(|_| "Cannot launch makepkg")?;
    if !status.success() {
        return Err(format!("Local source packaging failed in {} (makepkg exit status: {status}); another process may have consumed scratch space or quota during packaging", work.display()));
    }
    let packages: Vec<_> = fs::read_dir(work)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().is_some_and(|n| {
                n.to_string_lossy().contains(".pkg.tar.") && !n.to_string_lossy().ends_with(".sig")
            })
        })
        .collect();
    if packages.len() != 1 {
        return Err("makepkg did not produce exactly one package".into());
    }
    // Only the package archive is needed by pacman. Release the unpacked tree
    // before root installation, rather than retaining another full payload.
    crate::progress::operation(crate::progress::Manager::Sources, "Releasing build scratch", || -> Result<(), String> {
        for directory in ["pkg", "src", "payload"] {
            let path = work.join(directory);
            if path.exists() {
                fs::remove_dir_all(&path).map_err(|e| format!("Cannot release source build tree {}: {e}", path.display()))?;
            }
        }
        Ok(())
    })?;
    Ok(packages[0].clone())
}

pub fn execute(config: &mut Config, plan: &Plan, needed: bool) -> Result<(), String> {
    execute_inner(config, plan, needed, None)
}
pub fn execute_independent(config: &mut Config, plan: &Plan, permit: &crate::pm::AurBuildFailurePermit) -> Result<(), String> {
    execute_inner(config, plan, true, Some(permit))
}
fn execute_inner(config: &mut Config, plan: &Plan, needed: bool, permit: Option<&crate::pm::AurBuildFailurePermit>) -> Result<(), String> {
    // Revalidate even cached plans; never bypass policy at the privilege boundary.
    validate_config(config)?;
    if !plan.persist_source && config.sources.get(&plan.alias) != Some(&plan.source) {
        return Err("Source configuration changed after planning; check again".into());
    }
    crate::pm::check_lock()?;
    super::build::check_prerequisites(config, plan, permit)?;
    if let Some(permit) = permit {
        permit.validate()?;
        let manager = crate::db::AlpmManager::with_repo_order(&config.repo_order).map_err(|e| e.to_string())?;
        crate::pm::ensure_independent(&manager, &plan.name, &plan.recipe.depends, permit.failed_packages())?;
    }
    crate::preview::check_package_root_capacity(0, 0)?;
    let (mut work, artifact) = if let Some(prefetched) = take_prefetched(plan) {
        println!(
            ":: Using prefetched artifact for {}/{} ({})",
            plan.alias, plan.name, plan.tag
        );
        (prefetched.work, prefetched.artifact)
    } else {
        let mut work = Workdir::new_for_plan(plan, None)?;
        println!(
            ":: Downloading and verifying {}/{} ({})",
            plan.alias, plan.name, plan.tag
        );
        let artifact = download(plan, &mut work)?;
        (work, artifact)
    };
    let package = if plan.recipe.format == Format::Native {
        artifact
    } else {
        let artifact = crate::progress::operation(crate::progress::Manager::Sources, "Checking source build space", || prepare_build(plan, &mut work))?;
        build(plan, &work.0, &artifact)?
    };
    let manager =
        crate::db::AlpmManager::with_repo_order(&config.repo_order).map_err(|e| e.to_string())?;
    let loaded = manager
        .handle()
        .pkg_load(
            package.to_string_lossy().as_bytes().to_vec(),
            true,
            alpm::SigLevel::NONE,
        )
        .map_err(|_| "Artifact is not a valid Arch package")?;
    if loaded.name() != plan.name
        || loaded.version().as_str() != plan.version
        || !matches!(loaded.arch(), Some(arch) if arch == std::env::consts::ARCH || arch == "any")
    {
        return Err(
            "Native package identity, version, or architecture differs from the approved plan"
                .into(),
        );
    }
    let installed = manager.handle().localdb().pkg(plan.name.as_str()).ok();
    if let Some(permit) = permit {
        let dependencies = loaded.depends().into_iter().map(|d| d.to_string()).collect::<Vec<_>>();
        crate::pm::ensure_independent(&manager, &plan.name, &dependencies, permit.failed_packages())?;
        if loaded.has_scriptlet() || !loaded.conflicts().is_empty() || !loaded.replaces().is_empty() {
            return Err("Continuation cannot install packages with scriptlets, conflicts or replacements".into());
        }
    }
    crate::preview::check_package_root_capacity(loaded.isize(), installed.map(|p| p.isize()).unwrap_or(0))?;
    let before = manager
        .handle()
        .localdb()
        .pkg(plan.name.as_str())
        .ok()
        .map(|p| p.version().to_string());
    drop(loaded);
    drop(manager);
    if let Some(permit) = permit {
        crate::pm::continuation_archive_check(config, &package, &plan.name)?;
        permit.begin_source_install()?;
    }
    let mut command = crate::privilege::command(&config.options.privilege_tool);
    command.args(["pacman", "-U", "--noconfirm"]);
    command.args(crate::progress::pacman_progress_args());
    if needed && owner(config, &plan.name) == Some(plan.alias.as_str()) {
        command.arg("--needed");
    }
    command.arg("--").arg(&package);
    if !crate::progress::run(&mut command, crate::progress::Manager::Sources, "Installing source package", || {})
        .map_err(|_| "Cannot launch native package transaction")?
        .success()
    {
        return Err("Native source package transaction failed; package files may already have changed. Check the package before retrying".into());
    }
    let fresh = crate::db::AlpmManager::with_repo_order(&config.repo_order)
        .map_err(|e| format!("Cannot verify completed source transaction: {e}"))?;
    if !fresh
        .handle()
        .localdb()
        .pkg(plan.name.as_str())
        .is_ok_and(|p| p.version().as_str() == plan.version)
    {
        return Err("Pacman did not install the approved version (possibly held by IgnorePkg); source ownership was not saved".into());
    }
    drop(fresh);
    // Persist ownership only after pacman's successful transaction.
    config.pins.insert(plan.name.clone(), plan.alias.clone());
    crate::config::mutate_config(|current| {
        if plan.persist_source && !current.sources.contains_key(&plan.alias) {
            current
                .sources
                .insert(plan.alias.clone(), plan.source.clone());
        }
        if current.sources.get(&plan.alias) != Some(&plan.source) {
            return Err("Source changed during installation; ownership was not saved".into());
        }
        current.pins.insert(plan.name.clone(), plan.alias.clone());
        Ok(())
    })
    .map_err(|e| format!("Package installed, but saving source ownership failed: {e}"))?;
    let record = serde_json::json!({"name":plan.name,"from_version":before,"to_version":plan.version,"source":plan.alias,"tag":plan.tag,"sha256":plan.asset.sha256,"git":plan.git,"build_recipe_sha256":super::build::recipe_digest(plan)});
    if let Err(e) = crate::journal::TransactionJournal::record_transaction(
        "install",
        vec![record],
        "pacpin source install (pacman -U)",
    ) {
        eprintln!("Warning: Source transaction journal failed: {e}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(work: &Path) -> Plan {
        let recipe: Recipe = toml::from_str(
            "asset = 'app.tar'\nformat = 'archive'\n[binaries]\npacpin-source-test = 'bin/app'\n",
        )
        .unwrap();
        let source = Source {
            kind: Kind::Local,
            url: work.join("catalog.json").to_string_lossy().into(),
            token_env: None,
            packages: BTreeMap::from([("pacpin-source-test".into(), recipe.clone())]),
        };
        fs::write(&source.url, "{}").unwrap();
        let archive = work.join("app.tar");
        let file = File::create(&archive).unwrap();
        let mut builder = tar::Builder::new(file);
        let body = b"#!/bin/sh\nprintf 'source fixture works\\n'\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "bin/app", &body[..])
            .unwrap();
        builder.finish().unwrap();
        let digest = crate::download::compute_sha256(&archive).unwrap();
        Plan {
            alias: "test-source".into(),
            name: "pacpin-source-test".into(),
            source,
            recipe,
            tag: "v1.2.3".into(),
            version: "1.2.3-1".into(),
            asset: Asset {
                name: "app.tar".into(),
                url: "app.tar".into(),
                sha256: Some(digest),
            },
            git: None,
            persist_source: false,
        }
    }

    #[test]
    fn quota_limits_use_kib_units_and_the_stricter_nonzero_limit() {
        assert_eq!(quota_remaining(10, 0, 1024, 1024), Some(9 * 1024));
        assert_eq!(quota_remaining(10, 8, 1024, 1024), Some(7 * 1024));
        assert_eq!(quota_remaining(0, 8, 1024, 1024), Some(7 * 1024));
        assert_eq!(quota_remaining(0, 0, u64::MAX, 1024), None);
        assert_eq!(quota_remaining(8, 8, 9 * 1024, 1024), Some(0));
        assert_eq!(quota_remaining(10, 0, 4, 1), Some(6));
    }

    fn candidates(tmp_bytes: u64, disk_bytes: u64) -> [(PathBuf, Result<Capacity, String>); 2] {
        [
            (
                PathBuf::from("/tmp"),
                Ok(Capacity {
                    bytes: tmp_bytes,
                    inodes: 100,
                    ram: true,
                }),
            ),
            (
                PathBuf::from("/var/tmp"),
                Ok(Capacity {
                    bytes: disk_bytes,
                    inodes: 100,
                    ram: false,
                }),
            ),
        ]
    }

    #[test]
    fn exhausted_user_quota_selects_disk_even_when_disk_has_less_global_space() {
        // Simulate a tmpfs with plenty of global free space but only 100 bytes
        // remaining under the user's quota. Disk can fit the 200-byte build.
        let remaining = 1000.min(quota_remaining(1, 0, 924, 1024).unwrap());
        assert_eq!(
            select_scratch_base(candidates(remaining, 300), 200, 10, OS_RESERVE + 200).unwrap(),
            PathBuf::from("/var/tmp")
        );
    }

    #[test]
    fn insufficient_candidates_fail_instead_of_returning_tmp() {
        let error =
            select_scratch_base(candidates(100, 50), 200, 10, OS_RESERVE + 200).unwrap_err();
        assert!(error.contains("Insufficient source scratch capacity"));
        assert!(error.contains("/tmp:"));
        assert!(error.contains("/var/tmp:"));
        assert!(error.contains("TMPDIR"));
    }

    #[test]
    fn capacity_error_explains_ram_reserve_and_unlimited_inodes() {
        let mib = 1024 * 1024;
        let tmp = Capacity {
            bytes: 51845 * mib / 10,
            inodes: 1048542,
            ram: true,
        };
        let disk = Capacity {
            bytes: 907 * mib,
            inodes: u64::MAX,
            ram: false,
        };
        let error = select_scratch_base(
            [
                (PathBuf::from("/tmp"), Ok(tmp)),
                (PathBuf::from("/var/tmp"), Ok(disk)),
            ],
            15087 * mib / 10,
            125,
            25878 * mib / 10,
        )
        .unwrap_err();
        assert!(error.contains("2587.8 MiB available, 4068.7 MiB required"));
        assert!(error.contains("workspace + 2560.0 MiB OS reserve"));
        assert!(error.contains("no fixed inode limit"));
        assert!(!error.contains(&u64::MAX.to_string()));
        assert!(error.contains("Free RAM"));
    }

    #[test]
    fn low_memory_and_inode_limits_are_enforced() {
        assert_eq!(
            select_scratch_base(candidates(300, 300), 200, 10, OS_RESERVE + 199).unwrap(),
            PathBuf::from("/var/tmp")
        );
        assert!(select_scratch_base(candidates(300, 300), 200, 101, OS_RESERVE + 200).is_err());
        assert_eq!(
            select_scratch_base(candidates(300, 300), 200, 10, OS_RESERVE + 200).unwrap(),
            PathBuf::from("/tmp")
        );
        let explicit_disk = [(
            PathBuf::from("/scratch"),
            Ok(Capacity {
                bytes: 300,
                inodes: u64::MAX,
                ram: false,
            }),
        )];
        assert_eq!(
            select_scratch_base(explicit_disk, 200, 10, 0).unwrap(),
            PathBuf::from("/scratch")
        );
    }

    #[test]
    fn downloaded_archive_is_inspected_before_building() {
        let source = Workdir::new().unwrap();
        let plan = fixture(&source.0);
        let mut work = Workdir::new().unwrap();
        let artifact = download(&plan, &mut work).unwrap();
        let original_digest = crate::download::compute_sha256(&artifact).unwrap();
        let artifact = prepare_build(&plan, &mut work).unwrap();
        assert_eq!(
            crate::download::compute_sha256(&artifact).unwrap(),
            original_digest
        );
        assert!(!work.0.join("payload").exists());
        assert!(!work.0.join("PKGBUILD").exists());
    }

    #[test]
    fn local_artifact_mismatch_is_rejected_and_success_is_verified() {
        let source = Workdir::new().unwrap();
        let mut plan = fixture(&source.0);
        let mut download_dir = Workdir::new().unwrap();
        download(&plan, &mut download_dir).unwrap();
        plan.asset.sha256 = Some("0".repeat(64));
        let mut invalid_dir = Workdir::new().unwrap();
        assert!(download(&plan, &mut invalid_dir)
            .unwrap_err()
            .contains("mismatch"));
    }

    #[test]
    fn generated_recipe_does_not_execute_remote_scripts() {
        let source = Workdir::new().unwrap();
        let plan = fixture(&source.0);
        let text = pkgbuild(&plan, Path::new("/tmp/path with ' quote"));
        assert!(text.contains("options=('!strip' '!debug')"));
        assert!(text.contains("mv -T -- '/tmp/path with '\\'' quote'"));
        assert!(!text.contains("cp -a"));
        assert!(!text.contains("source=("));
        assert!(!text.contains("install="));
    }

    #[test]
    fn elf_architecture_and_raw_script_policy() {
        let work = Workdir::new().unwrap();
        let path = work.0.join("app");
        fs::write(&path, "#!/bin/sh\necho test\n").unwrap();
        assert!(verify_elf(&path, true).is_err());
        verify_elf(&path, false).unwrap();
        let mut header = [0u8; 20];
        header[..4].copy_from_slice(b"\x7fELF");
        header[4] = 2;
        header[5] = 1;
        fs::write(&path, header).unwrap();
        assert!(verify_elf(&path, true).is_err());
        verify_elf(Path::new("/usr/bin/true"), true).unwrap();
    }

    #[test]
    #[ignore = "requires Arch makepkg; creates a package but never installs it"]
    fn real_makepkg_archive_round_trip() {
        let source = Workdir::new().unwrap();
        let plan = fixture(&source.0);
        let mut work = Workdir::new().unwrap();
        download(&plan, &mut work).unwrap();
        let artifact = prepare_build(&plan, &mut work).unwrap();
        let package = build(&plan, &work.0, &artifact).unwrap();
        assert!(!artifact.exists(), "verified download must be released before packaging");
        assert!(!work.0.join("payload").exists(), "payload must be moved, not duplicated");
        assert!(!work.0.join("pkg").exists(), "unpacked package must be released before installation");
        let fixture = crate::db::test_support::AlpmFixture::new(&[], &[]);
        let loaded = fixture
            .manager
            .handle()
            .pkg_load(
                package.to_string_lossy().as_bytes().to_vec(),
                true,
                alpm::SigLevel::NONE,
            )
            .unwrap();
        assert_eq!(loaded.name(), plan.name);
        assert_eq!(loaded.version().as_str(), plan.version);
        assert_eq!(loaded.arch(), Some(std::env::consts::ARCH));
        let unpacked = work.0.join("verify");
        fs::create_dir(&unpacked).unwrap();
        fs::set_permissions(&unpacked, fs::Permissions::from_mode(0o700)).unwrap();
        crate::download::extract_archive(&package, &unpacked).unwrap();
        assert!(unpacked.join("opt/pacpin-source-test/bin/app").is_file());
        assert!(unpacked
            .join("opt/pacpin-source-test/.pacpin-source.json")
            .is_file());
        assert_eq!(
            fs::read_link(unpacked.join("usr/bin/pacpin-source-test")).unwrap(),
            PathBuf::from("../../opt/pacpin-source-test/bin/app")
        );
    }

    #[test]
    #[ignore = "requires PACPIN_TEST_BINARY; tests signals against a built CLI without installing"]
    fn cli_signals_remove_source_download_workspaces() {
        let executable = std::env::var_os("PACPIN_TEST_BINARY")
            .expect("set PACPIN_TEST_BINARY to the built pacpin executable");
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let fixture = Workdir::new().unwrap();
            let artifact = File::create(fixture.0.join("large.bin")).unwrap();
            artifact.set_len(512 * 1024 * 1024).unwrap();
            let catalog = serde_json::json!({"version":1,"packages":{"app":[{"tag":"v1.0.0","assets":[{"name":"large.bin","url":"large.bin","sha256":"0".repeat(64)}]}]}});
            fs::write(fixture.0.join("catalog.json"), catalog.to_string()).unwrap();
            let mut config: Config = toml::from_str("[features]\nshell_aliases=false\n[sources.signal-test]\nkind='local'\nurl='/placeholder'\n[sources.signal-test.packages.app]\nasset='large.bin'\nformat='binary'\nbinaries={app='artifact'}\n").unwrap();
            config.sources.get_mut("signal-test").unwrap().url =
                fixture.0.join("catalog.json").to_string_lossy().into();
            let config_dir = fixture.0.join("config/pacpin");
            fs::create_dir_all(&config_dir).unwrap();
            fs::write(
                config_dir.join("config.toml"),
                toml::to_string(&config).unwrap(),
            )
            .unwrap();
            let runtime = fixture.0.join("runtime");
            fs::create_dir(&runtime).unwrap();
            let mut child = Command::new(&executable)
                .args(["-S", "signal-test/app", "--noconfirm"])
                .env("XDG_CONFIG_HOME", fixture.0.join("config"))
                .env("XDG_STATE_HOME", fixture.0.join("state"))
                .env("TMPDIR", &runtime)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while fs::read_dir(&runtime).unwrap().next().is_none() {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "CLI exited before the download started"
                );
                if std::time::Instant::now() >= deadline {
                    child.kill().unwrap();
                    panic!("source download did not start");
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, signal) }, 0);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if std::time::Instant::now() >= deadline {
                    child.kill().unwrap();
                    panic!("CLI signal cleanup timed out");
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            };
            assert!(!status.success());
            assert!(
                fs::read_dir(&runtime).unwrap().next().is_none(),
                "source workspace survived signal"
            );
            let saved: Config =
                toml::from_str(&fs::read_to_string(config_dir.join("config.toml")).unwrap())
                    .unwrap();
            assert!(saved.pins.is_empty());
        }
    }
    #[test]
    fn workdir_is_private_and_removed() {
        let work = Workdir::new().unwrap();
        let path = work.0.clone();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        drop(work);
        assert!(!path.exists());
    }
    #[test]
    fn local_symlink_escape_is_rejected() {
        let work = Workdir::new().unwrap();
        fs::write(work.0.join("catalog.json"), "{}").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", work.0.join("escape")).unwrap();
        let source = Source {
            kind: Kind::Local,
            url: work.0.join("catalog.json").to_string_lossy().into(),
            token_env: None,
            packages: BTreeMap::new(),
        };
        assert!(catalog::local_path(&source, "escape").is_err());
        assert!(catalog::local_path(&source, "../passwd").is_err());
    }
}
