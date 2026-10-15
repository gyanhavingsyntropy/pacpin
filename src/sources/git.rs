//! Resolve release tags before approval and fetch the exact approved commit.
use super::{build::BuildRecipe, Kind, Plan, Source};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Revision {
    pub repository: String,
    pub commit: String,
    pub submodules: bool,
}

fn commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(super) fn validate_repository(source: &Source, repository: &str) -> Result<(), String> {
    if source.kind == Kind::Local && Path::new(repository).is_absolute() {
        if std::fs::symlink_metadata(repository)
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        {
            return Ok(());
        }
        return Err("A local Git build needs an existing directory, not a symlink".into());
    }
    super::catalog::https_url(repository)?;
    Ok(())
}

fn command(local: bool) -> Command {
    let mut command = Command::new("git");
    // Never inherit repository selection, executable hooks, protocol helpers or
    // credentials from an unrelated caller's Git environment/configuration.
    for (key, _) in std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
    {
        command.env_remove(key);
    }
    command
        .env("HOME", "/nonexistent/pacpin-git")
        .env("XDG_CONFIG_HOME", "/nonexistent/pacpin-git")
        .env_remove("NETRC")
        .env_remove("CURL_HOME")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ALLOW_PROTOCOL", if local { "file" } else { "https" })
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "credential.helper=",
            "-c",
            "http.followRedirects=false",
        ]);
    command
}

fn captured(command: &mut Command) -> Result<String, String> {
    let output = crate::execution::capture_output_with_limit(
        command,
        32_768,
        std::time::Duration::from_secs(60),
    )
    .map_err(|e| format!("Cannot inspect Git source: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Git metadata failed: {}",
            crate::utils::sanitize_display_text(&String::from_utf8_lossy(&output.stderr))
        ));
    }
    String::from_utf8(output.stdout).map_err(|_| "Invalid Git metadata encoding".into())
}

fn select_commit(text: &str, tag: &str) -> Result<String, String> {
    let reference = format!("refs/tags/{tag}");
    let peeled_reference = format!("{reference}^{{}}");
    let mut direct = None;
    let mut peeled = None;
    for line in text.lines() {
        let (hash, name) = line.split_once('\t').ok_or("Malformed Git tag metadata")?;
        if !commit(hash) {
            return Err("Git tag has an invalid commit ID".into());
        }
        let target = if name == reference {
            &mut direct
        } else if name == peeled_reference {
            &mut peeled
        } else {
            return Err("Git returned an unexpected tag; refusing ambiguous source".into());
        };
        if target.replace(hash.to_lowercase()).is_some() {
            return Err("Duplicate Git tag metadata".into());
        }
    }
    peeled
        .or(direct)
        .ok_or_else(|| "Selected release has no corresponding Git tag".into())
}

pub(super) fn resolve(source: &Source, build: &BuildRecipe, tag: &str) -> Result<Revision, String> {
    let repository = build.git.as_ref().ok_or("Missing Git repository")?;
    validate_repository(source, repository)?;
    if !crate::is_command_available("git") {
        return Err("Git source builds require git".into());
    }
    let hash = if let Some(hash) = &build.revision {
        if !commit(hash) {
            return Err("revision must be a full 40-digit Git commit ID".into());
        }
        hash.to_lowercase()
    } else {
        let reference = format!("refs/tags/{tag}");
        select_commit(
            &captured(command(Path::new(repository).is_absolute()).args([
                "ls-remote",
                "--exit-code",
                repository,
                &reference,
                &format!("{reference}^{{}}"),
            ]))?,
            tag,
        )?
    };
    Ok(Revision {
        repository: repository.clone(),
        commit: hash,
        submodules: build.submodules,
    })
}

pub(super) fn fetch(plan: &Plan, revision: &Revision, work: &Path) -> Result<PathBuf, String> {
    if unsafe { libc::geteuid() } == 0 {
        return Err("Git source preparation must run unprivileged".into());
    }
    validate_repository(&plan.source, &revision.repository)?;
    if !commit(&revision.commit) {
        return Err("Invalid approved Git commit".into());
    }
    super::build::check_capacity(plan, work)?;
    let checkout = work.join("git-source");
    let local = Path::new(&revision.repository).is_absolute();
    let run = |args: &[&str]| -> Result<(), String> {
        let status = crate::progress::run(
            command(local).current_dir(work).args(args),
            crate::progress::Manager::Sources,
            "Fetching approved Git commit",
            || {},
        )
        .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("Git source preparation failed ({status})"))
        }
    };
    run(&[
        "init",
        "--quiet",
        checkout.to_str().ok_or("Invalid workspace path")?,
    ])?;
    let directory = checkout.to_str().ok_or("Invalid workspace path")?;
    run(&[
        "-C",
        directory,
        "fetch",
        "--depth=1",
        "--no-tags",
        "--",
        &revision.repository,
        &revision.commit,
    ])?;
    run(&[
        "-C",
        directory,
        "checkout",
        "--quiet",
        "--detach",
        &revision.commit,
    ])?;
    let head = captured(command(local).args(["-C", directory, "rev-parse", "HEAD"]))?;
    if head.trim() != revision.commit {
        return Err("Git checkout differs from the approved commit".into());
    }
    if revision.submodules {
        // Submodules can only use HTTPS, even for a local catalog fixture.
        let status = crate::progress::run(
            command(false).args([
                "-C",
                directory,
                "submodule",
                "update",
                "--init",
                "--recursive",
                "--depth=1",
            ]),
            crate::progress::Manager::Sources,
            "Fetching pinned Git submodules",
            || {},
        )
        .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("Pinned HTTPS Git submodule fetch failed".into());
        }
    }
    Ok(checkout)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn annotated_tags_choose_the_commit_and_reject_ambiguity() {
        let object = "a".repeat(40);
        let hash = "b".repeat(40);
        assert_eq!(
            select_commit(
                &format!("{object}\trefs/tags/v1\n{hash}\trefs/tags/v1^{{}}\n"),
                "v1"
            )
            .unwrap(),
            hash
        );
        assert!(select_commit(&format!("{object}\trefs/tags/other\n"), "v1").is_err());
        assert!(select_commit(
            &format!("{object}\trefs/tags/v1\n{object}\trefs/tags/v1\n"),
            "v1"
        )
        .is_err());
        assert!(select_commit("123\trefs/tags/v1\n", "v1").is_err());
    }
}
