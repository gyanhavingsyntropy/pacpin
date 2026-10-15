//! Local build recipe schema, validation and approval preview.
use super::*;
use sha2::{Digest, Sha256};
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Cpu {
    #[default]
    System,
    Native,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Optimization {
    /// System inherits makepkg. Native is explicitly requested, never a fallback.
    #[serde(default)]
    pub cpu: Cpu,
    #[serde(default)]
    pub level: Option<u8>,
    #[serde(default)]
    pub lto: Option<bool>,
}

fn default_inodes() -> u64 {
    4096
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BuildRecipe {
    /// No shell interpolation; each nested array is one literal argv command.
    pub commands: Vec<Vec<String>>,
    /// Relative staging directory created by the commands; installed beneath /opt.
    pub output: String,
    /// Additional build headroom, not a quota or a guarantee of peak usage.
    pub scratch_mib: u64,
    #[serde(default = "default_inodes")]
    pub scratch_inodes: u64,
    #[serde(default)]
    pub makedepends: Vec<String>,
    #[serde(default)]
    pub optimization: Optimization,
    #[serde(default)]
    pub jobs: Option<u32>,
    /// HTTPS repository; local catalogs may explicitly use an absolute directory.
    #[serde(default)]
    pub git: Option<String>,
    /// Optional full commit; otherwise resolve the selected release tag once.
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub submodules: bool,
}

impl BuildRecipe {
    pub fn headroom_bytes(&self) -> u64 {
        self.scratch_mib.saturating_mul(1_048_576)
    }
    pub fn initial_bytes(&self) -> u64 {
        self.headroom_bytes().saturating_add(64 * 1_048_576)
    }
}

pub(in crate::sources) fn recipe_digest(plan: &Plan) -> Option<String> {
    plan.recipe.build.as_ref().map(|_| {
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&plan.recipe).expect("serializable build recipe"))
        )
    })
}

pub(in crate::sources) fn validate(recipe: &Recipe, source: &Source) -> Result<(), String> {
    let Some(build) = &recipe.build else {
        return if recipe.format == Format::Source {
            Err("format=source requires a build table".into())
        } else {
            Ok(())
        };
    };
    if recipe.format != Format::Source {
        return Err("build instructions require format=source".into());
    }
    if !safe_relative(&build.output) || build.commands.is_empty() || build.commands.len() > 64 {
        return Err(
            "Source build needs a relative output directory and 1..64 literal commands".into(),
        );
    }
    for args in &build.commands {
        if args.is_empty()
            || args.len() > 128
            || args[0].starts_with('-')
            || args
                .iter()
                .any(|arg| arg.len() > 8192 || arg.chars().any(char::is_control))
        {
            return Err(
                "Invalid source build argv (empty command, controls or excessive arguments)".into(),
            );
        }
        for arg in args {
            let remainder = arg
                .replace("{source}", "")
                .replace("{output}", "")
                .replace("{version}", "");
            if remainder.contains(['{', '}']) {
                return Err(
                    "Unknown build placeholder; supported: {source}, {output}, {version}".into(),
                );
            }
        }
        if args[0].is_empty() {
            return Err("Empty build executable".into());
        }
    }
    if !(1..=1_048_576).contains(&build.scratch_mib)
        || !(16..=10_000_000).contains(&build.scratch_inodes)
        || build.jobs.is_some_and(|jobs| !(1..=1024).contains(&jobs))
        || build.optimization.level.is_some_and(|level| level > 3)
    {
        return Err(
            "Invalid build headroom, jobs or optimization level (level must be 0..3)".into(),
        );
    }
    if build
        .makedepends
        .iter()
        .any(|name| !crate::utils::is_safe_pkg_name(name))
    {
        return Err("Build dependencies must be plain package names".into());
    }
    if let Some(repository) = &build.git {
        super::super::git::validate_repository(source, repository)?;
        if !recipe.asset.is_empty() || recipe.sha256.is_some() || recipe.checksum_asset.is_some() {
            return Err("Git builds use an approved commit, not an asset or SHA-256 field".into());
        }
    } else if build.revision.is_some() || build.submodules {
        return Err("revision and submodules require build.git".into());
    }
    if let Some(revision) = &build.revision {
        if recipe.tag.is_none()
            || revision.len() != 40
            || !revision.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(
                "An explicit Git revision needs a fixed tag and full 40-digit commit ID".into(),
            );
        }
    }
    if build.optimization.cpu == Cpu::Native
        && !matches!(std::env::consts::ARCH, "x86_64" | "x86" | "aarch64" | "arm")
    {
        return Err(
            "Native CPU flags are not supported on this architecture; choose cpu=system".into(),
        );
    }
    Ok(())
}

pub(in crate::sources) fn preview(plan: &Plan) {
    if let Some(build) = &plan.recipe.build {
        println!(":: Source build: {}/{}; CPU={:?}, level={}, LTO={}, extra headroom={} MiB (estimate, not reserved)",
            plan.alias, plan.name, build.optimization.cpu,
            build.optimization.level.map_or("system".into(), |v| v.to_string()),
            build.optimization.lto.map_or("system".into(), |v| v.to_string()), build.scratch_mib);
        if let Some(git) = &plan.git {
            println!(
                "   Git: {} @ {} (submodules: {})",
                git.repository, git.commit, git.submodules
            );
        }
        println!("   Build code executes as your user; only the completed package is installed with elevated pacman.");
        if !build.makedepends.is_empty() {
            println!(
                "   Required installed build dependencies: {}",
                build.makedepends.join(", ")
            );
        }
        for args in &build.commands {
            println!(
                "   Build: {}",
                args.iter()
                    .map(|arg| crate::utils::shell_quote(arg))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
    }
}
